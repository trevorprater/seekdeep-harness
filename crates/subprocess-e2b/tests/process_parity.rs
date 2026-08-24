//! Managed E2B command lifecycle parity over an object-safe fake SDK.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering},
    },
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use parking_lot::Mutex;
use seekdeep_e2b::{
    E2bCommandCompletion, E2bCommandExit, E2bCommandHandle, E2bCommandHandleRef, E2bCommandResult,
    E2bCommandStartOptions, E2bCommands, E2bEntryInfo, E2bFileType, E2bFiles, E2bSandbox,
    E2bService,
};
use seekdeep_llm::AbortSignal;
use seekdeep_subprocess::{
    SubprocessCollect, SubprocessEnvironment, SubprocessHandle as _, SubprocessOutputMode,
    SubprocessSpawnSpec, SubprocessSpill, SubprocessStdinMode, SubprocessStdio,
};
use seekdeep_subprocess_e2b::{output::E2B_OUTPUT_COMPLETE_FRAME, process::E2bSubprocessHandle};
use tokio::io::AsyncReadExt as _;

#[derive(Clone, Debug)]
enum Completion {
    Pending,
    Result(anyhow::Result<E2bCommandCompletion, Arc<str>>),
}

#[derive(Debug)]
struct FakeCommandHandle {
    pid: AtomicI64,
    completion: tokio::sync::watch::Sender<Completion>,
    options: Mutex<Option<E2bCommandStartOptions>>,
    stdin: Mutex<Vec<Vec<u8>>>,
    closes: AtomicUsize,
    close_error: Mutex<Option<String>>,
    kills: AtomicUsize,
    disconnects: AtomicUsize,
    alive: Arc<AtomicBool>,
}

impl FakeCommandHandle {
    fn new(pid: i64, alive: Arc<AtomicBool>) -> Arc<Self> {
        let (completion, _) = tokio::sync::watch::channel(Completion::Pending);
        Arc::new(Self {
            pid: AtomicI64::new(pid),
            completion,
            options: Mutex::new(None),
            stdin: Mutex::new(Vec::new()),
            closes: AtomicUsize::new(0),
            close_error: Mutex::new(None),
            kills: AtomicUsize::new(0),
            disconnects: AtomicUsize::new(0),
            alive,
        })
    }

    async fn output(&self, stdout: bool, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let callback = self.options.lock().as_ref().map(|options| {
            if stdout {
                options.on_stdout.clone()
            } else {
                options.on_stderr.clone()
            }
        });
        if let Some(callback) = callback {
            callback(format!("{}\n", STANDARD.encode(bytes)))
                .await
                .unwrap();
        }
    }

    async fn wire(&self, stdout: bool, text: &str) {
        let callback = self.options.lock().as_ref().map(|options| {
            if stdout {
                options.on_stdout.clone()
            } else {
                options.on_stderr.clone()
            }
        });
        if let Some(callback) = callback {
            callback(text.to_owned()).await.unwrap();
        }
    }

    async fn finish(&self, exit_code: i32) {
        self.alive.store(false, Ordering::Release);
        self.wire(true, &format!("{E2B_OUTPUT_COMPLETE_FRAME}\n"))
            .await;
        self.wire(false, &format!("{E2B_OUTPUT_COMPLETE_FRAME}\n"))
            .await;
        if exit_code == 0 {
            self.completion
                .send_replace(Completion::Result(Ok(E2bCommandCompletion { exit_code })));
        } else {
            self.completion
                .send_replace(Completion::Result(Err(Arc::<str>::from(format!(
                    "COMMAND_EXIT:{exit_code}"
                )))));
        }
    }
}

#[async_trait::async_trait]
impl E2bCommandHandle for FakeCommandHandle {
    fn pid(&self) -> i64 {
        self.pid.load(Ordering::Acquire)
    }

