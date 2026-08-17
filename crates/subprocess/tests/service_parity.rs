//! Abstract service, scrub, vocabulary, and invariant parity.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use seekdeep_cordis::Context;
use seekdeep_llm::AbortSignal;
use seekdeep_subprocess::{
    ProcessGroupId, ProcessId, ProcessSignal, SUBPROCESS, SeekDeepEnvironmentKey,
    SubprocessCollect, SubprocessCollectedOutputs, SubprocessHandle, SubprocessHandleRef,
    SubprocessInput, SubprocessLookupEnvironment, SubprocessOutcome, SubprocessOutput,
    SubprocessOutputMode, SubprocessOutputRead, SubprocessOutputReader, SubprocessRuntime,
    SubprocessService, SubprocessSpawnSpec, SubprocessStdinMode, SubprocessStdio,
    SubprocessTerminalForeground, SubprocessTerminalHandle, SubprocessTerminalHandleRef,
    SubprocessTerminalSignal, SubprocessTerminalSpawnSpec, scrub_environment,
};

#[derive(Debug)]
struct EmptyReader;

impl SubprocessOutputReader for EmptyReader {
    fn read_from(&self, _from_byte: u64) -> SubprocessOutputRead {
        SubprocessOutputRead::default()
    }
}

#[derive(Debug)]
struct StubHandle {
    pid: i64,
    collected: SubprocessCollectedOutputs,
    terminated: AtomicBool,
}

#[async_trait]
impl SubprocessHandle for StubHandle {
    fn pid(&self) -> ProcessId {
        ProcessId::new(self.pid)
    }

    fn stdin(&self) -> Option<SubprocessInput> {
        None
    }

    fn stdout(&self) -> Option<SubprocessOutput> {
        None
    }

    fn stderr(&self) -> Option<SubprocessOutput> {
        None
    }

    fn collected(&self) -> SubprocessCollectedOutputs {
        self.collected.clone()
    }

    async fn done(&self) -> anyhow::Result<SubprocessOutcome> {
        Ok(SubprocessOutcome {
            exit_code: Some(0),
            signal: None,
        })
    }

    fn terminate(&self) {
        self.terminated.store(true, Ordering::Release);
    }

    async fn wait_for_exit(&self, signal: Option<AbortSignal>) -> bool {
        !signal.is_some_and(|signal| signal.is_aborted())
    }
}

struct StubTerminal {
    pid: i64,
    output: SubprocessOutput,
    terminated: AtomicBool,
    writes: parking_lot::Mutex<Vec<String>>,
}

impl std::fmt::Debug for StubTerminal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StubTerminal")
            .field("pid", &self.pid)
            .field("terminated", &self.terminated)
            .field("writes", &self.writes)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl SubprocessTerminalHandle for StubTerminal {
    fn pid(&self) -> ProcessId {
        ProcessId::new(self.pid)
    }

    fn output(&self) -> SubprocessOutput {
        self.output.clone()
    }

    async fn done(&self) -> anyhow::Result<SubprocessOutcome> {
        Ok(SubprocessOutcome {
            exit_code: Some(0),
            signal: None,
        })
    }

    async fn write(&self, data: &str) -> anyhow::Result<()> {
        self.writes.lock().push(data.to_owned());
        Ok(())
    }

    async fn inspect_foreground(&self) -> anyhow::Result<Option<SubprocessTerminalForeground>> {
        Ok(Some(SubprocessTerminalForeground {
            process_group_id: ProcessGroupId::new(1),
            input_waiting: true,
        }))
    }

    async fn signal_foreground(
        &self,
        _signal: SubprocessTerminalSignal,
    ) -> anyhow::Result<ProcessGroupId> {
        Ok(ProcessGroupId::new(1))
    }

    async fn terminate(&self) -> anyhow::Result<()> {
        self.terminated.store(true, Ordering::Release);
        Ok(())
    }
}

#[derive(Debug)]
struct StubRuntime;

