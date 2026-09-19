#![cfg(not(windows))]

//! Confining executor parity over the real local subprocess service.

use std::{
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_llm::AbortSignal;
use seekdeep_pwsh_local::ENCODING_PREAMBLE;
use seekdeep_pwsh_sandbox::{Config, SandboxPwshExecutor, apply};
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
    RunnerFatal,
    MissingRunner,
}

#[derive(Debug)]
struct RecordingSandbox {
    behavior: Mutex<Behavior>,
    calls: Mutex<Vec<(Vec<String>, SandboxPolicy)>>,
}

impl RecordingSandbox {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            behavior: Mutex::new(Behavior::Clean),
            calls: Mutex::new(Vec::new()),
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
            Behavior::RunnerFatal => vec![
                "/bin/bash".to_owned(),
                "-c".to_owned(),
                "printf 'runner partial\\nRUNNER FATAL: unavailable\\n' >&2; exit 127".to_owned(),
            ],
            Behavior::MissingRunner => vec!["/nonexistent/seekdeep-sandbox-runner".to_owned()],
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
    executor: Arc<SandboxPwshExecutor>,
}

fn pwsh_shim(directory: &Path) -> PathBuf {
    let path = directory.join("pwsh-test-shim");
    let script = format!(
        "#!/bin/bash\nprefix='{ENCODING_PREAMBLE}'\nlast=\"${{@: -1}}\"\ncommand=\"${{last#\"$prefix\"}}\"\nexec /bin/bash -c \"$command\"\n"
    );
    fs::write(&path, script).expect("write shim");
    let mut permissions = fs::metadata(&path).expect("metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("permissions");
    path
}

async fn harness(default_mode: SandboxMode) -> Harness {
    let context = Context::new();
    let temp = tempfile::tempdir().expect("temp");
    LocalSubprocessRuntime::install_runtime(
        &context,
        Arc::new(LocalSubprocessRuntime::with_spill_dir(temp.path())),
    )
    .expect("subprocess");
    let sandbox = RecordingSandbox::new();
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
            pwsh_path: Some(pwsh_shim(temp.path()).to_string_lossy().into_owned()),
            timeout_ms: 5_000.0,
            grace_ms: 100.0,
            ..Config::default()
        },
    )
    .await
    .expect("pwsh sandbox");
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
async fn resolve_wrap_and_denial_facts_preserve_exact_policy_and_pwsh_argv() {
    let harness = harness(SandboxMode::ReadOnly).await;
    let spec = harness
        .executor
        .resolve(request("Write-Output hi"))
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
    assert_eq!(
        &calls[0].0[1..5],
        ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"]
    );
    assert_eq!(calls[0].0[5], format!("{ENCODING_PREAMBLE}Write-Output hi"));

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
    bad_cwd.workdir = Some(PathBuf::from("/nonexistent-seekdeep-pwsh-sandbox-cwd"));
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

    harness.sandbox.set(Behavior::Denied);
    let denied = harness
        .executor
        .start(
            harness
                .executor
                .resolve(request("denied"))
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
