#![cfg(not(windows))]

//! Confining executor parity over the real local subprocess service.

use std::{path::PathBuf, sync::Arc};

use parking_lot::Mutex;
use seekdeep_bash_sandbox::{Config, SandboxBashExecutor, apply};
use seekdeep_cordis::Context;
use seekdeep_llm::AbortSignal;
use seekdeep_sandbox::{
    ConfinedArgv, ConfinedSandboxMode, RunnerFailureRule, SandboxEnforcement,
    SandboxExecutionPolicy, SandboxMode, SandboxPolicy, SandboxProvider, SandboxService,
    SandboxUnavailableError,
};
use seekdeep_sandbox_policy::{SandboxPolicyConfig, SandboxPolicyService};
use seekdeep_shell::{ShellExecRequest, ShellExecutor, ShellProcessStatus};
use seekdeep_subprocess_local::LocalSubprocessRuntime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Behavior {
    Clean,
    Denied,
    NoticeDenied,
    RunnerFatal,
    MissingRunner,
    MalformedExecutable,
}

#[derive(Debug)]
struct RecordingSandbox {
    behavior: Mutex<Behavior>,
    calls: Mutex<Vec<(Vec<String>, SandboxPolicy)>>,
    malformed_program: PathBuf,
}

impl RecordingSandbox {
    fn new(malformed_program: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            behavior: Mutex::new(Behavior::Clean),
            calls: Mutex::new(Vec::new()),
            malformed_program,
        })
    }

    fn set(&self, behavior: Behavior) {
        *self.behavior.lock() = behavior;
    }

    fn calls(&self) -> Vec<(Vec<String>, SandboxPolicy)> {
        self.calls.lock().clone()
    }
}

impl SandboxProvider for RecordingSandbox {
    fn confine(&self, argv: &[String], policy: &SandboxPolicy) -> anyhow::Result<ConfinedArgv> {
        self.calls.lock().push((argv.to_vec(), policy.clone()));
        let command = match *self.behavior.lock() {
            Behavior::Clean => vec![
                "/bin/bash".to_owned(),
                "-c".to_owned(),
                "printf 'confined-ok\\n'".to_owned(),
            ],
            Behavior::Denied => vec![
                "/bin/bash".to_owned(),
                "-c".to_owned(),
                "printf 'ACCESS DENIED by sandbox\\n' >&2; exit 1".to_owned(),
            ],
            Behavior::NoticeDenied => vec![
                "/bin/bash".to_owned(),
                "-c".to_owned(),
                "printf 'runner partial\\nACCESS DENIED by child\\n' >&2; exit 1".to_owned(),
            ],
            Behavior::RunnerFatal => vec![
                "/bin/bash".to_owned(),
                "-c".to_owned(),
                "printf 'runner partial\\nRUNNER FATAL: unavailable\\n' >&2; exit 127".to_owned(),
            ],
            Behavior::MissingRunner => vec!["/nonexistent/seekdeep-sandbox-runner".to_owned()],
            Behavior::MalformedExecutable => {
                vec![self.malformed_program.to_string_lossy().into_owned()]
            }
        };
        Ok(ConfinedArgv {
            argv: command,
            enforcement: SandboxEnforcement::Partial,
            denial_signatures: vec!["access denied".to_owned()],
            runner_failure_rules: vec![RunnerFailureRule {
                allowed_exit_codes: Some(vec![127]),
                fatal_signatures: vec!["runner fatal".to_owned()],
                informational_lines: Some(vec!["runner partial".to_owned()]),
            }],
        })
    }
}

struct Harness {
    _context: Context,
    temp: tempfile::TempDir,
    sandbox: Arc<RecordingSandbox>,
    executor: Arc<SandboxBashExecutor>,
}

async fn harness(default_mode: SandboxMode) -> Harness {
    let context = Context::new();
    let temp = tempfile::tempdir().expect("temp");
    let malformed_program = temp.path().join("malformed-runner");
    std::fs::write(&malformed_program, "not an executable\n").expect("malformed runner");
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = std::fs::metadata(&malformed_program)
            .expect("malformed metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&malformed_program, permissions).expect("malformed permissions");
    }
    LocalSubprocessRuntime::install_runtime(
        &context,
        Arc::new(LocalSubprocessRuntime::with_spill_dir(temp.path())),
    )
    .expect("subprocess");
    let sandbox = RecordingSandbox::new(malformed_program);
    SandboxService::new(sandbox.clone())
        .provide(&context)
        .expect("sandbox");
    SandboxPolicyService::new(SandboxPolicyConfig {
        mode: default_mode,
        workspace_root: Some(temp.path().to_owned()),
    })
    .expect("policy")
    .provide(&context)
    .expect("provide policy");
    let executor = apply(
        &context,
        Config {
            timeout_ms: 5_000.0,
            grace_ms: 100.0,
            ..Config::default()
        },
    )
    .await
    .expect("bash sandbox");
    Harness {
        _context: context,
        temp,
        sandbox,
        executor,
    }
}

