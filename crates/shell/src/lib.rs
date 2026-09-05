//! Provider-neutral foreground shell and background-process capability seam.

use std::{ops::Deref, sync::Arc};

use async_trait::async_trait;
use seekdeep_cordis::{Context, CordisError, ServiceKey, fiber::EffectHandle};
use seekdeep_settings::SettingsNamespace;

pub mod invariant;
pub mod render;
pub mod types;

pub use render::{ParsedExitStatus, parse_exit_status};
pub use types::{
    CollectedOutput, ProcessSignal, SEEKDEEP_ENV_PREFIX, SandboxEnforcement,
    SandboxExecutionPolicy, SandboxMode, SeekDeepEnvironment, SeekDeepEnvironmentKey,
    ShellExecRequest, ShellExecSpec, ShellProcess, ShellProcessHandle, ShellProcessRead,
    ShellProcessStatus, ShellRunResult, ShellSandboxInfo,
};

/// Typed Cordis seat corresponding to `ctx.shell`.
pub const SHELL: ServiceKey<ShellService> = ServiceKey::new("shell");

/// Returns the shared settings namespace owned by this capability.
#[must_use]
pub fn shell_settings_namespace() -> SettingsNamespace {
    SettingsNamespace::new("shell")
}

/// Provider contract for resolved foreground and background shell execution.
#[async_trait]
pub trait ShellExecutor: std::fmt::Debug + Send + Sync {
    /// Default sandbox mode, absent for an unsandboxed provider.
    fn sandbox_mode(&self) -> Option<SandboxMode> {
        None
    }

    /// Applies provider defaults and caps to one caller request.
    ///
    /// # Errors
    ///
    /// Returns provider-specific request validation failures.
    fn resolve(&self, request: ShellExecRequest) -> anyhow::Result<ShellExecSpec>;

    /// Runs a foreground command to completion.
    ///
    /// # Errors
    ///
    /// Rejects only infrastructure failures. Command exits, timeout kills, and
    /// abort kills resolve as [`ShellRunResult`].
    async fn run(&self, spec: ShellExecSpec) -> anyhow::Result<ShellRunResult>;

    /// Starts a task-free background process immediately.
    ///
    /// # Errors
    ///
    /// Returns infrastructure failures that prevent handle construction.
    fn start(&self, spec: ShellExecSpec) -> anyhow::Result<ShellProcessHandle>;
}

/// Dynamically dispatched exact provider occupying the shell service seat.
#[derive(Clone)]
pub struct ShellService(Arc<dyn ShellExecutor>);

impl std::fmt::Debug for ShellService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("ShellService")
            .field(&self.0)
            .finish()
    }
}

impl ShellService {
    /// Wraps one concrete executor.
    #[must_use]
    pub fn new(executor: Arc<dyn ShellExecutor>) -> Arc<Self> {
        Arc::new(Self(executor))
    }

    /// Publishes this implementation in the exact context.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> anyhow::Result<EffectHandle> {
        match context.provide(SHELL, self.clone()) {
            Ok(effect) => Ok(effect),
            Err(CordisError::DuplicateService(_)) => {
                anyhow::bail!("service \"shell\" has been registered")
            }
            Err(error) => Err(error.into()),
        }
    }
}

impl Deref for ShellService {
    type Target = dyn ShellExecutor;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}
