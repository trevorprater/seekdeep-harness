//! E2B PTY allocation, output boundary, foreground, and session-cleanup parity.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
    },
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_e2b::{
    E2B, E2bByteStream, E2bCommandCompletion, E2bCommandExit, E2bCommandHandle,
    E2bCommandHandleRef, E2bCommandResult, E2bCommands, E2bEntryInfo, E2bFileType, E2bFiles,
    E2bPty, E2bPtyCreateOptions, E2bSandbox, E2bService,
};
use seekdeep_llm::AbortSignal;
use seekdeep_subprocess::{
    SubprocessRuntime as _, SubprocessTerminalHandle as _, SubprocessTerminalSignal,
    SubprocessTerminalSpawnSpec,
};
use seekdeep_subprocess_e2b::{
    E2bSubprocessConfig, E2bSubprocessRuntime, terminal::spawn_e2b_terminal,
};
use tokio::io::AsyncReadExt as _;

#[derive(Clone, Debug)]
enum Completion {
    Pending,
    Exit(i32),
}

#[derive(Debug)]
struct FakeHandle {
    pid: AtomicI64,
    completion: tokio::sync::watch::Sender<Completion>,
    kills: AtomicUsize,
    disconnects: AtomicUsize,
}

impl FakeHandle {
    fn new(pid: i64) -> Arc<Self> {
        let (completion, _) = tokio::sync::watch::channel(Completion::Pending);
        Arc::new(Self {
            pid: AtomicI64::new(pid),
            completion,
            kills: AtomicUsize::new(0),
            disconnects: AtomicUsize::new(0),
        })
    }

    fn finish(&self, exit_code: i32) {
        self.completion.send_replace(Completion::Exit(exit_code));
    }
}

#[async_trait::async_trait]
impl E2bCommandHandle for FakeHandle {
    fn pid(&self) -> i64 {
        self.pid.load(Ordering::Acquire)
    }

    async fn wait(&self) -> anyhow::Result<E2bCommandCompletion> {
        let mut completion = self.completion.subscribe();
        loop {
            match *completion.borrow() {
                Completion::Pending => {}
                Completion::Exit(0) => return Ok(E2bCommandCompletion { exit_code: 0 }),
                Completion::Exit(status) => {
                    return Err(E2bCommandExit {
                        status,
                        stderr: String::new(),
                    }
                    .into());
                }
            }
            completion.changed().await.unwrap();
        }
    }