fn request(command: &str) -> ShellExecRequest {
    ShellExecRequest::new(command)
}

#[tokio::test]
async fn resolve_wrap_and_denial_facts_preserve_exact_policy_and_bash_argv() {
    let harness = harness(SandboxMode::ReadOnly).await;
    let spec = harness
        .executor
        .resolve(request("printf hi"))
        .expect("resolve");
    let policy = spec.sandbox_policy.as_ref().expect("policy");
    assert_eq!(policy.mode, SandboxMode::ReadOnly);
    assert_eq!(
        policy.workspace_root,
        harness.temp.path().canonicalize().expect("canonical root")
    );

    let clean = harness.executor.run(spec.clone()).await.expect("clean");
    assert_eq!(clean.stdout.text, "confined-ok\n");
    assert_eq!(
        clean.sandbox.as_ref().expect("facts").mode,
        SandboxMode::ReadOnly
    );
    assert_eq!(
        clean.sandbox.as_ref().expect("facts").enforcement,
        Some(SandboxEnforcement::Partial)
    );
    assert!(!clean.sandbox.as_ref().expect("facts").denied);
    let calls = harness.sandbox.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].1.mode, ConfinedSandboxMode::ReadOnly);
    assert_eq!(calls[0].0, ["bash", "-c", "printf hi"]);

    harness.sandbox.set(Behavior::Denied);
    let denied = harness.executor.run(spec).await.expect("denied result");
    let facts = denied.sandbox.expect("denial facts");
    assert!(facts.denied);
    assert_eq!(facts.runner_failed, None);
}

#[tokio::test]
async fn supplied_policy_overrides_default_and_full_access_bypasses_confine() {
    let harness = harness(SandboxMode::ReadOnly).await;
    let mut workspace = request("printf full-access");
    workspace.sandbox_policy = Some(SandboxExecutionPolicy {
        mode: SandboxMode::WorkspaceWrite,
        workspace_root: harness.temp.path().to_owned(),
        session_id: None,
    });
    let spec = harness.executor.resolve(workspace).expect("workspace spec");
    harness.executor.run(spec).await.expect("workspace run");
    assert_eq!(
        harness.sandbox.calls()[0].1.mode,
        ConfinedSandboxMode::WorkspaceWrite
    );

    let before = harness.sandbox.calls().len();
    let mut full = request("printf full-access");
    full.sandbox_policy = Some(SandboxExecutionPolicy {
        mode: SandboxMode::DangerFullAccess,
        workspace_root: harness.temp.path().to_owned(),
        session_id: None,
    });
    let result = harness
        .executor
        .run(harness.executor.resolve(full).expect("full spec"))
        .await
        .expect("full run");
    assert_eq!(result.stdout.text, "full-access");
    assert_eq!(harness.sandbox.calls().len(), before);
    let facts = result.sandbox.expect("full facts");
    assert_eq!(facts.mode, SandboxMode::DangerFullAccess);
    assert!(!facts.denied);
    assert_eq!(facts.enforcement, None);
}

