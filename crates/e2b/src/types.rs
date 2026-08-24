//! Object-safe E2B client contracts shared by capability adapters.

use std::{collections::BTreeMap, sync::Arc};

use futures::{future::BoxFuture, stream::BoxStream};
use seekdeep_llm::AbortSignal;

/// Remote entry classification from the E2B SDK.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum E2bFileType {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Another remote node type.
    Other,
}

/// Remote metadata returned by file operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct E2bEntryInfo {
    /// Basename.
    pub name: String,
    /// Absolute remote path.
    pub path: String,
    /// Node type.
    pub kind: E2bFileType,
    /// Byte size.
    pub size: u64,
    /// Unix permission bits.
    pub mode: u32,
    /// Stable modified-time string from the provider.
    pub modified_time: Option<String>,
    /// Symbolic-link target.
    pub symlink_target: Option<String>,
    /// Provider metadata.
    pub metadata: BTreeMap<String, String>,
}

/// Remote command result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct E2bCommandResult {
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
}

/// Completed background-command result.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct E2bCommandCompletion {
    /// Direct command exit code.
    pub exit_code: i32,
}

/// Asynchronous output callback supplied to a managed background command.
pub type E2bOutputCallback =
    Arc<dyn Fn(String) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

/// Managed background-command start request.
#[derive(Clone)]
pub struct E2bCommandStartOptions {
    /// Remote working directory.
    pub cwd: String,
    /// Whether the SDK command exposes stdin.
    pub stdin: bool,
    /// Provider command timeout; zero means no command-level deadline.
    pub timeout_ms: f64,
    /// Login-shell environment overrides.
    pub env: BTreeMap<String, String>,
    /// Optional allocation cancellation.
    pub signal: Option<AbortSignal>,
    /// Decoded SDK stdout callback.
    pub on_stdout: E2bOutputCallback,
    /// Decoded SDK stderr callback.
    pub on_stderr: E2bOutputCallback,
}

impl std::fmt::Debug for E2bCommandStartOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("E2bCommandStartOptions")
            .field("cwd", &self.cwd)
            .field("stdin", &self.stdin)
            .field("timeout_ms", &self.timeout_ms)
            .field("env", &self.env)
            .field("signal", &self.signal)
            .field("on_stdout", &"<callback>")
            .field("on_stderr", &"<callback>")
            .finish()
    }
}

/// One provider-owned managed background command.
#[async_trait::async_trait]
pub trait E2bCommandHandle: std::fmt::Debug + Send + Sync + 'static {
    /// SDK-reported direct command pid.
    fn pid(&self) -> i64;
    /// Waits for direct command settlement.
    async fn wait(&self) -> anyhow::Result<E2bCommandCompletion>;
    /// Sends one stdin chunk.
    async fn send_stdin(&self, data: Vec<u8>) -> anyhow::Result<()>;
    /// Delivers stdin EOF.
    async fn close_stdin(&self) -> anyhow::Result<()>;
    /// Requests provider-level termination.
    async fn kill(&self) -> anyhow::Result<bool>;
    /// Disconnects callbacks that outlive direct status publication.
    async fn disconnect(&self) -> anyhow::Result<()>;
}

/// Shared managed background-command handle.
pub type E2bCommandHandleRef = Arc<dyn E2bCommandHandle>;

/// Synchronous PTY data callback.
pub type E2bPtyDataCallback = Arc<dyn Fn(Vec<u8>) + Send + Sync>;

/// Remote PTY allocation request.
#[derive(Clone)]
pub struct E2bPtyCreateOptions {
    /// Initial rows.
    pub rows: u32,
    /// Initial columns.
    pub cols: u32,
    /// Remote working directory.
    pub cwd: String,
    /// Login-shell environment overrides.
    pub env: BTreeMap<String, String>,
    /// Provider command timeout; zero means no command-level deadline.
    pub timeout_ms: f64,
    /// Raw PTY output callback.
    pub on_data: E2bPtyDataCallback,
}

impl std::fmt::Debug for E2bPtyCreateOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("E2bPtyCreateOptions")
            .field("rows", &self.rows)
            .field("cols", &self.cols)
            .field("cwd", &self.cwd)
            .field("env", &self.env)
            .field("timeout_ms", &self.timeout_ms)
            .field("on_data", &"<callback>")
            .finish()
    }
}

/// Remote PTY operations used by the subprocess adapter.
#[async_trait::async_trait]
pub trait E2bPty: Send + Sync + 'static {
    /// Allocates one PTY command handle.
    async fn create(&self, options: E2bPtyCreateOptions) -> anyhow::Result<E2bCommandHandleRef>;
    /// Sends raw input to a PTY by provider pid.
    async fn send_input(
        &self,
        pid: i64,
        data: Vec<u8>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<()>;
}

