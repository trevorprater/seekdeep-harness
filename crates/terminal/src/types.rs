//! Types shared by PTY backends, the owner-scoped registry, and tool consumers.

use std::{error::Error, fmt, sync::Arc};

use async_trait::async_trait;
use futures::future::BoxFuture;
use seekdeep_agent::Agent;
use seekdeep_llm::AbortSignal;
use seekdeep_subprocess::{ProcessGroupId, ProcessId, ProcessSignal};
use serde::{Deserialize, Serialize};

/// Cloneable error identity used by repeatably awaited terminal operations.
#[derive(Clone)]
pub struct TerminalFailure(Arc<dyn Error + Send + Sync>);

impl TerminalFailure {
    /// Retains one typed failure behind a cloneable identity.
    pub fn new(error: impl Error + Send + Sync + 'static) -> Self {
        Self(Arc::new(error))
    }

    /// Retains an already shared typed failure without changing its identity.
    pub fn from_arc<T>(error: Arc<T>) -> Self
    where
        T: Error + Send + Sync + 'static,
    {
        Self(error)
    }

    /// Retains the concrete error carried by an `anyhow` boundary.
    ///
    /// The conversion deliberately discards an `anyhow`-captured backtrace so
    /// the original concrete type remains downcastable across the terminal
    /// service seam. This mirrors JavaScript propagation of the thrown error
    /// object instead of flattening it to its display string.
    #[must_use]
    pub fn from_anyhow(error: anyhow::Error) -> Self {
        Self(Arc::from(
            error.reallocate_into_boxed_dyn_error_without_backtrace(),
        ))
    }

    /// Creates an ordinary message-only failure.
    pub fn message(message: impl Into<String>) -> Self {
        Self::new(TerminalMessageError(message.into()))
    }

    /// Borrows a concrete typed failure when its identity is known.
    pub fn downcast_ref<T>(&self) -> Option<&T>
    where
        T: Error + Send + Sync + 'static,
    {
        self.0.downcast_ref::<T>()
    }

    /// Exposes the retained standard error.
    pub fn as_error(&self) -> &(dyn Error + Send + Sync + 'static) {
        self.0.as_ref()
    }

    /// Tests whether two failures retain the exact same error allocation.
    #[must_use]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl fmt::Debug for TerminalFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, formatter)
    }
}

impl fmt::Display for TerminalFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl Error for TerminalFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.0.source()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct TerminalMessageError(String);

/// Result returned across the provider-neutral terminal seam.
pub type TerminalResult<T> = Result<T, TerminalFailure>;

/// Multiple lifecycle failures retained in source order.
#[derive(Clone, Debug)]
pub struct TerminalAggregateError {
    message: Arc<str>,
    errors: Arc<[TerminalFailure]>,
}

impl TerminalAggregateError {
    /// Creates one aggregate with its source-compatible summary.
    #[must_use]
    pub fn new(message: impl Into<Arc<str>>, errors: Vec<TerminalFailure>) -> Self {
        Self {
            message: message.into(),
            errors: errors.into(),
        }
    }

    /// Ordered constituent failures.
    #[must_use]
    pub fn errors(&self) -> &[TerminalFailure] {
        &self.errors
    }
}

impl fmt::Display for TerminalAggregateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for TerminalAggregateError {}

/// Backend-reported failure to clean partial unpublished resources.
#[derive(Clone, Debug)]
pub struct TerminalBackendCleanupError {
    /// Original setup or cancellation failure.
    pub spawn_error: TerminalFailure,
    /// Failure that may leave backend-owned resources alive.
    pub cleanup_error: TerminalFailure,
}

impl TerminalBackendCleanupError {
    /// Retains the original startup and cleanup failures.
    #[must_use]
    pub fn new(spawn_error: TerminalFailure, cleanup_error: TerminalFailure) -> Self {
        Self {
            spawn_error,
            cleanup_error,
        }
    }
}

impl fmt::Display for TerminalBackendCleanupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PTY backend startup and cleanup both failed")
    }
}

