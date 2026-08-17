//! Provider-neutral managed subprocess and terminal capability seam.

use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    ops::Deref,
    sync::Arc,
};

use async_trait::async_trait;
use seekdeep_cordis::{Context, CordisError, ServiceKey, fiber::EffectHandle};
use seekdeep_llm::AbortSignal;

/// Explained-empty invariant companion.
pub mod invariant;
/// Fully specified request and handle vocabulary.
pub mod types;

pub use types::{
    CollectedOutput, ProcessGroupId, ProcessId, ProcessSignal, SEEKDEEP_ENV_PREFIX,
    SeekDeepEnvironment, SeekDeepEnvironmentKey, SubprocessCollect, SubprocessCollectedOutputs,
    SubprocessEnvironment, SubprocessHandle, SubprocessHandleRef, SubprocessInput,
    SubprocessLookupEnvironment, SubprocessOutcome, SubprocessOutput, SubprocessOutputMode,
    SubprocessOutputRead, SubprocessOutputReader, SubprocessOutputReaderHandle,
    SubprocessSpawnSpec, SubprocessSpill, SubprocessStdinMode, SubprocessStdio,
    SubprocessTerminalForeground, SubprocessTerminalHandle, SubprocessTerminalHandleRef,
    SubprocessTerminalSignal, SubprocessTerminalSpawnSpec,
};

/// Typed Cordis seat corresponding to `ctx.subprocess`.
pub const SUBPROCESS: ServiceKey<SubprocessService> = ServiceKey::new("subprocess");

/// Credential-shaped environment-name fragments scrubbed case-insensitively.
pub const SENSITIVE_ENV_FRAGMENTS: &[&str] = &["KEY", "PASSWORD", "SECRET", "TOKEN"];

/// Returns a fresh ambient environment without credential-shaped or managed keys.
#[must_use]
pub fn scrubbed_parent_env() -> BTreeMap<OsString, OsString> {
    scrub_environment(std::env::vars_os())
}

/// Deterministic scrub over a supplied environment snapshot.
#[doc(hidden)]
pub fn scrub_environment(
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> BTreeMap<OsString, OsString> {
    environment
        .into_iter()
        .filter(|(key, _)| safe_ambient_key(key))
        .collect()
}

fn safe_ambient_key(key: &OsStr) -> bool {
    let uppercase = key.to_string_lossy().to_ascii_uppercase();
    !uppercase.starts_with(SEEKDEEP_ENV_PREFIX)
        && !SENSITIVE_ENV_FRAGMENTS
            .iter()
            .any(|fragment| uppercase.contains(fragment))
}

/// Concrete-provider contract for executable lookup and owned process trees.
#[async_trait]
pub trait SubprocessRuntime: std::fmt::Debug + Send + Sync {
    /// Resolves an absolute executable or bare PATH name in this execution world.
    async fn resolve_executable(
        &self,
        command: &str,
        env: Option<&SubprocessLookupEnvironment>,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<String>;

    /// Starts one fully specified ordinary process and returns immediately.
    ///
    /// # Errors
    ///
    /// Returns synchronous request-validation or provider-initialization failures.
    /// Operating-system spawn failures remain represented by a pid `-1` handle
    /// whose [`SubprocessHandle::done`](types::SubprocessHandle::done) rejects.
    fn spawn(&self, spec: SubprocessSpawnSpec) -> anyhow::Result<SubprocessHandleRef>;

    /// Allocates one fully specified terminal process session.
    async fn spawn_terminal(
        &self,
        spec: SubprocessTerminalSpawnSpec,
    ) -> anyhow::Result<SubprocessTerminalHandleRef>;
}

/// Dynamically dispatched provider occupying the subprocess service seat.
#[derive(Clone)]
pub struct SubprocessService(Arc<dyn SubprocessRuntime>);

impl std::fmt::Debug for SubprocessService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("SubprocessService")
            .field(&self.0)
            .finish()
    }
}

impl SubprocessService {
    /// Wraps one concrete provider.
    #[must_use]
    pub fn new(runtime: Arc<dyn SubprocessRuntime>) -> Arc<Self> {
        Arc::new(Self(runtime))
    }

    /// Publishes this provider in the exact context.
    ///
    /// # Errors
    ///
    /// Returns source-compatible duplicate-service or inactive-owner failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> anyhow::Result<EffectHandle> {
        match context.provide(SUBPROCESS, self.clone()) {
            Ok(effect) => Ok(effect),
            Err(CordisError::DuplicateService(_)) => {
                anyhow::bail!("service \"subprocess\" has been registered")
            }
            Err(error) => Err(error.into()),
        }
    }
}

impl Deref for SubprocessService {
    type Target = dyn SubprocessRuntime;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}