    async fn send_stdin(&self, _data: Vec<u8>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn close_stdin(&self) -> anyhow::Result<()> {
        Ok(())
    }

    async fn kill(&self) -> anyhow::Result<bool> {
        self.kills.fetch_add(1, Ordering::AcqRel);
        self.finish(137);
        Ok(true)
    }

    async fn disconnect(&self) -> anyhow::Result<()> {
        self.disconnects.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

#[derive(Debug, Default)]
struct FakeFiles {
    values: Mutex<BTreeMap<String, Vec<u8>>>,
    removed: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl E2bFiles for FakeFiles {
    async fn get_info(
        &self,
        path: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bEntryInfo> {
        Ok(E2bEntryInfo {
            name: path.rsplit('/').next().unwrap_or_default().to_owned(),
            path: path.to_owned(),
            kind: E2bFileType::File,
            size: 0,
            mode: 0o600,
            modified_time: None,
            symlink_target: None,
            metadata: BTreeMap::new(),
        })
    }

    async fn read_bytes(
        &self,
        path: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Vec<u8>> {
        Ok(self.values.lock().get(path).cloned().unwrap_or_default())
    }

    async fn read_stream(
        &self,
        _path: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bByteStream> {
        anyhow::bail!("unused")
    }

    async fn list(
        &self,
        _path: &str,
        _depth: u32,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Vec<E2bEntryInfo>> {
        Ok(Vec::new())
    }

    async fn make_dir(&self, path: &str, _signal: Option<&AbortSignal>) -> anyhow::Result<bool> {
        self.values.lock().entry(path.to_owned()).or_default();
        Ok(true)
    }

    async fn write(
        &self,
        path: &str,
        content: &str,
        _metadata: BTreeMap<String, String>,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<()> {
        self.values
            .lock()
            .insert(path.to_owned(), content.as_bytes().to_vec());
        Ok(())
    }

    async fn rename(
        &self,
        _from: &str,
        _to: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bEntryInfo> {
        anyhow::bail!("unused")
    }

    async fn remove(&self, path: &str) -> anyhow::Result<()> {
        self.values.lock().remove(path);
        self.removed.lock().push(path.to_owned());
        Ok(())
    }
}

#[derive(Debug)]
struct FakeCommands {
    handle: Arc<FakeHandle>,
    commands: Mutex<Vec<String>>,
    groups: Mutex<Vec<i64>>,
    foreground: AtomicI64,
}

#[async_trait::async_trait]
impl E2bCommands for FakeCommands {
    async fn run(
        &self,
        command: &str,
        _env: BTreeMap<String, String>,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bCommandResult> {
        self.commands.lock().push(command.to_owned());
        if command.contains("env -0 | base64") {
            let raw = "PATH=/bin\0KEEP=safe\0API_KEY=secret\0SEEKDEEP_OLD=old\0";
            return Ok(E2bCommandResult {
                stdout: format!(
                    "{}\n{}",
                    STANDARD.encode("/home/user"),
                    STANDARD.encode(raw)
                ),
                stderr: String::new(),
            });
        }
        if command.starts_with("ps -o sid=") {
            return Ok(E2bCommandResult {
                stdout: "5000\n".to_owned(),
                stderr: String::new(),
            });
        }
        if command.starts_with("ps -o tpgid=") {
            let foreground = self.foreground.load(Ordering::Acquire);
            if foreground <= 0 {
                return Err(E2bCommandExit {
                    status: 1,
                    stderr: String::new(),
                }
                .into());
            }
            return Ok(E2bCommandResult {
                stdout: format!("{foreground}\n"),
                stderr: String::new(),
            });
        }
        if command.starts_with("set -o pipefail; ps -eo sid=") {
            return Ok(E2bCommandResult {
                stdout: self
                    .groups
                    .lock()
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
                stderr: String::new(),
            });
        }
        if command.starts_with("kill -TERM ") || command.starts_with("kill -KILL ") {
            self.groups.lock().clear();
            self.handle
                .finish(if command.contains("TERM") { 143 } else { 137 });
        }
        Ok(E2bCommandResult::default())
    }
}

#[derive(Debug)]
struct FakePty {
    files: Arc<FakeFiles>,
    handle: Arc<FakeHandle>,
    options: Mutex<Option<E2bPtyCreateOptions>>,
    inputs: Mutex<Vec<Vec<u8>>>,
    abort_on_create: Mutex<Option<AbortSignal>>,
    exit_before_marker: AtomicBool,
    emit_marker: AtomicBool,
    block_writes: AtomicBool,
}

#[async_trait::async_trait]
impl E2bPty for FakePty {
    async fn create(&self, options: E2bPtyCreateOptions) -> anyhow::Result<E2bCommandHandleRef> {
        *self.options.lock() = Some(options);
        if let Some(signal) = self.abort_on_create.lock().take() {
            signal.abort();
        }
        Ok(self.handle.clone())
    }

    async fn send_input(
        &self,
        _pid: i64,
        data: Vec<u8>,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<()> {
        self.inputs.lock().push(data.clone());
        if data.starts_with(b"exec /bin/bash") {
            let marker = self
                .files
                .values
                .lock()
                .iter()
                .find(|(path, _)| path.ends_with("/output-marker"))
                .map(|(_, value)| value.clone())
                .expect("marker file");
            if self.exit_before_marker.load(Ordering::Acquire) {
                self.handle.finish(125);
                return Ok(());
            }
            let callback = self.options.lock().as_ref().unwrap().on_data.clone();
            callback(b"bootstrap prompt\nechoed runner".to_vec());
            if !self.emit_marker.load(Ordering::Acquire) {
                return Ok(());
            }
            let mut requested = marker;
            requested.extend_from_slice(b"requested prompt$ ");
            callback(requested);
            for suffix in ["environment", "argv", "output-marker", "runner.bash"] {
                let path = self
                    .files
                    .values
                    .lock()
                    .keys()
                    .find(|path| path.ends_with(suffix))
                    .cloned();
                if let Some(path) = path {
                    self.files.values.lock().remove(&path);
                }
            }
            return Ok(());
        }
        if self.block_writes.load(Ordering::Acquire)
            && let Some(signal) = signal
        {
            signal.cancelled().await;
            anyhow::bail!("write aborted")
        }
        Ok(())
    }
}

#[derive(Debug)]
struct FakeSandbox {
    files: Arc<FakeFiles>,
    commands: Arc<FakeCommands>,
    pty: Arc<FakePty>,
}

#[async_trait::async_trait]
impl E2bSandbox for FakeSandbox {
    fn sandbox_id(&self) -> &'static str {
        "fake"
    }

    fn files(&self) -> Arc<dyn E2bFiles> {
        self.files.clone()
    }

    fn commands(&self) -> Arc<dyn E2bCommands> {
        self.commands.clone()
    }

    fn pty(&self) -> Option<Arc<dyn E2bPty>> {
        Some(self.pty.clone())
    }

    async fn kill(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

struct Harness {
    runtime: Arc<E2bService>,
    files: Arc<FakeFiles>,
    commands: Arc<FakeCommands>,
    pty: Arc<FakePty>,
    handle: Arc<FakeHandle>,
}

impl Harness {
    fn new(pid: i64) -> Self {
        let files = Arc::new(FakeFiles::default());
        let handle = FakeHandle::new(pid);
        let commands = Arc::new(FakeCommands {
            handle: handle.clone(),
            commands: Mutex::new(Vec::new()),
            groups: Mutex::new(vec![pid, 6000]),
            foreground: AtomicI64::new(6000),
        });
        let pty = Arc::new(FakePty {
            files: files.clone(),
            handle: handle.clone(),
            options: Mutex::new(None),
            inputs: Mutex::new(Vec::new()),
            abort_on_create: Mutex::new(None),
            exit_before_marker: AtomicBool::new(false),
            emit_marker: AtomicBool::new(true),
            block_writes: AtomicBool::new(false),
        });
        let sandbox: Arc<dyn E2bSandbox> = Arc::new(FakeSandbox {
            files: files.clone(),
            commands: commands.clone(),
            pty: pty.clone(),
        });
        let runtime = E2bService::new(
            "/workspace",
            Arc::new(move || {
                let sandbox = sandbox.clone();
                Box::pin(async move { Ok(sandbox) })
            }),
        );
        Self {
            runtime,
            files,
            commands,
            pty,
            handle,
        }
    }

    fn spec() -> SubprocessTerminalSpawnSpec {
        SubprocessTerminalSpawnSpec {
            argv: vec!["bash".to_owned(), "--noprofile".to_owned()],
            cwd: "/workspace".into(),
            env: Some(BTreeMap::from([(
                "EXPLICIT".to_owned(),
                "value".to_owned(),
            )])),
            rows: 24,
            cols: 80,
            grace_ms: 20.0,
            signal: None,
        }
    }

    async fn spawn(
        &self,
        spec: SubprocessTerminalSpawnSpec,
    ) -> Arc<seekdeep_subprocess_e2b::terminal::E2bTerminalHandle> {
        spawn_e2b_terminal(
            self.runtime.clone(),
            spec,
            "/workspace/.seekdeep-e2b/terminals/test",
            1,
        )
        .await
        .unwrap()
    }
}

#[tokio::test]
async fn hides_bootstrap_output_and_preserves_requested_process_bytes() {
    let harness = Harness::new(4242);
    let terminal = harness.spawn(Harness::spec()).await;
    assert_eq!(terminal.pid().as_i64(), 4242);
    let output = terminal.output();
    let read = tokio::spawn(async move {
        let mut output = output.lock().await;
        let mut prefix = vec![0; "requested prompt$ ".len()];
        output.read_exact(&mut prefix).await.unwrap();
        prefix
    });
    assert_eq!(read.await.unwrap(), b"requested prompt$ ");
    {
        let options = harness.pty.options.lock();
        assert_eq!(options.as_ref().unwrap().rows, 24);
        assert_eq!(options.as_ref().unwrap().cols, 80);
        assert_eq!(options.as_ref().unwrap().env["API_KEY"], "");
        assert_eq!(options.as_ref().unwrap().env["SEEKDEEP_OLD"], "");
    }
    terminal.write("hello").await.unwrap();
    assert_eq!(harness.pty.inputs.lock().last().unwrap(), b"hello");
    let foreground = terminal.inspect_foreground().await.unwrap().unwrap();
    assert_eq!(foreground.process_group_id.as_i64(), 6000);
    assert!(!foreground.input_waiting);
    assert_eq!(
        terminal
            .signal_foreground(SubprocessTerminalSignal::Sigint)
            .await
            .unwrap()
            .as_i64(),
        6000
    );
    assert!(
        harness
            .commands
            .commands
            .lock()
            .iter()
            .any(|command| command == "kill -INT -- -6000")
    );
    harness.handle.finish(0);
    assert_eq!(terminal.done().await.unwrap().exit_code, Some(0));
}

#[tokio::test]
async fn rejects_killing_the_terminal_shell_and_owns_session_cleanup() {
    let harness = Harness::new(4242);
    let terminal = harness.spawn(Harness::spec()).await;
    harness.commands.foreground.store(4242, Ordering::Release);
    assert!(
        terminal
            .signal_foreground(SubprocessTerminalSignal::Sigkill)
            .await
            .unwrap_err()
            .to_string()
            .contains("refusing to SIGKILL")
    );
    terminal.terminate().await.unwrap();
    terminal.terminate().await.unwrap();
    assert!(harness.commands.groups.lock().is_empty());
    assert_eq!(
        terminal.done().await.unwrap().signal.unwrap().as_str(),
        "SIGTERM"
    );
    assert!(
        harness
            .files
            .removed
            .lock()
            .iter()
            .any(|path| path.ends_with("/terminals/test"))
    );
}

#[tokio::test]
async fn allocation_cancellation_and_missing_output_boundary_roll_back_the_handle() {
    let canceled = Harness::new(4242);
    let signal = AbortSignal::default();
    *canceled.pty.abort_on_create.lock() = Some(signal.clone());
    let mut spec = Harness::spec();
    spec.signal = Some(signal);
    assert!(
        spawn_e2b_terminal(
            canceled.runtime.clone(),
            spec,
            "/workspace/.seekdeep-e2b/terminals/test",
            1,
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("aborted")
    );
    assert!(
        canceled.handle.kills.load(Ordering::Acquire) > 0
            || canceled.commands.groups.lock().is_empty()
    );
    assert!(
        canceled
            .files
            .removed
            .lock()
            .iter()
            .any(|path| path.ends_with("/terminals/test"))
    );

    let boundary = Harness::new(4242);
    boundary
        .pty
        .exit_before_marker
        .store(true, Ordering::Release);
    assert!(
        spawn_e2b_terminal(
            boundary.runtime.clone(),
            Harness::spec(),
            "/workspace/.seekdeep-e2b/terminals/test",
            1,
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("output boundary")
    );
}

#[tokio::test]
async fn termination_aborts_and_joins_an_in_flight_write() {
    let harness = Harness::new(4242);
    let terminal = harness.spawn(Harness::spec()).await;
    harness.pty.block_writes.store(true, Ordering::Release);
    let writer = terminal.clone();
    let write = tokio::spawn(async move { writer.write("blocked").await });
    while harness
        .pty
        .inputs
        .lock()
        .last()
        .is_none_or(|data| data != b"blocked")
    {
        tokio::task::yield_now().await;
    }
    terminal.terminate().await.unwrap();
    assert!(
        write
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("aborted")
    );
}

#[tokio::test]
async fn invalid_terminal_requests_fail_before_or_during_owned_allocation() {
    let harness = Harness::new(4242);
    let mut nul = Harness::spec();
    nul.argv = vec!["bad\0argv".to_owned()];
    assert!(
        spawn_e2b_terminal(
            harness.runtime.clone(),
            nul,
            "/workspace/.seekdeep-e2b/terminals/nul",
            1,
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("must not contain NUL")
    );
    assert!(harness.pty.options.lock().is_none());

    let invalid_pid = Harness::new(0);
    assert!(
        spawn_e2b_terminal(
            invalid_pid.runtime.clone(),
            Harness::spec(),
            "/workspace/.seekdeep-e2b/terminals/test",
            1,
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("invalid terminal pid")
    );
    assert!(invalid_pid.handle.kills.load(Ordering::Acquire) > 0);
}

#[tokio::test]
async fn runtime_disposal_owns_published_and_unpublished_terminal_sessions() {
    let published = Harness::new(4242);
    let context = Context::new();
    context.provide(E2B, published.runtime.clone()).unwrap();
    let runtime = E2bSubprocessRuntime::install(&context, E2bSubprocessConfig::default()).unwrap();
    let terminal = runtime.spawn_terminal(Harness::spec()).await.unwrap();
    assert_eq!(runtime.live_terminal_count(), 1);
    context.root_fiber().dispose().await.unwrap();
    assert_eq!(runtime.live_terminal_count(), 0);
    assert_eq!(
        terminal.done().await.unwrap().signal.unwrap().as_str(),
        "SIGTERM"
    );

    let pending = Harness::new(4242);
    pending.pty.emit_marker.store(false, Ordering::Release);
    let context = Context::new();
    context.provide(E2B, pending.runtime.clone()).unwrap();
    let runtime = E2bSubprocessRuntime::install(&context, E2bSubprocessConfig::default()).unwrap();
    let spawning = runtime.spawn_terminal(Harness::spec());
    tokio::pin!(spawning);
    loop {
        tokio::select! {
            result = &mut spawning => panic!("setup settled before disposal: {result:?}"),
            () = tokio::task::yield_now() => {
                if pending.pty.inputs.lock().iter().any(|input| input.starts_with(b"exec /bin/bash")) {
                    break;
                }
            }
        }
    }
    assert_eq!(runtime.terminal_setup_count(), 1);
    let root = context.root_fiber().clone();
    let disposal = tokio::spawn(async move { root.dispose().await });
    let setup = spawning.await;
    assert!(setup.unwrap_err().to_string().contains("aborted"));
    disposal.await.unwrap().unwrap();
    assert_eq!(runtime.terminal_setup_count(), 0);
    assert!(pending.commands.groups.lock().is_empty());
}