impl Error for TerminalBackendCleanupError {}

/// Opaque identity minted for one live PTY session.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TerminalSessionId(String);

impl TerminalSessionId {
    /// Wraps one registry-issued identity for compatibility bindings.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrowed wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TerminalSessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Why one interactive send returned control to its caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalWaitReason {
    /// The foreground program began reading standard input.
    StdinRead,
    /// Output became quiescent long enough to infer readiness.
    InferredIdle,
    /// The configured wait elapsed.
    Timeout,
    /// The top-level terminal session exited.
    SessionExit,
}

/// Signals the model-facing PTY surface permits for foreground process groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TerminalSignal {
    /// Interrupt.
    SIGINT,
    /// Termination request.
    SIGTERM,
    /// Uncatchable termination.
    SIGKILL,
    /// Terminal stop.
    SIGTSTP,
    /// Hangup.
    SIGHUP,
}

/// Top-level PTY process status, independent of a send wait reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TerminalSessionStatus {
    /// The top-level process remains live.
    Running,
    /// The top-level process has exited.
    Exited {
        /// Numeric exit code when the process exited normally.
        #[serde(rename = "exitCode")]
        exit_code: Option<i32>,
        /// Extensible operating-system signal when signal-terminated.
        signal: Option<ProcessSignal>,
    },
}

/// Request to create one owner-scoped PTY session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalSpawnRequest {
    /// Registered backend type.
    #[serde(rename = "type")]
    pub terminal_type: String,
    /// Optional owner-local display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Optional initial working directory interpreted by the backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Fully identified request handed from the registry to a backend.
#[derive(Clone, Debug)]
pub struct TerminalBackendSpawnSpec {
    /// Registry-minted session identity.
    pub session_id: TerminalSessionId,
    /// Exact live owner for authority-aware backend setup.
    pub owner: Arc<Agent>,
    /// Registered backend type.
    pub terminal_type: String,
    /// Optional owner-local display name.
    pub name: Option<String>,
    /// Optional backend-interpreted working directory.
    pub cwd: Option<String>,
    /// Cancellation of unpublished backend setup.
    pub signal: Option<AbortSignal>,
}

/// Input for one line-oriented terminal interaction.
#[derive(Clone, Debug)]
pub struct TerminalSendRequest {
    /// UTF-8 text to write.
    pub text: String,
    /// Whether to write the backend's Enter sequence after the text.
    pub submit: bool,
    /// Cancellation for the wait and foreground command.
    pub signal: Option<AbortSignal>,
}

/// Incremental output consumed from one live send operation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalSendRead {
    /// Output produced since the previous operation read.
    pub delta: String,
    /// Whether unread operation output was dropped by the backend's bound.
    pub truncated: bool,
}

/// Settled result for one foreground or background send.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalSendResult {
    /// Bounded rendered terminal delta remaining at settlement.
    pub viewport: String,
    /// Why the wait returned.
    pub wait_reason: TerminalWaitReason,
    /// Top-level session status observed at settlement.
    pub session_status: TerminalSessionStatus,
    /// Whether output was dropped from the operation or retained scrollback.
    pub truncated: bool,
}

/// Live backend-owned send; exactly one may be active per PTY session.
pub trait TerminalSendOperation: fmt::Debug + Send + Sync {
    /// Repeatably awaits the one settled result.
    fn done(&self) -> BoxFuture<'static, TerminalResult<TerminalSendResult>>;
    /// Consumes output produced since the prior call.
    fn read_output(&self) -> TerminalSendRead;
    /// Requests `SIGINT`; returns false after settlement.
    fn cancel(&self) -> bool;
}

/// Shared terminal send handle.
pub type TerminalSendOperationRef = Arc<dyn TerminalSendOperation>;

/// Request for one backward scrollback page.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalReadRequest {
    /// Offset from the newest retained line.
    pub offset: Option<f64>,
    /// Requested line count.
    pub count: Option<f64>,
}

