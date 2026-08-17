//! Host directory-picker capability seam.
//!
//! Backends differ in interaction shape rather than only mechanism: a native
//! backend opens a chooser on the Host display, while a browse backend exposes
//! remote-safe listing and creation primitives. The service retains one stable
//! capability value for its complete Cordis lifetime.

use std::{fmt, sync::Arc};

use futures::future::BoxFuture;
use seekdeep_cordis::{Context, CordisError, ServiceKey, fiber::EffectHandle};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::AbortSignal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Typed Cordis slot corresponding to `ctx.directoryPicker`.
pub const DIRECTORY_PICKER: ServiceKey<DirectoryPickerService> = ServiceKey::new("directoryPicker");

/// Stable invariant companion name.
pub const INVARIANT_NAME: &str = "host-directory-picker-invariant";
const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-host-directory-picker";

/// One directory row: a listing child or breadcrumb ancestor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryEntry {
    /// Base name shown in a browser row.
    pub name: String,
    /// Absolute Host path.
    pub path: String,
    /// Host-platform hidden marker.
    pub hidden: bool,
}

/// One directory level plus its ancestry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryListing {
    /// Absolute path of the listed directory.
    pub path: String,
    /// Host account home directory.
    pub home: String,
    /// Filesystem-root-to-target ancestor chain.
    pub crumbs: Vec<DirectoryEntry>,
    /// Direct child directories in backend-defined stable order.
    pub entries: Vec<DirectoryEntry>,
    /// Whether the backend cut the name-sorted tail at its result bound.
    pub truncated: bool,
}

/// Closed browse-primitive failure vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectoryPickerErrorCode {
    /// The target is not fully qualified or cannot be listed.
    DirectoryUnreadable,
    /// The requested child already exists.
    DirectoryExists,
    /// The child could not be created.
    DirectoryCreateFailed,
}

impl DirectoryPickerErrorCode {
    /// Exact business-code wire literal.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DirectoryUnreadable => "directory-unreadable",
            Self::DirectoryExists => "directory-exists",
            Self::DirectoryCreateFailed => "directory-create-failed",
        }
    }
}

impl fmt::Display for DirectoryPickerErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Typed browse failure carrying its business code and subject path.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct DirectoryPickerError {
    /// Closed business code.
    pub code: DirectoryPickerErrorCode,
    /// Absolute path the failure concerns.
    pub path: String,
    /// Operator-facing description.
    pub message: String,
}

impl DirectoryPickerError {
    /// Creates one typed browse failure.
    #[must_use]
    pub fn new(
        code: DirectoryPickerErrorCode,
        path: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            path: path.into(),
            message: message.into(),
        }
    }
}

/// Native chooser callback.
pub type NativeDirectoryPicker =
    Arc<dyn Fn(AbortSignal) -> BoxFuture<'static, anyhow::Result<Option<String>>> + Send + Sync>;

/// Browse-level listing callback.
pub type DirectoryLister = Arc<
    dyn Fn(
            Option<String>,
            AbortSignal,
        ) -> BoxFuture<'static, Result<DirectoryListing, DirectoryPickerFailure>>
        + Send
        + Sync,
>;

/// Browse child-creation callback.
pub type DirectoryCreator = Arc<
    dyn Fn(String, String) -> BoxFuture<'static, Result<String, DirectoryPickerFailure>>
        + Send
        + Sync,
>;

/// Failure from a picker backend.
#[derive(Debug, Error)]
pub enum DirectoryPickerFailure {
    /// Typed browse business failure.
    #[error(transparent)]
    Picker(#[from] DirectoryPickerError),
    /// An unclassified backend failure.
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

/// Stable interaction capability exposed by one picker backend.
#[derive(Clone)]
pub enum DirectoryPickerCapability {
    /// One native OS chooser on the Host display.
    Native {
        /// Opens the chooser and resolves a path or explicit user cancellation.
        pick: NativeDirectoryPicker,
    },
    /// Remote-safe listing and creation primitives.
    Browse {
        /// Lists one fully qualified level, or the Host home when absent.
        list: DirectoryLister,
        /// Creates one child under an existing fully qualified parent.
        create_directory: DirectoryCreator,
    },
    /// A declaration-merged source capability not understood by this consumer.
    Unknown {
        /// Preserved capability discriminant.
        kind: String,
    },
}

impl DirectoryPickerCapability {
    /// Capability discriminant consumed by API affordance and error logic.
    #[must_use]
    pub fn kind(&self) -> &str {
        match self {
            Self::Native { .. } => "native",
            Self::Browse { .. } => "browse",
            Self::Unknown { kind } => kind,
        }
    }
}

impl fmt::Debug for DirectoryPickerCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DirectoryPickerCapability")
            .field("kind", &self.kind())
            .finish_non_exhaustive()
    }
}

/// Cordis service retaining one stable capability object for its lifetime.
#[derive(Debug)]
pub struct DirectoryPickerService {
    capability: DirectoryPickerCapability,
}

impl DirectoryPickerService {
    /// Creates a service around one stable backend capability.
    #[must_use]
    pub fn new(capability: DirectoryPickerCapability) -> Arc<Self> {
        Arc::new(Self { capability })
    }

    /// Returns the stable capability object.
    #[must_use]
    pub fn capability(&self) -> &DirectoryPickerCapability {
        &self.capability
    }

    /// Provides this service until its owning Cordis fiber leaves.
    ///
    /// # Errors
    ///
    /// Returns ordinary inactive-fiber or duplicate-service failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> Result<EffectHandle, CordisError> {
        context.provide(DIRECTORY_PICKER, self.clone())
    }
}

/// Registers the package's explained-empty invariant companion.
///
/// The seam is stateless; concrete backends and RPC consumers own runtime
/// observations.
///
/// # Errors
///
/// Returns ordinary invariant registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_error_carries_business_code_path_and_message() {
        let failure = DirectoryPickerError::new(
            DirectoryPickerErrorCode::DirectoryExists,
            "/home/u/x",
            "/home/u/x already exists",
        );
        assert_eq!(failure.code.as_str(), "directory-exists");
        assert_eq!(failure.path, "/home/u/x");
        assert!(failure.to_string().contains("already exists"));
    }

    #[tokio::test]
    async fn service_is_stable_and_leaves_with_owning_fiber() {
        let context = Context::new();
        let fiber = seekdeep_cordis::Fiber::active_child("picker");
        let child = context.with_fiber(fiber.clone());
        let picker = DirectoryPickerService::new(DirectoryPickerCapability::Native {
            pick: Arc::new(|_| Box::pin(async { Ok(None) })),
        });
        picker.provide(&child).expect("provide picker");
        let first = context.get(DIRECTORY_PICKER).expect("picker visible");
        let second = context.get(DIRECTORY_PICKER).expect("picker stable");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(first.capability().kind(), "native");
        fiber.dispose().await.expect("dispose picker fiber");
        assert!(context.get(DIRECTORY_PICKER).is_none());
    }

    #[test]
    fn unknown_capability_kind_is_preserved() {
        let capability = DirectoryPickerCapability::Unknown {
            kind: "remote-volume".to_owned(),
        };
        assert_eq!(capability.kind(), "remote-volume");
    }
}