    async fn wait(&self) -> anyhow::Result<E2bCommandCompletion> {
        let mut completion = self.completion.subscribe();
        loop {
            match completion.borrow().clone() {
                Completion::Pending => {}
                Completion::Result(Ok(result)) => return Ok(result),
                Completion::Result(Err(error)) => {
                    let text = error.to_string();
                    if let Some(status) = text.strip_prefix("COMMAND_EXIT:") {
                        return Err(E2bCommandExit {
                            status: status.parse().unwrap(),
                            stderr: String::new(),
                        }
                        .into());
                    }
                    anyhow::bail!(text);
                }
            }
            completion.changed().await.unwrap();
        }
    }

    async fn send_stdin(&self, data: Vec<u8>) -> anyhow::Result<()> {
        self.stdin.lock().push(data);
        Ok(())
    }

    async fn close_stdin(&self) -> anyhow::Result<()> {
        self.closes.fetch_add(1, Ordering::AcqRel);
        if let Some(error) = self.close_error.lock().take() {
            anyhow::bail!(error);
        }
        Ok(())
    }

    async fn kill(&self) -> anyhow::Result<bool> {
        self.kills.fetch_add(1, Ordering::AcqRel);
        if self.alive.swap(false, Ordering::AcqRel) {
            self.finish(137).await;
        }
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
    ) -> anyhow::Result<seekdeep_e2b::E2bByteStream> {
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
    files: Arc<FakeFiles>,
    handle: Arc<FakeCommandHandle>,
    started: tokio::sync::Notify,
    commands: Mutex<Vec<String>>,
    start_options: Mutex<Option<E2bCommandStartOptions>>,
    alive: Arc<AtomicBool>,
    probe_error: Mutex<Option<String>>,
}

impl FakeCommands {
    async fn wait_started(&self) {
        loop {
            let notified = self.started.notified();
            if self.start_options.lock().is_some() {
                return;
            }
            notified.await;
        }
    }

    async fn settle(&self, exit_code: i32, stdout: &[u8], stderr: &[u8]) {
        self.handle.output(true, stdout).await;
        self.handle.output(false, stderr).await;
        self.files.values.lock().insert(
            "/workspace/.seekdeep-e2b/processes/test/exit-code".to_owned(),
            format!("{exit_code}\n").into_bytes(),
        );
        self.handle.finish(exit_code).await;
    }

    fn publish_status(&self, exit_code: i32) {
        self.files.values.lock().insert(
            "/workspace/.seekdeep-e2b/processes/test/exit-code".to_owned(),
            format!("{exit_code}\n").into_bytes(),
        );
    }

    fn settle_without_output(&self, exit_code: i32) {
        self.handle.alive.store(false, Ordering::Release);
        self.publish_status(exit_code);
        if exit_code == 0 {
            self.handle
                .completion
                .send_replace(Completion::Result(Ok(E2bCommandCompletion { exit_code })));
        } else {
            self.handle
                .completion
                .send_replace(Completion::Result(Err(Arc::<str>::from(format!(
                    "COMMAND_EXIT:{exit_code}"
                )))));
        }
    }
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
            let raw = "PATH=/ambient/bin\0KEEP=safe\0NPM_TOKEN=secret\0SEEKDEEP_STALE=old\0";
            return Ok(E2bCommandResult {
                stdout: format!(
                    "{}\n{}",
                    STANDARD.encode("/home/user"),
                    STANDARD.encode(raw)
                ),
                stderr: String::new(),
            });
        }
        if command.starts_with("set -o pipefail; ps -eo") {
            if let Some(error) = self.probe_error.lock().take() {
                anyhow::bail!(error);
            }
            return Ok(E2bCommandResult {
                stdout: if self.alive.load(Ordering::Acquire) {
                    "live\n".to_owned()
                } else {
                    String::new()
                },
                stderr: String::new(),
            });
        }
        if command.starts_with("kill -TERM ") || command.starts_with("kill -KILL ") {
            self.alive.store(false, Ordering::Release);
            self.handle
                .finish(if command.contains("TERM") { 143 } else { 137 })
                .await;
        }
        Ok(E2bCommandResult::default())
    }

    async fn start(
        &self,
        command: &str,
        options: E2bCommandStartOptions,
    ) -> anyhow::Result<E2bCommandHandleRef> {
        self.commands.lock().push(command.to_owned());
        self.files.values.lock().insert(
            "/workspace/.seekdeep-e2b/processes/test/pid".to_owned(),
            b"4242\n".to_vec(),
        );
        self.files
            .values
            .lock()
            .remove("/workspace/.seekdeep-e2b/processes/test/environment");
        *self.handle.options.lock() = Some(options.clone());
        *self.start_options.lock() = Some(options);
        self.started.notify_waiters();
        Ok(self.handle.clone())
    }
}

