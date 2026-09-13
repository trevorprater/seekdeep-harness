//! Filesystem service definition and its vocabulary.

pub mod invariant;
pub mod types;

use std::sync::Arc;

use futures::stream::BoxStream;
use seekdeep_cordis::{Context, ServiceKey, fiber::EffectHandle};
use seekdeep_llm::AbortSignal;
use seekdeep_sandbox::{SandboxExecutionPolicy, SandboxMode};

pub use types::{
    FsDirEntry, FsEditOutcome, FsEditRequest, FsError, FsErrorCode, FsInfo, FsKind, FsObservation,
    FsPathInfo, FsPathKind, FsTarget, FsTargetKey, FsVersion, FsWriteIntent, FsWriteOperation,
    FsWriteOutcome,
};

/// Typed Cordis seat corresponding to `ctx.fs`.
pub const FS: ServiceKey<FileSystemService> = ServiceKey::new("fs");

/// Object-safe filesystem provider published through Cordis.
#[derive(Clone)]
pub struct FileSystemService(Arc<dyn FileSystem>);

impl std::fmt::Debug for FileSystemService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("FileSystemService")
            .field(&"dyn FileSystem")
            .finish()
    }
}

impl FileSystemService {
    /// Wraps one concrete filesystem provider.
    #[must_use]
    pub fn new(filesystem: Arc<dyn FileSystem>) -> Arc<Self> {
        Arc::new(Self(filesystem))
    }

    /// Returns the object-safe filesystem provider.
    #[must_use]
    pub fn filesystem(&self) -> Arc<dyn FileSystem> {
        self.0.clone()
    }

    /// Publishes this provider on the source-compatible Cordis seat.
    ///
    /// # Errors
    ///
    /// Returns inactive-fiber or duplicate-service failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> anyhow::Result<EffectHandle> {
        Ok(context.provide(FS, self.clone())?)
    }
}

/// Abstract filesystem provider for one execution world.
#[async_trait::async_trait]
pub trait FileSystem: Send + Sync + 'static {
    /// The sandbox mode this backend enforces by default, or none.
    #[must_use]
    fn sandbox_mode(&self) -> Option<SandboxMode> {
        None
    }

    /// Resolves a model/plugin-supplied path into a stable target.
    async fn resolve(
        &self,
        path: &str,
        cwd: Option<&str>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<FsTarget>;

    /// Returns the canonical absolute path a subprocess can open.
    #[must_use]
    fn process_path(&self, target: &FsTarget) -> String;

    /// Returns the canonical file URI for a target.
    #[must_use]
    fn file_url(&self, target: &FsTarget) -> String;

    /// Tests canonical containment.
    #[must_use]
    fn contains(&self, parent: &FsTarget, child: &FsTarget) -> bool;

    /// Returns target metadata, or none when absent.
    async fn stat(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<FsInfo>>;

    /// Returns path metadata without following a trailing symlink.
    async fn lstat(
        &self,
        path: &str,
        cwd: Option<&str>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<FsPathInfo>>;

    /// Reads the whole regular text file.
    async fn read_text(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<String>;

    /// Streams the whole regular text file as decoded chunks.
    async fn stream_text(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<String>>>;

    /// Reads the whole regular file as raw bytes.
    async fn read_bytes(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
        max_bytes: usize,
    ) -> anyhow::Result<Vec<u8>>;

    /// Lists direct children of a directory in stable name order.
    async fn list_dir(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Vec<FsDirEntry>>;

    /// Atomically creates or replaces UTF-8 text.
    async fn write_text(
        &self,
        target: &FsTarget,
        content: &str,
        expected: Option<&FsWriteIntent>,
        signal: Option<&AbortSignal>,
        sandbox_policy: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Result<FsWriteOutcome>;

    /// Atomically edits literal text.
    async fn edit_text(
        &self,
        target: &FsTarget,
        edit: &FsEditRequest,
        expected: Option<&FsVersion>,
        signal: Option<&AbortSignal>,
        sandbox_policy: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Result<FsEditOutcome>;
}
