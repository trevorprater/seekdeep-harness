//! Object-safe E2B client boundary used by the filesystem backend.

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

/// Remote metadata returned by `getInfo`, list, rename, or staging lookup.
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
    /// Symbolic-link target when this listing row is a link.
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

/// Byte stream plus its synchronous remote-cancellation hook.
pub struct E2bByteStream {
    /// Streamed chunks.
    pub stream: BoxStream<'static, anyhow::Result<Vec<u8>>>,
    /// Best-effort cancellation when a consumer abandons the stream.
    pub cancel: Arc<dyn Fn() + Send + Sync>,
}

/// Remote E2B file operations used by the provider.
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
    /// Opens a byte stream. Empty files return an empty stream.
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

/// Remote E2B command operations used for canonicalization and atomic publication.
#[async_trait::async_trait]
pub trait E2bCommands: Send + Sync + 'static {
    /// Runs one control command.
    async fn run(
        &self,
        command: &str,
        env: BTreeMap<String, String>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bCommandResult>;
}

/// One live remote sandbox.
pub trait E2bSandbox: Send + Sync + 'static {
    /// Files API.
    fn files(&self) -> Arc<dyn E2bFiles>;
    /// Commands API.
    fn commands(&self) -> Arc<dyn E2bCommands>;
}

/// Asynchronous sandbox resolver owned by the E2B capability provider.
pub type E2bSandboxFuture = BoxFuture<'static, anyhow::Result<Arc<dyn E2bSandbox>>>;

/// E2B capability service used by this backend.
pub struct E2bService {
    cwd: String,
    get_sandbox: Arc<dyn Fn() -> E2bSandboxFuture + Send + Sync>,
}

impl std::fmt::Debug for E2bService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("E2bService")
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

impl E2bService {
    /// Creates the capability service.
    #[must_use]
    pub fn new(
        cwd: impl Into<String>,
        get_sandbox: Arc<dyn Fn() -> E2bSandboxFuture + Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(Self {
            cwd: cwd.into(),
            get_sandbox,
        })
    }

    /// Default remote working directory.
    #[must_use]
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// Resolves the live sandbox.
    ///
    /// # Errors
    ///
    /// Returns provider startup, authentication, or lifecycle failures.
    pub async fn get_sandbox(&self) -> anyhow::Result<Arc<dyn E2bSandbox>> {
        (self.get_sandbox)().await
    }
}
