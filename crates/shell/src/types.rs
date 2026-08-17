//! Foreground execution and task-free background-process vocabulary.

use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use seekdeep_llm::AbortSignal;
pub use seekdeep_sandbox::{SandboxEnforcement, SandboxExecutionPolicy, SandboxMode};

pub use seekdeep_subprocess::{
    CollectedOutput, ProcessSignal, SEEKDEEP_ENV_PREFIX, SeekDeepEnvironment,
    SeekDeepEnvironmentKey,
};

/// Sandbox facts reported independently of process exit status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellSandboxInfo {
    /// Mode the command actually ran under.
    pub mode: SandboxMode,
    /// Whether the sandbox denied a file operation.
    pub denied: bool,
    /// Enforcement completeness when known.
    pub enforcement: Option<SandboxEnforcement>,
    /// Whether the runner failed before the command could run.
    pub runner_failed: Option<bool>,
}

/// Caller-facing execution request before provider defaults and caps.
#[derive(Clone, Debug)]
pub struct ShellExecRequest {
    /// Shell command text.
    pub command: String,
    /// Working directory override.
    pub workdir: Option<PathBuf>,
    /// Timeout override in milliseconds.
    pub timeout_ms: Option<u64>,
    /// Foreground stdout capture budget.
    pub stdout_max_bytes: Option<usize>,
    /// Caller cancellation.
    pub signal: Option<AbortSignal>,
    /// Bytes written once to stdin before close.
    pub stdin: Option<String>,
    /// Ordinary explicit child environment.
    pub env: Option<BTreeMap<String, String>>,
    /// Trusted managed environment snapshot.
    pub seekdeep_env: Option<SeekDeepEnvironment>,
    /// Complete per-call sandbox policy.
    pub sandbox_policy: Option<SandboxExecutionPolicy>,
}

impl ShellExecRequest {
    /// Builds the mandatory command with every optional field absent.
    #[must_use]
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            workdir: None,
            timeout_ms: None,
            stdout_max_bytes: None,
            signal: None,
            stdin: None,
            env: None,
            seekdeep_env: None,
            sandbox_policy: None,
        }
    }
}

/// Fully resolved execution specification handed to a provider.
#[derive(Clone, Debug)]
pub struct ShellExecSpec {
    /// Shell command text.
    pub command: String,
    /// Resolved working directory.
    pub workdir: PathBuf,
    /// Effective foreground timeout in milliseconds.
    pub timeout_ms: u64,
    /// Effective stdout capture budget.
    pub stdout_max_bytes: usize,
    /// Caller cancellation.
    pub signal: Option<AbortSignal>,
    /// Bytes written once to stdin before close.
    pub stdin: Option<String>,
    /// Ordinary explicit child environment.
    pub env: Option<BTreeMap<String, String>>,
    /// Trusted managed environment snapshot.
    pub seekdeep_env: Option<SeekDeepEnvironment>,
    /// Resolved policy; unsandboxed providers carry and ignore it.
    pub sandbox_policy: Option<SandboxExecutionPolicy>,
}

/// Completed or killed foreground execution outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellRunResult {
    /// Exit code, absent when terminated by signal.
    pub exit_code: Option<i32>,
    /// Terminating signal, absent on ordinary exit.
    pub signal: Option<ProcessSignal>,
    /// Whether the executor timeout was the first abort cause.
    pub timed_out: bool,
    /// Whether caller cancellation was the first abort cause.
    pub aborted: bool,
    /// Effective timeout after defaulting and caps.
    pub timeout_ms: u64,
    /// Captured stdout.
    pub stdout: CollectedOutput,
    /// Captured stderr.
    pub stderr: CollectedOutput,
    /// Sandbox facts for a confined executor.
    pub sandbox: Option<ShellSandboxInfo>,
}

/// Lifecycle of one background process handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellProcessStatus {
    /// Underlying process is live.
    Running,
    /// Underlying process closed normally.
    Completed,
    /// Underlying process was killed or failed to spawn.
    Killed,
}

/// One consuming incremental process-output read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShellProcessRead {
    /// Output since the previous read, with stderr in a marked section.
    pub delta: String,
    /// Whether unread bytes were dropped.
    pub lossy: bool,
    /// Complete stdout spill file when available.
    pub stdout_spill_path: Option<PathBuf>,
    /// Complete stderr spill file when available.
    pub stderr_spill_path: Option<PathBuf>,
}

/// Task-free live background process handle.
#[async_trait]
pub trait ShellProcess: std::fmt::Debug + Send + Sync {
    /// Current lifecycle state.
    fn status(&self) -> ShellProcessStatus;
    /// Exit code once finished, absent while running or signal-killed.
    fn exit_code(&self) -> Option<i32>;
    /// Terminating signal once known.
    fn signal(&self) -> Option<ProcessSignal>;
    /// Sandbox facts once known.
    fn sandbox(&self) -> Option<ShellSandboxInfo>;
    /// Waits for process close; spawn failures are represented on the handle.
    async fn done(&self);
    /// Consumes output produced since this handle's previous read.
    fn read_output(&self) -> ShellProcessRead;
    /// Kills the process group, returning false after settlement.
    fn kill(&self) -> bool;
}

/// Shared process-handle value returned by executor implementations.
pub type ShellProcessHandle = Arc<dyn ShellProcess>;
