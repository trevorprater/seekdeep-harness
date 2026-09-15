//! Real restricted-token probe matching the source `probe.spec.ts` contract.

#![cfg(windows)]

use std::{path::Path, sync::Arc};

use seekdeep_sandbox_windows_acl::{
    AclSandbox, AclSandboxMode, AclSandboxOptions, AclTempDirState, SandboxStdio,
    WindowsAclBindings, temp_write_sid, workspace_write_sid,
};
use seekdeep_sandbox_windows_acl_native::WindowsBindings;

#[path = "windows_support/pwsh.rs"]
mod pwsh_support;

use pwsh_support::pwsh_path;

fn ps_literal(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

fn binding() -> Arc<dyn WindowsAclBindings> {
    Arc::new(WindowsBindings)
}

fn options(workspace: &Path, temp: Option<&Path>, write_sid: &str) -> AclSandboxOptions {
    AclSandboxOptions {
        writable_dirs: vec![workspace.to_owned()],
        temp_dir: temp.map(Path::to_owned),
        temp_was_explicit: true,
        write_sid: Some(write_sid.to_owned()),
        temp_write_sid: temp.map(|path| temp_write_sid(&path.to_string_lossy())),
        mode: AclSandboxMode::WorkspaceWrite,
        manage_dacls: true,
    }
}

#[tokio::test]
async fn granted_directories_accept_writes_escape_is_denied_and_reads_remain_available() {
    let Some(pwsh) = pwsh_path() else {
        eprintln!("skipping real Windows probe because PowerShell 7 is unavailable");
        return;
    };

    let scratch = tempfile::tempdir().unwrap();
    let workspace = scratch.path().join("writable");
    std::fs::create_dir(&workspace).unwrap();
    let private_temp = tempfile::tempdir().unwrap();
    let secret = scratch.path().join("secret.txt");
    let escape = scratch.path().join("escaped.txt");
    std::fs::write(&secret, "read boundary").unwrap();

    let write_sid = workspace_write_sid(&workspace.to_string_lossy());
    let mut sandbox = AclSandbox::new(
        &options(&workspace, Some(private_temp.path()), &write_sid),
        binding(),
    )
    .unwrap();
    sandbox.init(std::process::id()).unwrap();

    let script = format!(
        "$ErrorActionPreference='SilentlyContinue';\
         try{{Set-Content -LiteralPath '{}' -Value ok -ErrorAction Stop;'TARGET-WRITE: OK'}}catch{{'TARGET-WRITE: DENIED'}};\
         try{{Set-Content -LiteralPath '{}' -Value ok -ErrorAction Stop;'TEMP-WRITE: OK'}}catch{{'TEMP-WRITE: DENIED'}};\
         try{{Set-Content -LiteralPath '{}' -Value ok -ErrorAction Stop;'ESCAPE-WRITE: OK (ESCAPE!)'}}catch{{'ESCAPE-WRITE: DENIED'}};\
         try{{Get-Content -LiteralPath '{}' -ErrorAction Stop | Out-Null;'SECRET-READ: OK'}}catch{{'SECRET-READ: DENIED'}}",
        ps_literal(&workspace.join("child-wrote.txt")),
        ps_literal(&private_temp.path().join("child-wrote.txt")),
        ps_literal(&escape),
        ps_literal(&secret),
    );
    let args = [
        "/NoLogo".to_owned(),
        "/NonInteractive".to_owned(),
        "/NoProfile".to_owned(),
        "/Command".to_owned(),
        script,
    ];
    let child = sandbox
        .spawn(pwsh, &args, &workspace, SandboxStdio::Pipe)
        .unwrap();
    let result = child.wait().await;
    let dispose = sandbox.dispose();
    let result = result.unwrap();
    dispose.unwrap();
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );

    assert_eq!(result.exit_code, 0, "child output:\n{output}");
    assert!(output.contains("TARGET-WRITE: OK"), "{output}");
    assert!(output.contains("TEMP-WRITE: OK"), "{output}");
    assert!(output.contains("ESCAPE-WRITE: DENIED"), "{output}");
    assert!(output.contains("SECRET-READ: OK"), "{output}");
    assert!(!escape.exists());
    assert!(workspace.join("child-wrote.txt").exists());
}

#[test]
fn malformed_write_sid_fails_closed_without_an_unrestricted_fallback() {
    let workspace = tempfile::tempdir().unwrap();
    let mut sandbox =
        AclSandbox::new(&options(workspace.path(), None, "S-1-4-abc-1"), binding()).unwrap();

    let error = sandbox.init(std::process::id()).unwrap_err();
    assert!(error.to_string().contains("ConvertStringSidToSidW"));
    assert_eq!(sandbox.temp_dir(), &AclTempDirState::Unresolved);
    assert!(
        sandbox
            .spawn("cmd.exe", &[], workspace.path(), SandboxStdio::Pipe)
            .unwrap_err()
            .to_string()
            .contains("is not initialized")
    );
}

#[test]
fn failed_init_clears_provisional_private_temp_state_before_retry() {
    let workspace = tempfile::tempdir().unwrap();
    let private_temp = tempfile::tempdir().unwrap();
    let mut sandbox = AclSandbox::new(
        &options(workspace.path(), Some(private_temp.path()), "S-1-4-abc-1"),
        binding(),
    )
    .unwrap();

    let error = sandbox.init(std::process::id()).unwrap_err();
    assert!(error.to_string().contains("ConvertStringSidToSidW"));
    assert_eq!(sandbox.temp_dir(), &AclTempDirState::Unresolved);
}