/// Bounded scrollback page.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalReadResult {
    /// Retained text in chronological order.
    pub text: String,
    /// Number of lines currently retained.
    pub total_lines: u64,
    /// Inclusive newest-relative offset of the first returned line.
    pub line_begin: u64,
    /// Exclusive newest-relative offset after the returned page.
    pub line_end: u64,
    /// Whether older output or the requested result exceeded a bound.
    pub truncated: bool,
}

/// Result of delivering a signal to a verified foreground process group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalSignalResult {
    /// True only after the backend delivered the signal.
    #[serde(deserialize_with = "deserialize_delivered")]
    delivered: bool,
    /// Process group that received the signal.
    pub target_pgid: ProcessGroupId,
}

impl TerminalSignalResult {
    /// Constructs the only successful result after signal delivery.
    #[must_use]
    pub const fn delivered(target_pgid: ProcessGroupId) -> Self {
        Self {
            delivered: true,
            target_pgid,
        }
    }

    /// Literal source-compatible success marker.
    #[must_use]
    pub const fn is_delivered(&self) -> bool {
        self.delivered
    }
}

fn deserialize_delivered<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let delivered = bool::deserialize(deserializer)?;
    if delivered {
        Ok(true)
    } else {
        Err(serde::de::Error::custom(
            "terminal signal result must report delivered: true",
        ))
    }
}

/// Owner-visible summary of one published PTY session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSessionSnapshot {
    /// Registry-minted identity used by every operation.
    pub session_id: TerminalSessionId,
    /// Optional owner-local display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Backend type that created the session.
    #[serde(rename = "type")]
    pub terminal_type: String,
    /// Top-level process id when the backend has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<ProcessId>,
    /// Current top-level process status.
    pub status: TerminalSessionStatus,
}

/// Successful publication returned by the terminal registry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSpawnResult {
    /// Registry-minted identity used by every operation.
    pub session_id: TerminalSessionId,
    /// Optional owner-local display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Backend type that created the session.
    #[serde(rename = "type")]
    pub terminal_type: String,
    /// Top-level process id when the backend has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<ProcessId>,
    /// Current top-level process status.
    pub status: TerminalSessionStatus,
    /// Initial bounded output captured before publication.
    pub motd: String,
}

/// Backend-owned live session retained by the owner-scoped registry.
#[async_trait]
pub trait TerminalBackendSession: fmt::Debug + Send + Sync {
    /// Initial bounded terminal output returned from terminal creation.
    fn motd(&self) -> String;
    /// Top-level process id when one exists.
    fn pid(&self) -> Option<ProcessId>;
    /// Starts one exclusive send operation.
    ///
    /// # Errors
    ///
    /// Returns backend validation or lifecycle failures before the operation starts.
    fn start_send(&self, request: TerminalSendRequest) -> TerminalResult<TerminalSendOperationRef>;
    /// Reads one bounded page from retained scrollback.
    ///
    /// # Errors
    ///
    /// Returns backend pagination or lifecycle failures.
    fn read(&self, request: TerminalReadRequest) -> TerminalResult<TerminalReadResult>;
    /// Signals the verified foreground process group.
    async fn signal(&self, signal: TerminalSignal) -> TerminalResult<TerminalSignalResult>;
    /// Observes top-level process status.
    fn status(&self) -> TerminalSessionStatus;
    /// Idempotently closes the captured owned process tree and awaits quiescence.
    async fn close(&self, reason: &str) -> TerminalResult<()>;
}

/// Shared backend session handle.
pub type TerminalBackendSessionRef = Arc<dyn TerminalBackendSession>;

/// Replaceable provider for one PTY session type.
#[async_trait]
pub trait TerminalBackend: fmt::Debug + Send + Sync {
    /// Stable type selected by a spawn request.
    fn backend_type(&self) -> &str;
    /// Creates one unpublished session or rejects after partial cleanup.
    async fn spawn(
        &self,
        spec: TerminalBackendSpawnSpec,
    ) -> TerminalResult<TerminalBackendSessionRef>;
}

/// Shared terminal backend provider.
pub type TerminalBackendRef = Arc<dyn TerminalBackend>;
