//! Provider-neutral same-world process-confinement capability seam.

use std::{ops::Deref, path::PathBuf, sync::Arc};

use seekdeep_cordis::{Context, ServiceKey, fiber::EffectHandle};
use seekdeep_core::session::SessionId;
use seekdeep_llm::HarnessError;
use serde::{Deserialize, Serialize};

pub mod escalation;
pub mod invariant;
pub mod roots;

pub use escalation::{
    ESCALATION_TARGETS, EscalationApproval, EscalationApprover, EscalationAsk, EscalationOutcome,
    EscalationRequest, WIDER_MODES, approve_escalation, escalation_hint_marker,
    sandbox_denial_marker, validate_escalation_args,
};
pub use roots::{canonical_path, writable_roots};

/// Error code for a requested confined mode when no backend is usable.
pub const SANDBOX_UNAVAILABLE: &str = "SANDBOX_UNAVAILABLE";

/// Every closed file-effect mode, in source advertisement order.
pub const SANDBOX_MODES: [SandboxMode; 3] = [
    SandboxMode::ReadOnly,
    SandboxMode::WorkspaceWrite,
    SandboxMode::DangerFullAccess,
];

/// File-effect policy for one execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    /// Permit reads but only required write sinks.
    ReadOnly,
    /// Permit writes beneath the workspace and platform temp areas.
    WorkspaceWrite,
    /// Bypass confinement.
    DangerFullAccess,
}

impl SandboxMode {
    /// Exact wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }

    /// Parses an untrusted wire spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "read-only" => Some(Self::ReadOnly),
            "workspace-write" => Some(Self::WorkspaceWrite),
            "danger-full-access" => Some(Self::DangerFullAccess),
            _ => None,
        }
    }
}

impl std::fmt::Display for SandboxMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A mode that must be enforced by a sandbox backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfinedSandboxMode {
    /// Permit reads but no ordinary writes.
    ReadOnly,
    /// Permit writes beneath the workspace and platform temp areas.
    WorkspaceWrite,
}

impl ConfinedSandboxMode {
    /// Exact wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
        }
    }
}

impl From<ConfinedSandboxMode> for SandboxMode {
    fn from(value: ConfinedSandboxMode) -> Self {
        match value {
            ConfinedSandboxMode::ReadOnly => Self::ReadOnly,
            ConfinedSandboxMode::WorkspaceWrite => Self::WorkspaceWrite,
        }
    }
}

impl TryFrom<SandboxMode> for ConfinedSandboxMode {
    type Error = DangerFullAccessIsNotConfined;

    fn try_from(value: SandboxMode) -> Result<Self, Self::Error> {
        match value {
            SandboxMode::ReadOnly => Ok(Self::ReadOnly),
            SandboxMode::WorkspaceWrite => Ok(Self::WorkspaceWrite),
            SandboxMode::DangerFullAccess => Err(DangerFullAccessIsNotConfined),
        }
    }
}

/// Conversion error for trying to hand bypass mode to an enforcing provider.
#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
#[error("danger-full-access is not a confined sandbox mode")]
pub struct DangerFullAccessIsNotConfined;

/// Complete file-effect policy resolved for one capability call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxExecutionPolicy {
    /// Selected file-effect mode.
    pub mode: SandboxMode,
    /// Absolute root `workspace-write` may modify beneath.
    pub workspace_root: PathBuf,
    /// Calling session identity, absent for agentless calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

/// Complete policy accepted by a confining provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxPolicy {
    /// Selected enforcing mode.
    pub mode: ConfinedSandboxMode,
    /// Absolute root `workspace-write` may modify beneath.
    pub workspace_root: PathBuf,
    /// Calling session identity, absent for agentless calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

impl TryFrom<SandboxExecutionPolicy> for SandboxPolicy {
    type Error = DangerFullAccessIsNotConfined;

    fn try_from(value: SandboxExecutionPolicy) -> Result<Self, Self::Error> {
        Ok(Self {
            mode: value.mode.try_into()?,
            workspace_root: value.workspace_root,
            session_id: value.session_id,
        })
    }
}

