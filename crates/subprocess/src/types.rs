//! Fully specified subprocess requests, process handles, and terminal handles.

use std::{collections::BTreeMap, path::PathBuf, pin::Pin, sync::Arc};

use async_trait::async_trait;
use seekdeep_llm::AbortSignal;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _};

/// Namespace prefix reserved for harness-managed child environment facts.
pub const SEEKDEEP_ENV_PREFIX: &str = "SEEKDEEP_";

/// Operating-system process identity crossing the subprocess capability seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProcessId(i64);

impl ProcessId {
    /// Wraps one provider-reported process id, including the spawn-failure sentinel `-1`.
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// Raw operating-system value.
    #[must_use]
    pub const fn as_i64(self) -> i64 {
        self.0
    }
}

/// POSIX process-group identity crossing terminal inspection and signalling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProcessGroupId(i64);

impl ProcessGroupId {
    /// Wraps one provider-reported process-group id.
    #[must_use]
    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    /// Raw operating-system value.
    #[must_use]
    pub const fn as_i64(self) -> i64 {
        self.0
    }
}

/// Validated key inside the managed [`SEEKDEEP_ENV_PREFIX`] namespace.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SeekDeepEnvironmentKey(String);

impl SeekDeepEnvironmentKey {
    /// Validates and wraps one managed environment key.
    ///
    /// # Errors
    ///
    /// Rejects keys outside the `SEEKDEEP_*` namespace.
    pub fn new(value: impl Into<String>) -> anyhow::Result<Self> {
        let value = value.into();
        anyhow::ensure!(
            value.to_ascii_uppercase().starts_with(SEEKDEEP_ENV_PREFIX),
            "managed subprocess environment key must start with {SEEKDEEP_ENV_PREFIX}"
        );
        Ok(Self(value))
    }

    /// Borrowed environment spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Trusted harness variables for one child-process execution.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SeekDeepEnvironment(BTreeMap<SeekDeepEnvironmentKey, String>);

impl SeekDeepEnvironment {
    /// Builds a managed environment from already validated keys.
    #[must_use]
    pub fn new(values: BTreeMap<SeekDeepEnvironmentKey, String>) -> Self {
        Self(values)
    }

    /// Iterates the checked keys and their values.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }
}

/// One captured stream: possibly truncated tail plus recovery information.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectedOutput {
    /// Collected text, which is the stream tail after truncation.
    pub text: String,
    /// Whether bytes were dropped from `text`.
    pub truncated: bool,
    /// Complete-stream spill file when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spill_path: Option<PathBuf>,
}

/// Extensible operating-system process signal name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProcessSignal(String);

impl ProcessSignal {
    /// Wraps a provider-reported signal spelling.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrowed signal spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Explicit stdin disposition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubprocessStdinMode {
    /// Connect fd 0 to the platform null device.
    Ignore,
    /// Expose a writable stream for ongoing protocol traffic.
    Pipe,
    /// Write one batch and close stdin.
    Data(String),
}

/// Optional full-stream spill configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct SubprocessSpill {
    /// Whole-stream byte cap; overflow invalidates the incomplete spill.
    pub max_bytes: f64,
}

/// Bounded in-memory collection configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct SubprocessCollect {
    /// In-memory byte cap; overflow retains the tail.
    pub max_bytes: f64,
    /// Optional complete-stream spill recovery.
    pub spill: Option<SubprocessSpill>,
}

/// Explicit stdout or stderr disposition.
#[derive(Clone, Debug, PartialEq)]
pub enum SubprocessOutputMode {
    /// Expose the raw readable stream.
    Pipe,
    /// Pass the parent descriptor through.
    Inherit,
    /// Buffer with an offset-based reader.
    Collect(SubprocessCollect),
}

/// All three explicit stdio dispositions.
#[derive(Clone, Debug, PartialEq)]
pub struct SubprocessStdio {
    /// stdin disposition.
    pub stdin: SubprocessStdinMode,
    /// stdout disposition.
    pub stdout: SubprocessOutputMode,
    /// stderr disposition.
    pub stderr: SubprocessOutputMode,
}

