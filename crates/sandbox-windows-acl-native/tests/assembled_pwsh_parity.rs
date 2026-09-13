//! Real Windows assembly of sandbox policy, local provider, subprocess, and PowerShell execution.

#![cfg(windows)]

use std::{path::Path, sync::Arc};

use seekdeep_cordis::Context;
use seekdeep_pwsh_sandbox::{Config, apply};
use seekdeep_sandbox::{SandboxExecutionPolicy, SandboxMode};
use seekdeep_sandbox_local::{
    LocalSandboxConfig, LocalSandboxRunner, SandboxInternals, install as install_sandbox,
};
use seekdeep_sandbox_policy::{SandboxPolicyConfig, SandboxPolicyService};
use seekdeep_shell::{ShellExecRequest, ShellExecutor};
use seekdeep_subprocess_local::LocalSubprocessRuntime;

#[path = "windows_support/pwsh.rs"]
mod pwsh_support;

use pwsh_support::pwsh_path;

const RUNNER: &str = env!("CARGO_BIN_EXE_windows-acl-run");

fn ps_literal(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

fn request(command: String, mode: SandboxMode, workspace: &Path) -> ShellExecRequest {
    let mut request = ShellExecRequest::new(command);
    request.sandbox_policy = Some(SandboxExecutionPolicy {
        mode,
        workspace_root: workspace.to_owned(),
        session_id: None,
    });
    request
}

#[tokio::test]
async fn assembled_executor_preserves_acl_effects_and_settled_denial_facts() {
    let Some(pwsh) = pwsh_path() else {
        eprintln!("skipping assembled Windows ACL test because PowerShell 7 is unavailable");
        return;
    };
    let profile = std::env::var_os("USERPROFILE")
        .map(std::path::PathBuf::from)
        .expect("Windows test host must expose USERPROFILE");
    let scratch = tempfile::Builder::new()
        .prefix("seekdeep-pwsh-sandbox-e2e-")
        .tempdir_in(profile)
        .unwrap();
    let workspace = scratch.path().join("writable");
    std::fs::create_dir(&workspace).unwrap();
    let outside_temp = tempfile::Builder::new()
        .prefix("seekdeep-pwsh-sandbox-outside-")
        .tempdir()
        .unwrap();
    let secret = scratch.path().join("secret.txt");
    let escape = scratch.path().join("escaped.txt");
    std::fs::write(&secret, "read boundary").unwrap();

    let context = Context::new();
    LocalSubprocessRuntime::install(&context).unwrap();
    let sandbox = install_sandbox(&context, &LocalSandboxConfig::default()).unwrap();
    sandbox.provider.set_internals(SandboxInternals {
        platform: Some("win32".into()),
        chain: Some(vec![LocalSandboxRunner::WindowsAcl]),
        probe_windows_acl: Some(Arc::new(|| true)),
        windows_acl_runner_args: Some(vec![RUNNER.into()]),
        ..SandboxInternals::default()
    });
    SandboxPolicyService::new(SandboxPolicyConfig {
        mode: SandboxMode::WorkspaceWrite,
        workspace_root: Some(workspace.clone()),
    })
    .unwrap()
    .provide(&context)
    .unwrap();
    let executor = apply(
        &context,
        Config {
            pwsh_path: Some(pwsh.to_owned()),
            timeout_ms: 60_000.0,
            ..Config::default()
        },
    )
    .await
    .unwrap();

    let read_only_target = workspace.join("ro-write.txt");
    let outside_target = outside_temp.path().join("ro-write.txt");
    let read_only_script = format!(
        "$ErrorActionPreference='SilentlyContinue';\
         try{{Set-Content -LiteralPath '{}' -Value ok -ErrorAction Stop;'TARGET-WRITE: OK'}}catch{{'TARGET-WRITE: DENIED'}};\
         try{{Set-Content -LiteralPath '{}' -Value ok -ErrorAction Stop;'TEMP-WRITE: OK'}}catch{{'TEMP-WRITE: DENIED'}};\
         try{{Set-Content -LiteralPath '{}' -Value ok -ErrorAction Stop;'ESCAPE-WRITE: OK'}}catch{{'ESCAPE-WRITE: DENIED'}};\
         try{{Get-Content -LiteralPath '{}' -ErrorAction Stop | Out-Null;'SECRET-READ: OK'}}catch{{'SECRET-READ: DENIED'}}",
        ps_literal(&read_only_target),
        ps_literal(&outside_target),
        ps_literal(&escape),
        ps_literal(&secret),
    );
    let read_only = executor
        .run(
            executor
                .resolve(request(read_only_script, SandboxMode::ReadOnly, &workspace))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(read_only.exit_code, Some(0), "{}", read_only.stderr.text);
    assert!(read_only.stdout.text.contains("TARGET-WRITE: DENIED"));
    assert!(read_only.stdout.text.contains("TEMP-WRITE: DENIED"));
    assert!(read_only.stdout.text.contains("ESCAPE-WRITE: DENIED"));
    assert!(read_only.stdout.text.contains("SECRET-READ: OK"));
    assert!(!read_only_target.exists());
    assert_eq!(
        read_only.sandbox,
        Some(seekdeep_shell::ShellSandboxInfo {
            mode: SandboxMode::ReadOnly,
            denied: false,
            enforcement: Some(seekdeep_sandbox::SandboxEnforcement::Partial),
            runner_failed: None,
        })
    );

    let raw_denial = executor
        .run(
            executor
                .resolve(request(
                    format!(
                        "Set-Content -LiteralPath '{}' -Value x",
                        ps_literal(&escape)
                    ),
                    SandboxMode::ReadOnly,
                    &workspace,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(raw_denial.exit_code, Some(0));
    assert!(raw_denial.sandbox.as_ref().unwrap().denied);

    let workspace_target = workspace.join("ww-write.txt");
    let ambient_target = outside_temp.path().join("ww-write.txt");
    let workspace_script = format!(
        "$ErrorActionPreference='SilentlyContinue';\
         try{{Set-Content -LiteralPath '{}' -Value ok -ErrorAction Stop;'TARGET-WRITE: OK'}}catch{{'TARGET-WRITE: DENIED'}};\
         try{{Set-Content -LiteralPath (Join-Path $env:TEMP 'ww-write.txt') -Value ok -ErrorAction Stop;'TEMP-WRITE: OK'}}catch{{'TEMP-WRITE: DENIED'}};\
         try{{Set-Content -LiteralPath '{}' -Value ok -ErrorAction Stop;'AMBIENT-TEMP-WRITE: OK'}}catch{{'AMBIENT-TEMP-WRITE: DENIED'}};\
         try{{Set-Content -LiteralPath '{}' -Value ok -ErrorAction Stop;'ESCAPE-WRITE: OK'}}catch{{'ESCAPE-WRITE: DENIED'}};\
         try{{Get-Content -LiteralPath '{}' -ErrorAction Stop | Out-Null;'SECRET-READ: OK'}}catch{{'SECRET-READ: DENIED'}};\
         'TEMP-PATH: ' + $env:TEMP",
        ps_literal(&workspace_target),
        ps_literal(&ambient_target),
        ps_literal(&escape),
        ps_literal(&secret),
    );
    let workspace_write = executor
        .run(
            executor
                .resolve(request(
                    workspace_script,
                    SandboxMode::WorkspaceWrite,
                    &workspace,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        workspace_write.exit_code,
        Some(0),
        "{}",
        workspace_write.stderr.text
    );
    assert!(workspace_write.stdout.text.contains("TARGET-WRITE: OK"));
    assert!(workspace_write.stdout.text.contains("TEMP-WRITE: OK"));
    assert!(
        workspace_write
            .stdout
            .text
            .contains("AMBIENT-TEMP-WRITE: DENIED")
    );
    assert!(workspace_write.stdout.text.contains("ESCAPE-WRITE: DENIED"));
    assert!(workspace_write.stdout.text.contains("SECRET-READ: OK"));
    assert!(workspace_target.exists());
    assert!(!ambient_target.exists());
    assert!(!escape.exists());
    let private_temp = workspace_write
        .stdout
        .text
        .lines()
        .find_map(|line| line.strip_prefix("TEMP-PATH: "))
        .map(str::trim)
        .expect("runner must report its private temp path");
    let ambient_temp = std::env::temp_dir().to_string_lossy().into_owned();
    assert!(private_temp.starts_with(&ambient_temp));
    assert!(!Path::new(private_temp).exists());
    assert_eq!(
        workspace_write.sandbox,
        Some(seekdeep_shell::ShellSandboxInfo {
            mode: SandboxMode::WorkspaceWrite,
            denied: false,
            enforcement: Some(seekdeep_sandbox::SandboxEnforcement::Partial),
            runner_failed: None,
        })
    );

    sandbox.dispose().await.unwrap();
}