/// Provider-specific missing-file error.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct E2bFileNotFound {
    /// Provider diagnostic.
    pub message: String,
}

/// Provider-specific command exit.
#[derive(Clone, Debug, thiserror::Error)]
#[error("command exited with status {status}: {stderr}")]
pub struct E2bCommandExit {
    /// Exit status.
    pub status: i32,
    /// Standard error.
    pub stderr: String,
}

/// Provider-specific already-deleted sandbox error.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct E2bSandboxNotFound {
    /// Provider diagnostic.
    pub message: String,
}

/// Byte stream plus its synchronous remote-cancellation hook.
pub struct E2bByteStream {
    /// Streamed chunks.
    pub stream: BoxStream<'static, anyhow::Result<Vec<u8>>>,
    /// Best-effort cancellation when a consumer abandons the stream.
    pub cancel: Arc<dyn Fn() + Send + Sync>,
}

/// Remote E2B file operations used by adapters and the owner.
#[async_trait::async_trait]
pub trait E2bFiles: Send + Sync + 'static {
    /// Returns path metadata.
    async fn get_info(
        &self,
        path: &str,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bEntryInfo>;
    /// Reads all bytes.
    async fn read_bytes(&self, path: &str, signal: Option<&AbortSignal>)
    -> anyhow::Result<Vec<u8>>;
    /// Opens a byte stream.
    async fn read_stream(
        &self,
        path: &str,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bByteStream>;
    /// Lists direct children.
    async fn list(
        &self,
        path: &str,
        depth: u32,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Vec<E2bEntryInfo>>;
    /// Creates one directory and reports whether creation won.
    async fn make_dir(&self, path: &str, signal: Option<&AbortSignal>) -> anyhow::Result<bool>;
    /// Writes one remote file with metadata.
    async fn write(
        &self,
        path: &str,
        content: &str,
        metadata: BTreeMap<String, String>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<()>;
    /// Renames a path and returns committed metadata.
    async fn rename(
        &self,
        from: &str,
        to: &str,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bEntryInfo>;
    /// Removes a private staging directory.
    async fn remove(&self, path: &str) -> anyhow::Result<()>;
}

/// Remote E2B command operations.
#[async_trait::async_trait]
pub trait E2bCommands: Send + Sync + 'static {
    /// Runs one control command.
    async fn run(
        &self,
        command: &str,
        env: BTreeMap<String, String>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bCommandResult>;

    /// Runs one control command from an explicit remote working directory.
    async fn run_in(
        &self,
        command: &str,
        _cwd: &str,
        env: BTreeMap<String, String>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bCommandResult> {
        self.run(command, env, signal).await
    }

    /// Starts one managed background command.
    async fn start(
        &self,
        _command: &str,
        _options: E2bCommandStartOptions,
    ) -> anyhow::Result<E2bCommandHandleRef> {
        anyhow::bail!("E2B SDK binding does not implement managed background commands")
    }
}

/// One live remote sandbox.
#[async_trait::async_trait]
pub trait E2bSandbox: Send + Sync + 'static {
    /// Stable provider sandbox id.
    fn sandbox_id(&self) -> &str;
    /// Files API.
    fn files(&self) -> Arc<dyn E2bFiles>;
    /// Commands API.
    fn commands(&self) -> Arc<dyn E2bCommands>;
    /// Optional PTY API.
    fn pty(&self) -> Option<Arc<dyn E2bPty>> {
        None
    }
    /// Deletes the remote sandbox.
    async fn kill(&self) -> anyhow::Result<()>;
}

/// Sandbox creation request.
#[derive(Clone, Debug, PartialEq)]
pub struct E2bCreateOptions {
    /// API key used only by the provider call.
    pub api_key: String,
    /// Sandbox lifetime in milliseconds.
    pub timeout_ms: f64,
    /// Provider secure-mode request.
    pub secure: bool,
    /// Whether timeout deletes the sandbox.
    pub kill_on_timeout: bool,
}

/// Asynchronous sandbox creation result.
pub type E2bSandboxFuture = BoxFuture<'static, anyhow::Result<Arc<dyn E2bSandbox>>>;

/// Object-safe SDK factory.
pub trait E2bSandboxFactory: Send + Sync + 'static {
    /// Creates one sandbox.
    fn create(&self, options: E2bCreateOptions) -> E2bSandboxFuture;
}