/// Explicit environment layer; `None` is a tombstone for an ambient key.
pub type SubprocessEnvironment = BTreeMap<String, Option<String>>;
/// String-only explicit environment used during executable lookup.
pub type SubprocessLookupEnvironment = BTreeMap<String, String>;

/// Fully specified ordinary-process spawn request.
#[derive(Clone, Debug)]
pub struct SubprocessSpawnSpec {
    /// Executable and arguments; element zero is the executable.
    pub argv: Vec<String>,
    /// Child working directory.
    pub cwd: PathBuf,
    /// Explicit stdio dispositions.
    pub stdio: SubprocessStdio,
    /// TERM-to-KILL and inherited-pipe grace in milliseconds.
    pub grace_ms: f64,
    /// Optional caller-owned cancellation.
    pub signal: Option<AbortSignal>,
    /// Explicit environment merged after the provider's ambient scrub.
    pub env: Option<SubprocessEnvironment>,
}

/// Closed direct-process exit facts without cause classification or output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubprocessOutcome {
    /// Exit code, absent when signal-terminated.
    pub exit_code: Option<i32>,
    /// Terminating signal, absent on ordinary exit.
    pub signal: Option<ProcessSignal>,
}

/// One offset-based collected-output read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubprocessOutputRead {
    /// Text since the requested whole-stream byte offset.
    pub text: String,
    /// Whole-stream byte offset for the next read.
    pub next_offset: u64,
    /// Whether the requested offset slid out of the retained tail.
    pub lossy: bool,
    /// Complete-stream spill path when an intact spill exists.
    pub spill_path: Option<PathBuf>,
}

/// Cursor-free, independently readable collected stream.
pub trait SubprocessOutputReader: std::fmt::Debug + Send + Sync {
    /// Reads from one whole-stream byte offset.
    fn read_from(&self, from_byte: u64) -> SubprocessOutputRead;
}

/// Shared collected-output reader.
pub type SubprocessOutputReaderHandle = Arc<dyn SubprocessOutputReader>;

/// Readers present exactly for streams requested in collect mode.
#[derive(Clone, Debug, Default)]
pub struct SubprocessCollectedOutputs {
    /// stdout reader.
    pub stdout: Option<SubprocessOutputReaderHandle>,
    /// stderr reader.
    pub stderr: Option<SubprocessOutputReaderHandle>,
}

/// Erased provider-owned writable child pipe.
type BoxedSubprocessInput = Pin<Box<dyn AsyncWrite + Send + Unpin + 'static>>;

/// Shared asynchronous child-stdin stream with an explicit EOF operation.
#[derive(Clone)]
pub struct SubprocessInput {
    inner: Arc<tokio::sync::Mutex<Option<BoxedSubprocessInput>>>,
}

impl std::fmt::Debug for SubprocessInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SubprocessInput")
            .finish_non_exhaustive()
    }
}

impl SubprocessInput {
    /// Wraps one provider-owned writable pipe.
    #[must_use]
    pub fn new(stream: BoxedSubprocessInput) -> Self {
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(Some(stream))),
        }
    }

    /// Writes all bytes to the still-open child pipe.
    ///
    /// # Errors
    ///
    /// Returns an explicit closed-pipe error or the underlying write failure.
    pub async fn write_all(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut inner = self.inner.lock().await;
        let stream = inner.as_mut().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "subprocess stdin is closed")
        })?;
        stream.write_all(bytes).await
    }

    /// Shuts down the writable descriptor, delivering EOF to the child. Idempotent.
    ///
    /// # Errors
    ///
    /// Returns the provider-owned EOF delivery failure.
    pub async fn close(&self) -> std::io::Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(stream) = inner.as_mut() {
            stream.shutdown().await?;
        }
        inner.take();
        Ok(())
    }

    /// Whether EOF has been delivered by closing this surface.
    pub async fn is_closed(&self) -> bool {
        self.inner.lock().await.is_none()
    }
}

/// Shared asynchronous child-output stream.
pub type SubprocessOutput =
    Arc<tokio::sync::Mutex<Pin<Box<dyn AsyncRead + Send + Unpin + 'static>>>>;

