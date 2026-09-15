//! Real macOS Seatbelt world-effect proofs through the provider's ordinary confine path.

#![cfg(target_os = "macos")]

use std::{fs, path::Path, process::Command};

use seekdeep_sandbox::{ConfinedSandboxMode, SandboxPolicy, SandboxProvider};
use seekdeep_sandbox_local::{
    LocalSandboxConfig, LocalSandboxProvider, SandboxInternals, seatbelt_profile_args,
};

fn policy(mode: ConfinedSandboxMode, workspace_root: &Path) -> SandboxPolicy {
    SandboxPolicy {
        mode,
        workspace_root: workspace_root.to_owned(),
        session_id: None,
    }
}

fn usable() -> bool {
    let profile =
        seatbelt_profile_args(&policy(ConfinedSandboxMode::ReadOnly, Path::new("/"))).unwrap();
    Command::new("sandbox-exec")
        .args(profile)
        .args(["--", "true"])
        .status()
        .is_ok_and(|status| status.success())
}

fn run(
    provider: &LocalSandboxProvider,
    command: &str,
    policy: &SandboxPolicy,
) -> (std::process::Output, seekdeep_sandbox::ConfinedArgv) {
    let confined = provider
        .confine(&["bash".into(), "-c".into(), command.into()], policy)
        .unwrap();
    let output = Command::new(&confined.argv[0])
        .args(&confined.argv[1..])
        .output()
        .unwrap();
    (output, confined)
}

#[test]
fn real_seatbelt_denies_read_only_writes_and_grants_workspace_and_temp_roots() {
    if !usable() {
        eprintln!("seatbelt e2e skipped: sandbox-exec cannot enforce the profile");
        return;
    }
    let provider = LocalSandboxProvider::new(&LocalSandboxConfig::default()).unwrap();
    provider.set_internals(SandboxInternals {
        platform: Some("darwin".into()),
        ..SandboxInternals::default()
    });

    let read_only_dir = tempfile::tempdir().unwrap();
    let denied = read_only_dir.path().join("denied.txt");
    let (output, confined) = run(
        &provider,
        &format!("echo hi > {}", denied.display()),
        &policy(ConfinedSandboxMode::ReadOnly, read_only_dir.path()),
    );
    assert!(!output.status.success());
    assert!(!denied.exists());
    assert_eq!(confined.denial_signatures, ["operation not permitted"]);
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .to_lowercase()
            .contains("operation not permitted")
    );

    let (output, _) = run(
        &provider,
        "ls / > /dev/null && echo dev-ok",
        &policy(ConfinedSandboxMode::ReadOnly, read_only_dir.path()),
    );
    assert!(output.status.success());
    assert_eq!(output.stdout, b"dev-ok\n");

    let home = std::env::var_os("HOME").unwrap();
    let workspace = tempfile::Builder::new()
        .prefix("seekdeep-seatbelt-e2e-")
        .tempdir_in(home)
        .unwrap();
    let outside = tempfile::Builder::new()
        .prefix("seekdeep-seatbelt-outside-")
        .tempdir_in(std::env::var_os("HOME").unwrap())
        .unwrap();
    let allowed = workspace.path().join("allowed.txt");
    let (output, _) = run(
        &provider,
        &format!("printf seatbelt-ok > {}", allowed.display()),
        &policy(ConfinedSandboxMode::WorkspaceWrite, workspace.path()),
    );
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(fs::read_to_string(allowed).unwrap(), "seatbelt-ok");

    let outside_file = outside.path().join("denied.txt");
    let (output, _) = run(
        &provider,
        &format!("echo hi > {}", outside_file.display()),
        &policy(ConfinedSandboxMode::WorkspaceWrite, workspace.path()),
    );
    assert!(!output.status.success());
    assert!(!outside_file.exists());

    let host_tmp = tempfile::tempdir_in("/tmp").unwrap();
    let user_tmp = tempfile::tempdir_in(std::env::temp_dir()).unwrap();
    let host_file = host_tmp.path().join("scratch.txt");
    let user_file = user_tmp.path().join("scratch.txt");
    let (output, _) = run(
        &provider,
        &format!(
            "printf tmp-ok > {} && printf user-tmp-ok > {}",
            host_file.display(),
            user_file.display()
        ),
        &policy(ConfinedSandboxMode::WorkspaceWrite, workspace.path()),
    );
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(fs::read_to_string(host_file).unwrap(), "tmp-ok");
    assert_eq!(fs::read_to_string(user_file).unwrap(), "user-tmp-ok");
}
