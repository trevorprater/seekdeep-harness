//! Loader-facing E2B subprocess runtime and lifecycle ownership parity.

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_e2b::{
    E2B, E2bByteStream, E2bCommandCompletion, E2bCommandExit, E2bCommandHandle,
    E2bCommandHandleRef, E2bCommandResult, E2bCommandStartOptions, E2bCommands, E2bEntryInfo,
    E2bFileType, E2bFiles, E2bSandbox, E2bService,
};
use seekdeep_llm::AbortSignal;
use seekdeep_subprocess::{
    SubprocessCollect, SubprocessOutputMode, SubprocessRuntime as _, SubprocessSpawnSpec,
    SubprocessStdinMode, SubprocessStdio,
};
use seekdeep_subprocess_e2b::{E2bSubprocessConfig, E2bSubprocessRuntime, INJECT, NAME, plugin};

#[derive(Clone, Copy, Debug)]
enum Completion {
    Pending,
    Exit(i32),
}

#[derive(Debug)]
struct FakeHandle {
    completion: tokio::sync::watch::Sender<Completion>,
    kills: AtomicUsize,
    alive: Arc<AtomicBool>,
}

impl FakeHandle {
    fn new(alive: Arc<AtomicBool>) -> Arc<Self> {
        let (completion, _) = tokio::sync::watch::channel(Completion::Pending);
        Arc::new(Self {
            completion,
            kills: AtomicUsize::new(0),
            alive,
        })
    }

    fn finish(&self, status: i32) {
        self.alive.store(false, Ordering::Release);
        self.completion.send_replace(Completion::Exit(status));
    }
}

#[async_trait::async_trait]
impl E2bCommandHandle for FakeHandle {
    fn pid(&self) -> i64 {
        4242
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
        Ok(())
    }
}

#[derive(Debug, Default)]
struct FakeFiles {
    values: Mutex<BTreeMap<String, Vec<u8>>>,
}