#[derive(Debug)]
struct FakeSandbox {
    files: Arc<FakeFiles>,
    commands: Arc<FakeCommands>,
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

    async fn kill(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

struct Harness {
    runtime: Arc<E2bService>,
    sandbox: Arc<dyn E2bSandbox>,
    files: Arc<FakeFiles>,
    commands: Arc<FakeCommands>,
    handle: Arc<FakeCommandHandle>,
}

impl Harness {
    fn new(pid: i64) -> Self {
        let files = Arc::new(FakeFiles::default());
        let alive = Arc::new(AtomicBool::new(true));
        let handle = FakeCommandHandle::new(pid, alive.clone());
        let commands = Arc::new(FakeCommands {
            files: files.clone(),
            handle: handle.clone(),
            started: tokio::sync::Notify::new(),
            commands: Mutex::new(Vec::new()),
            start_options: Mutex::new(None),
            alive,
            probe_error: Mutex::new(None),
        });
        let sandbox: Arc<dyn E2bSandbox> = Arc::new(FakeSandbox {
            files: files.clone(),
            commands: commands.clone(),
        });
        let runtime_sandbox = sandbox.clone();
        let runtime = E2bService::new(
            "/workspace",
            Arc::new(move || {
                let sandbox = runtime_sandbox.clone();
                Box::pin(async move { Ok(sandbox) })
            }),
        );
        Self {
            runtime,
            sandbox,
            files,
            commands,
            handle,
        }
    }

    fn spawn(&self, spec: SubprocessSpawnSpec) -> Arc<E2bSubprocessHandle> {
        E2bSubprocessHandle::spawn(
            self.runtime.clone(),
            spec,
            "/workspace/.seekdeep-e2b/processes/test",
            1,
        )
        .unwrap()
    }
}

fn spec(stdout: SubprocessOutputMode, stdin: SubprocessStdinMode) -> SubprocessSpawnSpec {
    SubprocessSpawnSpec {
        argv: vec!["bash".to_owned(), "-c".to_owned(), "printf ok".to_owned()],
        cwd: "/workspace".into(),
        stdio: SubprocessStdio {
            stdin,
            stdout,
            stderr: SubprocessOutputMode::Collect(SubprocessCollect {
                max_bytes: 8.0,
                spill: None,
            }),
        },
        grace_ms: 20.0,
        signal: None,
        env: Some(SubprocessEnvironment::from([
            ("EXPLICIT".to_owned(), Some("opted-in".to_owned())),
            ("NPM_TOKEN".to_owned(), Some("explicit-secret".to_owned())),
        ])),
    }
}

#[tokio::test]
async fn starts_asynchronously_hides_secrets_and_supports_deferred_piped_io() {
    let harness = Harness::new(4242);
    let process = harness.spawn(spec(SubprocessOutputMode::Pipe, SubprocessStdinMode::Pipe));
    assert_eq!(process.pid().as_i64(), -1);
    let stdin = process.stdin().unwrap();
    let write = tokio::spawn(async move {
        stdin.write_all(b"input").await.unwrap();
        stdin.close().await.unwrap();
    });
    harness.commands.wait_started().await;
    write.await.unwrap();
    assert_eq!(harness.handle.stdin.lock().as_slice(), &[b"input".to_vec()]);
    assert_eq!(harness.handle.closes.load(Ordering::Acquire), 1);
    let command = harness.commands.commands.lock().last().unwrap().clone();
    assert!(command.contains("setsid"));
    assert!(command.contains("environment"));
    assert!(!command.contains("explicit-secret"));
    {
        let options = harness.commands.start_options.lock();
        assert_eq!(options.as_ref().unwrap().env["NPM_TOKEN"], "");
        assert_eq!(options.as_ref().unwrap().env["SEEKDEEP_STALE"], "");
        assert!(options.as_ref().unwrap().env["HOME"].starts_with("/.seekdeep-e2b-control-"));
    }
    harness
        .commands
        .settle(0, "A你好B".as_bytes(), b"warn")
        .await;
    assert_eq!(process.done().await.unwrap().exit_code, Some(0));
    assert_eq!(process.pid().as_i64(), 4242);
    let stdout = process.stdout().unwrap();
    let mut stdout = stdout.lock().await;
    let mut bytes = Vec::new();
    stdout.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(String::from_utf8(bytes).unwrap(), "A你好B");
    assert_eq!(
        process.collected().stderr.unwrap().read_from(0).text,
        "warn"
    );
}

#[tokio::test]
async fn collected_tail_retains_an_in_cap_remote_spill_and_maps_nonzero_exit() {
    let harness = Harness::new(4242);
    let process = harness.spawn(spec(
        SubprocessOutputMode::Collect(SubprocessCollect {
            max_bytes: 2.0,
            spill: Some(SubprocessSpill { max_bytes: 10.0 }),
        }),
        SubprocessStdinMode::Ignore,
    ));
    harness.commands.wait_started().await;
    harness.commands.settle(7, b"abcd", b"").await;
    assert_eq!(process.done().await.unwrap().exit_code, Some(7));
    let output = process.collected().stdout.unwrap().read_from(0);
    assert_eq!(output.text, "cd");
    assert!(output.lossy);
    assert_eq!(
        output.spill_path.unwrap(),
        std::path::PathBuf::from("/workspace/.seekdeep-e2b/processes/test/stdout.log")
    );
}

#[tokio::test]
async fn termination_signals_the_remote_group_and_waits_for_quiescence() {
    let harness = Harness::new(4242);
    let process = harness.spawn(spec(
        SubprocessOutputMode::Pipe,
        SubprocessStdinMode::Ignore,
    ));
    harness.commands.wait_started().await;
    while process.pid().as_i64() <= 0 {
        tokio::task::yield_now().await;
    }
    process.terminate();
    assert!(process.wait_for_exit(None).await.unwrap());
    assert_eq!(
        process.done().await.unwrap().signal.unwrap().as_str(),
        "SIGTERM"
    );
    assert!(
        harness
            .commands
            .commands
            .lock()
            .iter()
            .any(|command| command == "kill -TERM -- -4242")
    );
}

#[tokio::test]
async fn invalid_sdk_pid_is_killed_and_private_state_is_removed() {
    let harness = Harness::new(0);
    let process = harness.spawn(spec(
        SubprocessOutputMode::Collect(SubprocessCollect {
            max_bytes: 4.0,
            spill: None,
        }),
        SubprocessStdinMode::Ignore,
    ));
    assert!(
        process
            .done()
            .await
            .unwrap_err()
            .to_string()
            .contains("invalid command pid")
    );
    assert_eq!(harness.handle.kills.load(Ordering::Acquire), 1);
    let removed = harness.files.removed.lock();
    assert!(removed.iter().any(|path| path.ends_with("/environment")));
    assert!(removed.iter().any(|path| path.ends_with("/test")));
}

#[tokio::test]
async fn malformed_output_and_liveness_transport_failures_remain_observable() {
    let harness = Harness::new(4242);
    let process = harness.spawn(spec(
        SubprocessOutputMode::Pipe,
        SubprocessStdinMode::Ignore,
    ));
    harness.commands.wait_started().await;
    harness.handle.wire(true, "%\n").await;
    harness.commands.settle(0, b"", b"").await;
    assert!(
        process
            .done()
            .await
            .unwrap_err()
            .to_string()
            .contains("invalid base64")
    );

    let probe = Harness::new(4242);
    let process = probe.spawn(spec(
        SubprocessOutputMode::Pipe,
        SubprocessStdinMode::Ignore,
    ));
    probe.commands.wait_started().await;
    while process.pid().as_i64() <= 0 {
        tokio::task::yield_now().await;
    }
    *probe.commands.probe_error.lock() = Some("probe transport failed".to_owned());
    assert!(
        process
            .wait_for_exit(None)
            .await
            .unwrap_err()
            .to_string()
            .contains("probe transport failed")
    );
}

#[tokio::test]
async fn caller_abort_bounds_a_stalled_liveness_reconnection() {
    let harness = Harness::new(4242);
    let calls = Arc::new(AtomicUsize::new(0));
    let gate = Arc::new(tokio::sync::Notify::new());
    let sandbox = harness.sandbox.clone();
    let runtime = E2bService::new(
        "/workspace",
        Arc::new({
            let calls = calls.clone();
            let gate = gate.clone();
            move || {
                let call = calls.fetch_add(1, Ordering::AcqRel);
                let gate = gate.clone();
                let sandbox = sandbox.clone();
                Box::pin(async move {
                    if call > 0 {
                        gate.notified().await;
                    }
                    Ok(sandbox)
                })
            }
        }),
    );
    let process = E2bSubprocessHandle::spawn(
        runtime,
        spec(SubprocessOutputMode::Pipe, SubprocessStdinMode::Ignore),
        "/workspace/.seekdeep-e2b/processes/test",
        1,
    )
    .unwrap();
    harness.commands.wait_started().await;
    while process.pid().as_i64() <= 0 {
        tokio::task::yield_now().await;
    }
    let signal = AbortSignal::default();
    let waiting = process.wait_for_exit(Some(signal.clone()));
    tokio::pin!(waiting);
    tokio::task::yield_now().await;
    signal.abort();
    assert!(
        !tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("bounded liveness wait")
            .unwrap()
    );
    gate.notify_waiters();
    harness.commands.settle(0, b"", b"").await;
    process.done().await.unwrap();
}

#[tokio::test]
async fn deferred_stdin_close_surfaces_the_provider_eof_failure() {
    let harness = Harness::new(4242);
    let process = harness.spawn(spec(SubprocessOutputMode::Pipe, SubprocessStdinMode::Pipe));
    harness.commands.wait_started().await;
    *harness.handle.close_error.lock() = Some("remote close failed".to_owned());
    assert!(
        process
            .stdin()
            .unwrap()
            .close()
            .await
            .unwrap_err()
            .to_string()
            .contains("remote close failed")
    );
    harness.commands.settle(0, b"", b"").await;
    process.done().await.unwrap();
}

#[tokio::test]
async fn natural_completion_requires_transport_frames_and_bounded_drain_disconnects() {
    let incomplete = Harness::new(4242);
    let process = incomplete.spawn(spec(
        SubprocessOutputMode::Pipe,
        SubprocessStdinMode::Ignore,
    ));
    incomplete.commands.wait_started().await;
    incomplete.commands.settle_without_output(0);
    assert!(
        process
            .done()
            .await
            .unwrap_err()
            .to_string()
            .contains("incomplete output transport")
    );

    let draining = Harness::new(4242);
    let mut request = spec(
        SubprocessOutputMode::Collect(SubprocessCollect {
            max_bytes: 2.0,
            spill: Some(SubprocessSpill { max_bytes: 10.0 }),
        }),
        SubprocessStdinMode::Ignore,
    );
    request.grace_ms = 1.0;
    let process = draining.spawn(request);
    draining.commands.wait_started().await;
    draining.commands.publish_status(0);
    assert_eq!(process.done().await.unwrap().exit_code, Some(0));
    assert_eq!(draining.handle.disconnects.load(Ordering::Acquire), 1);
}