/// Live owned ordinary process tree.
#[async_trait]
pub trait SubprocessHandle: std::fmt::Debug + Send + Sync {
    /// Direct child pid, or `-1` when spawning failed.
    fn pid(&self) -> ProcessId;
    /// Writable stdin iff requested as a pipe.
    fn stdin(&self) -> Option<SubprocessInput>;
    /// Raw stdout iff requested as a pipe.
    fn stdout(&self) -> Option<SubprocessOutput>;
    /// Raw stderr iff requested as a pipe.
    fn stderr(&self) -> Option<SubprocessOutput>;
    /// Offset readers for collect-mode streams.
    fn collected(&self) -> SubprocessCollectedOutputs;
    /// Waits for direct-process close; only spawn-level failures reject.
    async fn done(&self) -> anyhow::Result<SubprocessOutcome>;
    /// Starts idempotent tree-scoped termination escalation.
    fn terminate(&self);
    /// Waits for whole-tree exit, returning false when the wait signal wins.
    ///
    /// # Errors
    ///
    /// Returns provider liveness or cleanup failures.
    async fn wait_for_exit(&self, signal: Option<AbortSignal>) -> anyhow::Result<bool>;
}

/// Shared ordinary-process handle.
pub type SubprocessHandleRef = Arc<dyn SubprocessHandle>;

/// Signals supported by the terminal-process primitive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SubprocessTerminalSignal {
    /// Interrupt.
    Sigint,
    /// Graceful termination.
    Sigterm,
    /// Uncatchable termination.
    Sigkill,
    /// Terminal stop.
    Sigtstp,
    /// Hangup.
    Sighup,
}

impl SubprocessTerminalSignal {
    /// Exact operating-system signal spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sigint => "SIGINT",
            Self::Sigterm => "SIGTERM",
            Self::Sigkill => "SIGKILL",
            Self::Sigtstp => "SIGTSTP",
            Self::Sighup => "SIGHUP",
        }
    }
}

/// Fully specified terminal-session spawn request.
#[derive(Clone, Debug)]
pub struct SubprocessTerminalSpawnSpec {
    /// Executable and arguments.
    pub argv: Vec<String>,
    /// Working directory in the provider's execution world.
    pub cwd: PathBuf,
    /// Explicit environment after ambient scrubbing.
    pub env: Option<BTreeMap<String, String>>,
    /// Initial terminal rows.
    pub rows: u32,
    /// Initial terminal columns.
    pub cols: u32,
    /// TERM-to-KILL session cleanup grace in milliseconds.
    pub grace_ms: f64,
    /// Cancellation of allocation only.
    pub signal: Option<AbortSignal>,
}

/// Current terminal foreground-process-group facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubprocessTerminalForeground {
    /// Foreground process-group identity.
    pub process_group_id: ProcessGroupId,
    /// Whether the provider proves that group is waiting for terminal input.
    pub input_waiting: bool,
}

/// Live terminal process and its owned operating-system session.
#[async_trait]
pub trait SubprocessTerminalHandle: std::fmt::Debug + Send + Sync {
    /// Top-level terminal child pid.
    fn pid(&self) -> ProcessId;
    /// UTF-8 terminal output in delivery order.
    fn output(&self) -> SubprocessOutput;
    /// Waits for top-level process exit.
    async fn done(&self) -> anyhow::Result<SubprocessOutcome>;
    /// Writes text without newline conversion.
    async fn write(&self, data: &str) -> anyhow::Result<()>;
    /// Resolves current foreground group facts when available.
    async fn inspect_foreground(&self) -> anyhow::Result<Option<SubprocessTerminalForeground>>;
    /// Signals the current foreground group and returns its exact id.
    async fn signal_foreground(
        &self,
        signal: SubprocessTerminalSignal,
    ) -> anyhow::Result<ProcessGroupId>;
    /// Idempotently terminates and awaits the complete observed session.
    async fn terminate(&self) -> anyhow::Result<()>;
}

/// Shared terminal-session handle.
pub type SubprocessTerminalHandleRef = Arc<dyn SubprocessTerminalHandle>;