#[async_trait::async_trait]
impl E2bFiles for FakeFiles {
    async fn get_info(
        &self,
        path: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bEntryInfo> {
        Ok(E2bEntryInfo {
            name: path.to_owned(),
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
        Ok(())
    }
}

#[derive(Debug)]
struct FakeCommands {
    files: Arc<FakeFiles>,
    handle: Arc<FakeHandle>,
    alive: Arc<AtomicBool>,
    resolved: Mutex<String>,
    run_in_cwds: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl E2bCommands for FakeCommands {
    async fn run(
        &self,
        command: &str,
        _env: BTreeMap<String, String>,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bCommandResult> {
        if command.contains("env -0 | base64") {
            return Ok(E2bCommandResult {
                stdout: format!(
                    "{}\n{}",
                    STANDARD.encode("/home/user"),
                    STANDARD.encode("PATH=/bin\0")
                ),
                stderr: String::new(),
            });
        }
        if command.starts_with("set -o pipefail; ps -eo pgid=") {
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
            self.handle
                .finish(if command.contains("TERM") { 143 } else { 137 });
        }
        Ok(E2bCommandResult::default())
    }

    async fn run_in(
        &self,
        _command: &str,
        cwd: &str,
        _env: BTreeMap<String, String>,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<E2bCommandResult> {
        self.run_in_cwds.lock().push(cwd.to_owned());
        Ok(E2bCommandResult {
            stdout: self.resolved.lock().clone(),
            stderr: String::new(),
        })
    }

    async fn start(
        &self,
        _command: &str,
        _options: E2bCommandStartOptions,
    ) -> anyhow::Result<E2bCommandHandleRef> {
        let pid = self
            .files
            .values
            .lock()
            .keys()
            .find(|path| path.ends_with("/pid"))
            .cloned()
            .expect("pid file");
        self.files.values.lock().insert(pid, b"4242\n".to_vec());
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

fn harness() -> (Arc<E2bService>, Arc<FakeCommands>, Arc<FakeHandle>) {
    let files = Arc::new(FakeFiles::default());
    let alive = Arc::new(AtomicBool::new(true));
    let handle = FakeHandle::new(alive.clone());
    let commands = Arc::new(FakeCommands {
        files: files.clone(),
        handle: handle.clone(),
        alive,
        resolved: Mutex::new("bin/tool\n".to_owned()),
        run_in_cwds: Mutex::new(Vec::new()),
    });
    let sandbox: Arc<dyn E2bSandbox> = Arc::new(FakeSandbox {
        files,
        commands: commands.clone(),
    });
    let service = E2bService::new(
        "/workspace",
        Arc::new(move || {
            let sandbox = sandbox.clone();
            Box::pin(async move { Ok(sandbox) })
        }),
    );
    (service, commands, handle)
}

fn spec() -> SubprocessSpawnSpec {
    SubprocessSpawnSpec {
        argv: vec!["bash".to_owned()],
        cwd: "/workspace".into(),
        stdio: SubprocessStdio {
            stdin: SubprocessStdinMode::Ignore,
            stdout: SubprocessOutputMode::Collect(SubprocessCollect {
                max_bytes: 4.0,
                spill: None,
            }),
            stderr: SubprocessOutputMode::Collect(SubprocessCollect {
                max_bytes: 4.0,
                spill: None,
            }),
        },
        grace_ms: 10.0,
        signal: None,
        env: None,
    }
}

#[tokio::test]
async fn resolves_executables_and_rejects_invalid_inputs() {
    let (service, commands, _) = harness();
    let runtime = E2bSubprocessRuntime::new(service, E2bSubprocessConfig::default()).unwrap();
    assert_eq!(
        runtime
            .resolve_executable("tool", None, None)
            .await
            .unwrap(),
        "/workspace/bin/tool"
    );
    assert_eq!(commands.run_in_cwds.lock().as_slice(), &["/workspace"]);
    assert_eq!(
        runtime
            .resolve_executable("/usr/bin/tool", None, None)
            .await
            .unwrap(),
        "/usr/bin/tool"
    );
    assert!(runtime.resolve_executable("", None, None).await.is_err());
    assert!(
        runtime
            .resolve_executable("relative/tool", None, None)
            .await
            .unwrap_err()
            .to_string()
            .contains("relative path")
    );
    *commands.resolved.lock() = "two\npaths\n".to_owned();
    assert!(
        runtime
            .resolve_executable("tool", None, None)
            .await
            .unwrap_err()
            .to_string()
            .contains("did not resolve")
    );
}

#[tokio::test]
async fn plugin_shape_config_and_sync_spawn_validation_are_fail_loud() {
    assert_eq!(plugin().name(), NAME);
    assert_eq!(plugin().inject(), INJECT);
    let (service, _, _) = harness();
    assert!(
        E2bSubprocessRuntime::new(service.clone(), E2bSubprocessConfig { poll_ms: 0 })
            .unwrap_err()
            .to_string()
            .contains("pollMs")
    );
    let runtime = E2bSubprocessRuntime::new(service, E2bSubprocessConfig::default()).unwrap();
    let mut invalid = spec();
    invalid.argv.clear();
    assert!(
        runtime
            .spawn(invalid)
            .unwrap_err()
            .to_string()
            .contains("argv")
    );
    let mut invalid = spec();
    invalid.grace_ms = 0.0;
    assert!(
        runtime
            .spawn(invalid)
            .unwrap_err()
            .to_string()
            .contains("graceMs")
    );
}

#[tokio::test]
async fn cordis_disposal_terminates_and_joins_owned_remote_processes() {
    let (service, _, handle) = harness();
    let context = Context::new();
    context.provide(E2B, service).unwrap();
    let runtime = E2bSubprocessRuntime::install(&context, E2bSubprocessConfig::default()).unwrap();
    let process = runtime.spawn(spec()).unwrap();
    while process.pid().as_i64() <= 0 {
        tokio::task::yield_now().await;
    }
    context.root_fiber().dispose().await.unwrap();
    assert_eq!(runtime.live_process_count(), 0);
    assert!(handle.kills.load(Ordering::Acquire) > 0 || !handle.alive.load(Ordering::Acquire));
}