#[async_trait]
impl SubprocessRuntime for StubRuntime {
    async fn resolve_executable(
        &self,
        command: &str,
        _env: Option<&SubprocessLookupEnvironment>,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<String> {
        anyhow::ensure!(
            !signal.is_some_and(|signal| signal.is_aborted()),
            "lookup aborted"
        );
        Ok(format!("/bin/{command}"))
    }

    fn spawn(&self, spec: SubprocessSpawnSpec) -> anyhow::Result<SubprocessHandleRef> {
        let stdout = matches!(spec.stdio.stdout, SubprocessOutputMode::Collect(_))
            .then(|| Arc::new(EmptyReader) as Arc<dyn SubprocessOutputReader>);
        Ok(Arc::new(StubHandle {
            pid: i64::try_from(spec.argv.len()).expect("small argv"),
            collected: SubprocessCollectedOutputs {
                stdout,
                stderr: None,
            },
            terminated: AtomicBool::new(false),
        }))
    }

    async fn spawn_terminal(
        &self,
        spec: SubprocessTerminalSpawnSpec,
    ) -> anyhow::Result<SubprocessTerminalHandleRef> {
        let output: Pin<Box<dyn tokio::io::AsyncRead + Send + Unpin>> =
            Box::pin(tokio::io::empty());
        Ok(Arc::new(StubTerminal {
            pid: i64::try_from(spec.argv.len()).expect("small argv"),
            output: Arc::new(tokio::sync::Mutex::new(output)),
            terminated: AtomicBool::new(false),
            writes: parking_lot::Mutex::new(Vec::new()),
        }))
    }
}

fn ordinary_spec() -> SubprocessSpawnSpec {
    SubprocessSpawnSpec {
        argv: vec!["true".to_owned()],
        cwd: PathBuf::from("/stub"),
        stdio: SubprocessStdio {
            stdin: SubprocessStdinMode::Ignore,
            stdout: SubprocessOutputMode::Collect(SubprocessCollect {
                max_bytes: 1.0,
                spill: None,
            }),
            stderr: SubprocessOutputMode::Inherit,
        },
        grace_ms: 1.0,
        signal: None,
        env: None,
    }
}

#[tokio::test]
async fn concrete_provider_serves_the_complete_ordinary_process_seam() {
    let context = Context::new();
    let service = SubprocessService::new(Arc::new(StubRuntime));
    service.provide(&context).expect("provide");
    let runtime = context.get(SUBPROCESS).expect("service");
    assert_eq!(
        runtime
            .resolve_executable("true", None, None)
            .await
            .expect("resolve"),
        "/bin/true"
    );
    let handle = runtime.spawn(ordinary_spec()).expect("spawn");
    assert_eq!(handle.pid(), ProcessId::new(1));
    assert!(handle.stdin().is_none());
    assert!(handle.stdout().is_none());
    assert!(handle.stderr().is_none());
    assert_eq!(
        handle
            .collected()
            .stdout
            .expect("stdout reader")
            .read_from(0),
        SubprocessOutputRead::default()
    );
    handle.terminate();
    assert!(handle.wait_for_exit(None).await);
    assert_eq!(
        handle.done().await.expect("done"),
        SubprocessOutcome {
            exit_code: Some(0),
            signal: None,
        }
    );
}

#[tokio::test]
async fn terminal_primitive_preserves_all_foreground_and_lifecycle_operations() {
    let runtime = StubRuntime;
    let handle = runtime
        .spawn_terminal(SubprocessTerminalSpawnSpec {
            argv: vec!["bash".to_owned(), "-l".to_owned()],
            cwd: PathBuf::from("/stub"),
            env: Some(BTreeMap::from([("TERM".to_owned(), "xterm".to_owned())])),
            rows: 24,
            cols: 80,
            grace_ms: 1.0,
            signal: None,
        })
        .await
        .expect("terminal");
    assert_eq!(handle.pid(), ProcessId::new(2));
    drop(handle.output());
    handle.write("echo ready\n").await.expect("write");
    assert_eq!(
        handle.inspect_foreground().await.expect("inspect"),
        Some(SubprocessTerminalForeground {
            process_group_id: ProcessGroupId::new(1),
            input_waiting: true,
        })
    );
    for signal in [
        SubprocessTerminalSignal::Sigint,
        SubprocessTerminalSignal::Sigterm,
        SubprocessTerminalSignal::Sigkill,
        SubprocessTerminalSignal::Sigtstp,
        SubprocessTerminalSignal::Sighup,
    ] {
        assert_eq!(
            handle.signal_foreground(signal).await.expect("signal"),
            ProcessGroupId::new(1)
        );
    }
    handle.terminate().await.expect("terminate");
    assert_eq!(handle.done().await.expect("done").exit_code, Some(0));
}

#[test]
fn duplicate_provider_fails_loudly_and_leaves_the_original_live() {
    let context = Context::new();
    let first = SubprocessService::new(Arc::new(StubRuntime));
    first.provide(&context).expect("first");
    let second = SubprocessService::new(Arc::new(StubRuntime));
    let error = second.provide(&context).expect_err("duplicate");
    assert_eq!(
        format!("{error:#}"),
        "service \"subprocess\" has been registered"
    );
    assert!(Arc::ptr_eq(
        &context.get(SUBPROCESS).expect("original"),
        &first
    ));
}

#[test]
fn ambient_scrub_is_case_insensitive_and_preserves_ordinary_execution_facts() {
    let scrubbed = scrub_environment([
        (OsString::from("PATH"), OsString::from("/bin")),
        (OsString::from("HOME"), OsString::from("/home/test")),
        (OsString::from("SEEKDEEP_STALE"), OsString::from("stale")),
        (
            OsString::from("seekdeep_stale_lower"),
            OsString::from("stale"),
        ),
        (OsString::from("SERVICE_API_KEY"), OsString::from("secret")),
        (OsString::from("db_PASSWORD"), OsString::from("secret")),
        (OsString::from("Secret_Path"), OsString::from("secret")),
        (OsString::from("auth_token_file"), OsString::from("secret")),
        (OsString::from("PLAIN"), OsString::from("visible")),
    ]);
    assert_eq!(
        scrubbed.get(&OsString::from("PATH")),
        Some(&OsString::from("/bin"))
    );
    assert_eq!(
        scrubbed.get(&OsString::from("HOME")),
        Some(&OsString::from("/home/test"))
    );
    assert_eq!(
        scrubbed.get(&OsString::from("PLAIN")),
        Some(&OsString::from("visible"))
    );
    for removed in [
        "SEEKDEEP_STALE",
        "seekdeep_stale_lower",
        "SERVICE_API_KEY",
        "db_PASSWORD",
        "Secret_Path",
        "auth_token_file",
    ] {
        assert!(!scrubbed.contains_key(&OsString::from(removed)));
    }
}

#[test]
fn managed_keys_and_extensible_process_signals_preserve_unknown_values() {
    assert!(SeekDeepEnvironmentKey::new("SEEKDEEP_SESSION_ID").is_ok());
    assert!(SeekDeepEnvironmentKey::new("OTHER_SESSION_ID").is_err());
    let unknown = ProcessSignal::new("SIGFUTURE");
    assert_eq!(unknown.as_str(), "SIGFUTURE");
}

#[tokio::test]
async fn invariant_companion_reserves_and_releases_the_renamed_package() {
    let context = Context::new();
    let registry = seekdeep_invariants::InvariantRegistry::install(
        &context,
        &seekdeep_invariants::InvariantConfig::default(),
    )
    .expect("registry");
    let registration =
        seekdeep_subprocess::invariant::register_invariant(&registry).expect("registration");
    registration.await_ready().await.expect("ready");
    assert!(registry.is_registered("seekdeep-subprocess"));
    registration.dispose().await.expect("dispose");
    assert!(!registry.is_registered("seekdeep-subprocess"));
}
