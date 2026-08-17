//! Concrete-provider contract and service lifecycle parity.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use seekdeep_cordis::Context;
use seekdeep_shell::{
    CollectedOutput, SHELL, ShellExecRequest, ShellExecSpec, ShellExecutor, ShellProcess,
    ShellProcessHandle, ShellProcessRead, ShellProcessStatus, ShellRunResult, ShellService,
};

#[derive(Debug)]
struct StubExecutor;

#[async_trait]
impl ShellExecutor for StubExecutor {
    fn resolve(&self, request: ShellExecRequest) -> anyhow::Result<ShellExecSpec> {
        Ok(ShellExecSpec {
            command: request.command,
            workdir: request.workdir.unwrap_or_else(|| PathBuf::from("/stub")),
            timeout_ms: request.timeout_ms.unwrap_or(1_000),
            stdout_max_bytes: request.stdout_max_bytes.unwrap_or(64_000),
            signal: request.signal,
            stdin: request.stdin,
            env: request.env,
            seekdeep_env: request.seekdeep_env,
            sandbox_policy: request.sandbox_policy,
        })
    }

    async fn run(&self, spec: ShellExecSpec) -> anyhow::Result<ShellRunResult> {
        Ok(ShellRunResult {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
            aborted: false,
            timeout_ms: spec.timeout_ms,
            stdout: CollectedOutput {
                text: "ok".to_owned(),
                truncated: false,
                spill_path: None,
            },
            stderr: CollectedOutput::default(),
            sandbox: None,
        })
    }

    fn start(&self, _spec: ShellExecSpec) -> anyhow::Result<ShellProcessHandle> {
        Ok(Arc::new(StubProcess {
            running: AtomicBool::new(true),
        }))
    }
}

#[derive(Debug)]
struct StubProcess {
    running: AtomicBool,
}

#[async_trait]
impl ShellProcess for StubProcess {
    fn status(&self) -> ShellProcessStatus {
        if self.running.load(Ordering::Acquire) {
            ShellProcessStatus::Running
        } else {
            ShellProcessStatus::Killed
        }
    }

    fn exit_code(&self) -> Option<i32> {
        None
    }

    fn signal(&self) -> Option<seekdeep_shell::ProcessSignal> {
        None
    }

    fn sandbox(&self) -> Option<seekdeep_shell::ShellSandboxInfo> {
        None
    }

    async fn done(&self) {}

    fn read_output(&self) -> ShellProcessRead {
        ShellProcessRead::default()
    }

    fn kill(&self) -> bool {
        self.running.swap(false, Ordering::AcqRel)
    }
}

#[tokio::test]
async fn concrete_provider_serves_the_complete_task_free_seam() {
    let context = Context::new();
    let service = ShellService::new(Arc::new(StubExecutor));
    let provision = service.provide(&context).expect("provide shell");
    let shell = context.get(SHELL).expect("shell service");
    let spec = shell
        .resolve(ShellExecRequest::new("echo hi"))
        .expect("resolve");
    assert_eq!(spec.command, "echo hi");
    assert_eq!(spec.workdir, PathBuf::from("/stub"));
    assert_eq!(spec.timeout_ms, 1_000);
    assert_eq!(spec.stdout_max_bytes, 64_000);
    assert!(spec.sandbox_policy.is_none());

    let result = shell.run(spec.clone()).await.expect("run");
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout.text, "ok");

    let process = shell.start(spec).expect("start");
    assert_eq!(process.status(), ShellProcessStatus::Running);
    assert_eq!(process.read_output(), ShellProcessRead::default());
    assert!(process.kill());
    assert!(!process.kill());
    process.done().await;
    assert!(shell.sandbox_mode().is_none());
    provision.dispose().await.expect("dispose provider");
    assert!(context.get(SHELL).is_none());
}

#[test]
fn duplicate_provider_fails_loud_and_original_remains_live() {
    let context = Context::new();
    let first = ShellService::new(Arc::new(StubExecutor));
    first.provide(&context).expect("first");
    let second = ShellService::new(Arc::new(StubExecutor));
    let error = second.provide(&context).expect_err("duplicate");
    assert!(
        error
            .to_string()
            .contains("service \"shell\" has been registered")
    );
    assert!(Arc::ptr_eq(&context.get(SHELL).expect("original"), &first));
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
        seekdeep_shell::invariant::register_invariant(&registry).expect("invariant registration");
    registration.await_ready().await.expect("ready");
    assert!(registry.is_registered("seekdeep-shell"));
    registration.dispose().await.expect("dispose");
    assert!(!registry.is_registered("seekdeep-shell"));
}
