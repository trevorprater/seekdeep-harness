//! Failure boundaries of provider-owned output streams and typed spawn errors.
use super::*;

#[test]
fn executable_failure_classification_accepts_only_spawn_evidence() {
    for text in [
        "no such file",
        "not found",
        "permission denied",
        "access is denied",
        "os error 2",
        "os error 5",
        "os error 13",
    ] {
        assert!(executable_spawn_failure_text(text));
    }
    assert!(!executable_spawn_failure_text("child exited with status 1"));
}

#[tokio::test]
async fn a_missing_collect_stream_is_rejected_for_each_output() {
    let context = Context::new();
    seekdeep_subprocess_local::LocalSubprocessRuntime::install(&context).unwrap();
    let subprocess = context.get(SUBPROCESS).unwrap();
    let collect = SubprocessOutputMode::Collect(SubprocessCollect {
        max_bytes: 100.0,
        spill: None,
    });
    for (stdout, stderr) in [
        (SubprocessOutputMode::Pipe, collect.clone()),
        (collect, SubprocessOutputMode::Pipe),
    ] {
        let handle = subprocess
            .spawn(SubprocessSpawnSpec {
                argv: vec!["node".into(), "-e".into(), String::new()],
                cwd: std::env::current_dir().unwrap(),
                stdio: SubprocessStdio {
                    stdin: SubprocessStdinMode::Ignore,
                    stdout,
                    stderr,
                },
                grace_ms: 100.0,
                signal: None,
                env: None,
            })
            .unwrap();
        handle.done().await.unwrap();
        assert!(
            LocalPwshExecutor::collected(&handle)
                .unwrap_err()
                .to_string()
                .contains("dropped a requested collect stream")
        );
    }
}

#[test]
fn missing_current_directory_falls_back_to_a_relative_workdir() {
    assert_eq!(
        current_workdir(Ok(PathBuf::from("workspace"))),
        PathBuf::from("workspace")
    );
    assert_eq!(
        current_workdir(Err(std::io::Error::from(std::io::ErrorKind::NotFound))),
        PathBuf::from(".")
    );
}

#[derive(Clone, Copy, Debug)]
enum Fault {
    MissingStdout,
    MissingStderr,
    LateFailure,
    SpawnFailure,
}

#[derive(Debug)]
struct FaultyRuntime {
    inner: seekdeep_subprocess_local::LocalSubprocessRuntime,
    fault: Fault,
    spawned: Mutex<Vec<SubprocessHandleRef>>,
}

#[async_trait]
impl seekdeep_subprocess::SubprocessRuntime for FaultyRuntime {
    async fn resolve_executable(
        &self,
        command: &str,
        env: Option<&seekdeep_subprocess::SubprocessLookupEnvironment>,
        signal: Option<seekdeep_llm::AbortSignal>,
    ) -> anyhow::Result<String> {
        self.inner.resolve_executable(command, env, signal).await
    }

    fn spawn(&self, spec: SubprocessSpawnSpec) -> anyhow::Result<SubprocessHandleRef> {
        if matches!(self.fault, Fault::SpawnFailure) {
            anyhow::bail!("provider initialization failed");
        }
        let inner = self.inner.spawn(spec)?;
        self.spawned.lock().push(inner.clone());
        Ok(Arc::new(FaultyHandle {
            inner,
            fault: self.fault,
        }))
    }

    async fn spawn_terminal(
        &self,
        spec: seekdeep_subprocess::SubprocessTerminalSpawnSpec,
    ) -> anyhow::Result<seekdeep_subprocess::SubprocessTerminalHandleRef> {
        self.inner.spawn_terminal(spec).await
    }
}

#[derive(Debug)]
struct FaultyHandle {
    inner: SubprocessHandleRef,
    fault: Fault,
}

#[async_trait]
impl seekdeep_subprocess::SubprocessHandle for FaultyHandle {
    fn pid(&self) -> seekdeep_subprocess::ProcessId {
        self.inner.pid()
    }
    fn stdin(&self) -> Option<seekdeep_subprocess::SubprocessInput> {
        self.inner.stdin()
    }
    fn stdout(&self) -> Option<seekdeep_subprocess::SubprocessOutput> {
        self.inner.stdout()
    }
    fn stderr(&self) -> Option<seekdeep_subprocess::SubprocessOutput> {
        self.inner.stderr()
    }
    fn collected(&self) -> SubprocessCollectedOutputs {
        let mut outputs = self.inner.collected();
        match self.fault {
            Fault::MissingStdout => outputs.stdout = None,
            Fault::MissingStderr => outputs.stderr = None,
            Fault::LateFailure | Fault::SpawnFailure => {}
        }
        outputs
    }
    async fn done(&self) -> anyhow::Result<seekdeep_subprocess::SubprocessOutcome> {
        let outcome = self.inner.done().await?;
        if matches!(self.fault, Fault::LateFailure) {
            anyhow::bail!("provider failed after process start: os error 2");
        }
        Ok(outcome)
    }
    fn terminate(&self) {
        self.inner.terminate();
    }
    async fn wait_for_exit(
        &self,
        signal: Option<seekdeep_llm::AbortSignal>,
    ) -> anyhow::Result<bool> {
        self.inner.wait_for_exit(signal).await
    }
}

#[tokio::test]
async fn foreground_and_background_reject_malformed_provider_contracts() {
    for fault in [
        Fault::MissingStdout,
        Fault::MissingStderr,
        Fault::LateFailure,
        Fault::SpawnFailure,
    ] {
        let context = Context::new();
        let runtime = Arc::new(FaultyRuntime {
            inner: seekdeep_subprocess_local::LocalSubprocessRuntime::default(),
            fault,
            spawned: Mutex::new(Vec::new()),
        });
        seekdeep_subprocess::SubprocessService::new(runtime.clone())
            .provide(&context)
            .unwrap();
        let executor = build(&context, Config::default()).await.unwrap();
        let spec = executor.resolve(ShellExecRequest::new("probe")).unwrap();
        let argv = vec!["node".into(), "-e".into(), String::new()];
        let error = executor
            .run_argv(spec.clone(), argv.clone())
            .await
            .unwrap_err();
        assert!(error.downcast_ref::<PwshSpawnFailure>().is_none());
        match fault {
            Fault::MissingStdout | Fault::MissingStderr => {
                assert!(
                    error
                        .to_string()
                        .contains("dropped a requested collect stream")
                );
                assert!(executor.start_argv(&spec, argv).is_err());
            }
            Fault::SpawnFailure => {
                assert!(error.to_string().contains("provider initialization failed"));
                assert!(executor.start_argv(&spec, argv).is_err());
            }
            Fault::LateFailure => assert!(error.to_string().contains("after process start")),
        }
        let spawned = runtime.spawned.lock().clone();
        for handle in spawned {
            handle.done().await.unwrap();
        }
    }
}