/// Completeness of the selected confinement backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxEnforcement {
    /// Every promised file effect is governed.
    Full,
    /// This host cannot govern every promised file effect.
    Partial,
}

/// Evidence that identifies a runner failing before it executes the command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunnerFailureRule {
    /// Nonzero statuses on which this rule may match; absent admits any nonzero status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_exit_codes: Option<Vec<i32>>,
    /// Nonempty case-insensitive fatal substrings.
    pub fatal_signatures: Vec<String>,
    /// Benign lines excluded by case-insensitive full-line equality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub informational_lines: Option<Vec<String>>,
}

/// Exact argv wrapper and backend-specific failure dialect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfinedArgv {
    /// Runner argv replacing the caller's own argv.
    pub argv: Vec<String>,
    /// Completeness achieved by the selected backend.
    pub enforcement: SandboxEnforcement,
    /// Backend-specific denial substrings.
    pub denial_signatures: Vec<String>,
    /// Structured pre-execution runner failure evidence.
    pub runner_failure_rules: Vec<RunnerFailureRule>,
}

/// Fail-closed structured error for unavailable confinement.
#[derive(Debug, thiserror::Error)]
#[error("{inner}")]
pub struct SandboxUnavailableError {
    inner: HarnessError,
}

impl SandboxUnavailableError {
    /// Constructs the exact operator-facing failure, with optional runner detail.
    #[must_use]
    pub fn new(mode: ConfinedSandboxMode, detail: Option<&str>) -> Self {
        let mut message = format!(
            "sandbox mode \"{}\" is requested but no sandbox backend is usable on this host; refusing to run the command unconfined. Install bubblewrap or run a Landlock-enforcing kernel (Linux), ensure sandbox-exec is usable (macOS), or ensure the ACL restricted-token runner can start (Windows) — otherwise switch the consumer to danger-full-access.",
            mode.as_str()
        );
        if let Some(detail) = detail {
            message.push_str(" Runner failure: ");
            message.push_str(detail);
        }
        Self {
            inner: HarnessError::named("SandboxUnavailableError", message, SANDBOX_UNAVAILABLE),
        }
    }

    /// Stable JavaScript error-class name.
    #[must_use]
    pub fn name(&self) -> &str {
        self.inner.name()
    }

    /// Stable machine route.
    #[must_use]
    pub fn code(&self) -> &str {
        self.inner.code()
    }

    /// Human-readable message.
    #[must_use]
    pub fn message(&self) -> &str {
        self.inner.message()
    }
}

/// Exact same-world process confinement provider.
pub trait SandboxProvider: std::fmt::Debug + Send + Sync {
    /// Wraps the exact argv or fails closed.
    ///
    /// # Errors
    ///
    /// Returns an enforcing-backend or configuration failure. Silent unconfined
    /// passthrough is forbidden.
    fn confine(&self, argv: &[String], policy: &SandboxPolicy) -> anyhow::Result<ConfinedArgv>;
}

/// Dynamically dispatched provider occupying `ctx.sandbox`.
#[derive(Clone)]
pub struct SandboxService(Arc<dyn SandboxProvider>);

impl std::fmt::Debug for SandboxService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("SandboxService")
            .field(&self.0)
            .finish()
    }
}

impl SandboxService {
    /// Wraps one concrete provider.
    #[must_use]
    pub fn new(provider: Arc<dyn SandboxProvider>) -> Arc<Self> {
        Arc::new(Self(provider))
    }

    /// Publishes this exact provider in a Cordis context.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> anyhow::Result<EffectHandle> {
        Ok(context.provide(SANDBOX, self.clone())?)
    }
}

impl Deref for SandboxService {
    type Target = dyn SandboxProvider;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

/// Typed Cordis seat corresponding to `ctx.sandbox`.
pub const SANDBOX: ServiceKey<SandboxService> = ServiceKey::new("sandbox");
