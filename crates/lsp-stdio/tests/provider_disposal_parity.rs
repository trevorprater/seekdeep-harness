//! Provider-disposal cancellation and join boundaries over injected filesystem waits.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::{StreamExt as _, stream::BoxStream};
use seekdeep_cordis::Context;
use seekdeep_fs::{
    FileSystem, FileSystemService, FsDirEntry, FsEditOutcome, FsEditRequest, FsInfo, FsPathInfo,
    FsTarget, FsVersion, FsWriteIntent, FsWriteOutcome,
};
use seekdeep_fs_local::{Config as FsConfig, LocalFileSystem};
use seekdeep_llm::AbortSignal;
use seekdeep_lsp::{Lsp, LspOperation, LspPosition, LspQueryRequest};
use seekdeep_lsp_stdio::{abort_error, plugin};
use seekdeep_sandbox::SandboxExecutionPolicy;
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use serde_json::json;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BlockMode {
    Workspace,
    Stream,
}

#[derive(Debug)]
struct BlockingState {
    mode: BlockMode,
    workspace: String,
    started: AtomicBool,
    aborted: AtomicBool,
    released: AtomicBool,
    signal: parking_lot::Mutex<Option<AbortSignal>>,
    notify: tokio::sync::Notify,
}

impl BlockingState {
    async fn wait(&self, flag: &AtomicBool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let notified = self.notify.notified();
                if flag.load(Ordering::Acquire) {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("filesystem wait boundary timed out");
    }

    fn start(&self, signal: &AbortSignal) {
        *self.signal.lock() = Some(signal.clone());
        self.started.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    fn observe_abort(&self) {
        self.aborted.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    fn release(&self) {
        self.released.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }
}

struct BlockingFilesystem {
    inner: Arc<dyn FileSystem>,
    state: Arc<BlockingState>,
}

#[async_trait]
impl FileSystem for BlockingFilesystem {
    async fn resolve(
        &self,
        path: &str,
        cwd: Option<&str>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<FsTarget> {
        if self.state.mode == BlockMode::Workspace && path == self.state.workspace {
            let signal =
                signal.ok_or_else(|| anyhow::anyhow!("workspace lookup missing signal"))?;
            self.state.start(signal);
            signal.cancelled().await;
            self.state.observe_abort();
            self.state.wait(&self.state.released).await;
            return Err(abort_error(signal));
        }
        self.inner.resolve(path, cwd, signal).await
    }

    fn process_path(&self, target: &FsTarget) -> String {
        self.inner.process_path(target)
    }

    fn file_url(&self, target: &FsTarget) -> String {
        self.inner.file_url(target)
    }

    fn contains(&self, parent: &FsTarget, child: &FsTarget) -> bool {
        self.inner.contains(parent, child)
    }

    async fn stat(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<FsInfo>> {
        self.inner.stat(target, signal).await
    }

    async fn lstat(
        &self,
        path: &str,
        cwd: Option<&str>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<FsPathInfo>> {
        self.inner.lstat(path, cwd, signal).await
    }

    async fn read_text(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<String> {
        self.inner.read_text(target, signal).await
    }

    async fn stream_text(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<String>>> {
        if self.state.mode == BlockMode::Stream {
            let signal = signal
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("source stream missing signal"))?;
            self.state.start(&signal);
            let state = self.state.clone();
            return Ok(futures::stream::once(async move {
                signal.cancelled().await;
                state.observe_abort();
                Err(abort_error(&signal))
            })
            .boxed());
        }
        self.inner.stream_text(target, signal).await
    }

    async fn read_bytes(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
        max_bytes: usize,
    ) -> anyhow::Result<Vec<u8>> {
        self.inner.read_bytes(target, signal, max_bytes).await
    }

    async fn list_dir(
        &self,
        target: &FsTarget,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Vec<FsDirEntry>> {
        self.inner.list_dir(target, signal).await
    }

    async fn write_text(
        &self,
        target: &FsTarget,
        content: &str,
        expected: Option<&FsWriteIntent>,
        signal: Option<&AbortSignal>,
        sandbox_policy: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Result<FsWriteOutcome> {
        self.inner
            .write_text(target, content, expected, signal, sandbox_policy)
            .await
    }

    async fn edit_text(
        &self,
        target: &FsTarget,
        edit: &FsEditRequest,
        expected: Option<&FsVersion>,
        signal: Option<&AbortSignal>,
        sandbox_policy: Option<&SandboxExecutionPolicy>,
    ) -> anyhow::Result<FsEditOutcome> {
        self.inner
            .edit_text(target, edit, expected, signal, sandbox_policy)
            .await
    }
}

async fn setup(mode: BlockMode) -> (Context, Arc<Lsp>, Arc<BlockingState>, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    let canonical = tokio::fs::canonicalize(root.path()).await.unwrap();
    let workspace = canonical.join("ws");
    tokio::fs::create_dir(&workspace).await.unwrap();
    tokio::fs::write(workspace.join("a.ts"), "const x = 1\n")
        .await
        .unwrap();
    let local = LocalFileSystem::new(FsConfig {
        cwd: Some(canonical.to_string_lossy().into_owned()),
        diff_basis_max_bytes: None,
    })
    .unwrap();
    let state = Arc::new(BlockingState {
        mode,
        workspace: workspace.to_string_lossy().into_owned(),
        started: AtomicBool::new(false),
        aborted: AtomicBool::new(false),
        released: AtomicBool::new(false),
        signal: parking_lot::Mutex::new(None),
        notify: tokio::sync::Notify::new(),
    });
    let filesystem = Arc::new(BlockingFilesystem {
        inner: local,
        state: state.clone(),
    });
    let context = Context::new();
    let lsp = Arc::new(Lsp::new());
    lsp.provide(&context).unwrap();
    LocalSubprocessRuntime::install(&context).unwrap();
    FileSystemService::new(filesystem)
        .provide(&context)
        .unwrap();
    let fiber = context
        .plugin(
            plugin(),
            json!({"servers": {"fake": {
                "command": env!("CARGO_BIN_EXE_seekdeep-lsp-stdio-fixture"),
                "extensionToLanguage": {".ts": "typescript"},
                "env": {"LSP_FAKE_DEF": "null"}
            }}}),
        )
        .unwrap();
    fiber.await_settled().await.unwrap();
    (context, lsp, state, root)
}

fn request(workspace: &str) -> LspQueryRequest {
    LspQueryRequest {
        operation: LspOperation::GoToDefinition,
        file_path: "a.ts".to_owned(),
        position: LspPosition {
            line: 0.0,
            character: 0.0,
        },
        workspace_root: workspace.to_owned(),
    }
}

#[tokio::test]
async fn disposal_aborts_and_joins_a_workspace_lookup_before_returning() {
    let (context, lsp, state, _root) = setup(BlockMode::Workspace).await;
    let query_lsp = lsp.clone();
    let query = request(&state.workspace);
    let pending = tokio::spawn(async move { query_lsp.query(query, None).await });
    state.wait(&state.started).await;
    let root_fiber = context.fiber().clone();
    let disposing = tokio::spawn(async move { root_fiber.restart().await });
    state.wait(&state.aborted).await;
    assert!(state.signal.lock().as_ref().unwrap().is_aborted());
    assert!(!disposing.is_finished());
    state.release();
    assert!(
        pending
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("provider is disposed")
    );
    disposing.await.unwrap().unwrap();
}

#[tokio::test]
async fn disposal_aborts_and_joins_a_source_stream_inside_the_workspace_queue() {
    let (context, lsp, state, _root) = setup(BlockMode::Stream).await;
    let query_lsp = lsp.clone();
    let query = request(&state.workspace);
    let pending = tokio::spawn(async move { query_lsp.query(query, None).await });
    state.wait(&state.started).await;
    let root_fiber = context.fiber().clone();
    let disposing = tokio::spawn(async move { root_fiber.restart().await });
    state.wait(&state.aborted).await;
    assert!(
        pending
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("provider is disposed")
    );
    disposing.await.unwrap().unwrap();
    assert!(state.signal.lock().as_ref().unwrap().is_aborted());
}
