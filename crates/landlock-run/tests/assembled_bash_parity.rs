//! Real Linux Landlock confinement through the assembled shell executor.

#![cfg(target_os = "linux")]

use std::{fs, path::Path, sync::Arc, time::Duration};

use seekdeep_cordis::Context;
use seekdeep_landlock_run::{LandlockEnforcement, probe};
use seekdeep_sandbox::{SandboxExecutionPolicy, SandboxMode, SandboxService};
use seekdeep_sandbox_local::{LocalSandboxConfig, LocalSandboxProvider, SandboxInternals};
use seekdeep_sandbox_policy::{SandboxPolicyConfig, SandboxPolicyService};
use seekdeep_shell::{ShellExecRequest, ShellExecutor};
use seekdeep_subprocess_local::LocalSubprocessRuntime;

const LAUNCHER: &str = env!("CARGO_BIN_EXE_landlock-run");

fn enforcement() -> Option<seekdeep_sandbox::SandboxEnforcement> {
    match probe(LAUNCHER.as_ref(), Duration::from_secs(5)) {
        LandlockEnforcement::Full => Some(seekdeep_sandbox::SandboxEnforcement::Full),
        LandlockEnforcement::Partial => Some(seekdeep_sandbox::SandboxEnforcement::Partial),
        LandlockEnforcement::Unusable => None,
    }
}

fn require_or_skip() -> Option<seekdeep_sandbox::SandboxEnforcement> {
    let enforcement = enforcement();
    if enforcement.is_some() {
        return enforcement;
    }
    assert_ne!(
        std::env::var("SEEKDEEP_REQUIRE_SANDBOX_E2E").as_deref(),
        Ok("1"),
        "Landlock e2e was required but its functional probe failed"
    );
    eprintln!("Landlock e2e skipped: functional probe failed");
    None
}

async fn executor(
    context: &Context,
    workspace: &Path,
) -> Arc<seekdeep_bash_sandbox::SandboxBashExecutor> {
    let spill = workspace.join("spill");
    fs::create_dir_all(&spill).unwrap();
    LocalSubprocessRuntime::install_runtime(
        context,
        Arc::new(LocalSubprocessRuntime::with_spill_dir(&spill)),
    )
    .unwrap();
    let sandbox = LocalSandboxProvider::new(&LocalSandboxConfig::default()).unwrap();
    sandbox.set_internals(SandboxInternals {
        platform: Some("linux".into()),
        probe_bwrap: Some(Arc::new(|| false)),
        landlock_launcher: Some(LAUNCHER.into()),
        ..SandboxInternals::default()
    });
    SandboxService::new(sandbox).provide(context).unwrap();
    SandboxPolicyService::new(SandboxPolicyConfig {
        mode: SandboxMode::ReadOnly,
        workspace_root: Some(workspace.to_owned()),
    })
    .unwrap()
    .provide(context)
    .unwrap();
    seekdeep_bash_sandbox::apply(
        context,
        seekdeep_bash_sandbox::Config {
            cwd: Some(workspace.to_string_lossy().into_owned()),
            timeout_ms: 30_000.0,
            ..seekdeep_bash_sandbox::Config::default()
        },
    )
    .await
    .unwrap()
}

fn request(command: impl Into<String>, mode: SandboxMode, workspace: &Path) -> ShellExecRequest {
    let mut request = ShellExecRequest::new(command);
    request.sandbox_policy = Some(SandboxExecutionPolicy {
        mode,
        workspace_root: workspace.to_owned(),
        session_id: None,
    });
    request
}

#[tokio::test]
async fn assembled_landlock_denies_read_only_and_escape_but_grants_workspace_and_retry() {
    let Some(expected_enforcement) = require_or_skip() else {
        return;
    };
    let home = std::env::var_os("HOME").unwrap();
    let workspace = tempfile::Builder::new()
        .prefix("seekdeep-landlock-e2e-")
        .tempdir_in(&home)
        .unwrap();
    let outside = tempfile::Builder::new()
        .prefix("seekdeep-landlock-outside-")
        .tempdir_in(home)
        .unwrap();
    let context = Context::new();
    let bash = executor(&context, workspace.path()).await;

    let target = workspace.path().join("target.txt");
    let command = format!("printf landlock-ok > {}", target.display());
    let denied = bash
        .run(
            bash.resolve(request(
                command.clone(),
                SandboxMode::ReadOnly,
                workspace.path(),
            ))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(denied.exit_code, Some(0));
    assert!(denied.sandbox.as_ref().unwrap().denied);
    assert_eq!(
        denied.sandbox.as_ref().unwrap().enforcement,
        Some(expected_enforcement)
    );
    assert!(!target.exists());

    let allowed = bash
        .run(
            bash.resolve(request(
                command,
                SandboxMode::WorkspaceWrite,
                workspace.path(),
            ))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(allowed.exit_code, Some(0));
    assert_eq!(fs::read_to_string(&target).unwrap(), "landlock-ok");

    let escaped = outside.path().join("escaped.txt");
    let escape = bash
        .run(
            bash.resolve(request(
                format!("printf escaped > {}", escaped.display()),
                SandboxMode::WorkspaceWrite,
                workspace.path(),
            ))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(escape.exit_code, Some(0));
    assert!(escape.sandbox.unwrap().denied);
    assert!(!escaped.exists());
}

#[tokio::test]
async fn assembled_landlock_background_denial_is_stamped_before_done_returns() {
    let Some(expected_enforcement) = require_or_skip() else {
        return;
    };
    let home = std::env::var_os("HOME").unwrap();
    let workspace = tempfile::Builder::new()
        .prefix("seekdeep-landlock-background-")
        .tempdir_in(home)
        .unwrap();
    let context = Context::new();
    let bash = executor(&context, workspace.path()).await;
    let target = workspace.path().join("background-denied.txt");
    let process = bash
        .start(
            bash.resolve(request(
                format!("printf denied > {}", target.display()),
                SandboxMode::ReadOnly,
                workspace.path(),
            ))
            .unwrap(),
        )
        .unwrap();
    process.done().await;
    let facts = process.sandbox().unwrap();
    assert!(facts.denied);
    assert_eq!(facts.enforcement, Some(expected_enforcement));
    assert!(!target.exists());
}