#[tokio::test]
async fn spawn_and_runtime_runner_failures_fail_closed_while_abort_and_bad_cwd_keep_their_cause() {
    let harness = harness(SandboxMode::ReadOnly).await;
    harness.sandbox.set(Behavior::MissingRunner);
    let spec = harness.executor.resolve(request("ignored")).expect("spec");
    let error = harness.executor.run(spec).await.unwrap_err();
    let unavailable = error
        .downcast_ref::<SandboxUnavailableError>()
        .expect("sandbox unavailable");
    assert_eq!(unavailable.code(), seekdeep_sandbox::SANDBOX_UNAVAILABLE);

    let signal = AbortSignal::default();
    signal.abort();
    let mut aborted = request("ignored");
    aborted.signal = Some(signal);
    let error = harness
        .executor
        .run(harness.executor.resolve(aborted).expect("aborted spec"))
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<SandboxUnavailableError>().is_none());
    assert!(error.to_string().contains("aborted"));

    let mut bad_cwd = request("ignored");
    bad_cwd.workdir = Some(PathBuf::from("/nonexistent-seekdeep-bash-sandbox-cwd"));
    let error = harness
        .executor
        .run(harness.executor.resolve(bad_cwd).expect("bad cwd spec"))
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<SandboxUnavailableError>().is_none());

    harness.sandbox.set(Behavior::RunnerFatal);
    let spec = harness
        .executor
        .resolve(request("ignored"))
        .expect("fatal spec");
    let error = harness.executor.run(spec).await.unwrap_err();
    assert!(
        error
            .downcast_ref::<SandboxUnavailableError>()
            .expect("runtime unavailable")
            .message()
            .contains("RUNNER FATAL: unavailable")
    );

    harness.sandbox.set(Behavior::MalformedExecutable);
    let spec = harness
        .executor
        .resolve(request("ignored"))
        .expect("malformed spec");
    match harness.executor.run(spec).await {
        Ok(result) => {
            let facts = result.sandbox.expect("ordinary malformed facts");
            assert_eq!(facts.runner_failed, None);
            assert!(!facts.denied);
        }
        Err(error) => {
            assert!(error.downcast_ref::<SandboxUnavailableError>().is_none());
            assert!(
                error.to_string().contains("Exec format")
                    || error.to_string().contains("os error 8"),
                "{error:#}"
            );
        }
    }

    let process = harness
        .executor
        .start(
            harness
                .executor
                .resolve(request("ignored"))
                .expect("malformed background spec"),
        )
        .expect("malformed handle");
    process.done().await;
    let facts = process.sandbox().expect("ordinary malformed facts");
    assert_eq!(facts.runner_failed, None);
    assert!(!facts.denied);
}

#[tokio::test]
async fn background_settlement_stamps_per_process_facts_before_done_returns() {
    let harness = harness(SandboxMode::ReadOnly).await;
    let clean = harness
        .executor
        .start(
            harness
                .executor
                .resolve(request("clean"))
                .expect("clean spec"),
        )
        .expect("clean start");
    assert_eq!(clean.sandbox(), None);
    clean.done().await;
    assert_eq!(clean.status(), ShellProcessStatus::Completed);
    let facts = clean.sandbox().expect("clean facts");
    assert!(!facts.denied);
    assert_eq!(facts.runner_failed, None);

    harness.sandbox.set(Behavior::NoticeDenied);
    let denied = harness
        .executor
        .start(
            harness
                .executor
                .resolve(request("notice plus denied"))
                .expect("denied spec"),
        )
        .expect("denied start");
    denied.done().await;
    assert!(denied.sandbox().expect("denied facts").denied);

    harness.sandbox.set(Behavior::RunnerFatal);
    let fatal = harness
        .executor
        .start(
            harness
                .executor
                .resolve(request("fatal"))
                .expect("fatal spec"),
        )
        .expect("fatal start");
    fatal.done().await;
    let facts = fatal.sandbox().expect("fatal facts");
    assert_eq!(facts.runner_failed, Some(true));
    assert!(!facts.denied);

    harness.sandbox.set(Behavior::MissingRunner);
    let missing = harness
        .executor
        .start(
            harness
                .executor
                .resolve(request("missing"))
                .expect("missing spec"),
        )
        .expect("missing handle");
    missing.done().await;
    assert_eq!(missing.status(), ShellProcessStatus::Killed);
    let facts = missing.sandbox().expect("missing facts");
    assert_eq!(facts.runner_failed, Some(true));
    assert!(!facts.denied);
    assert!(missing.read_output().delta.contains("spawn failed:"));
}

#[tokio::test]
async fn full_access_background_bypasses_confine_and_carries_no_facts() {
    let harness = harness(SandboxMode::DangerFullAccess).await;
    let process = harness
        .executor
        .start(
            harness
                .executor
                .resolve(request("printf background-ok"))
                .expect("spec"),
        )
        .expect("start");
    process.done().await;
    assert_eq!(process.read_output().delta, "background-ok");
    assert_eq!(process.sandbox(), None);
    assert!(harness.sandbox.calls().is_empty());
}
