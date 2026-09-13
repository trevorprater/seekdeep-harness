//! Real bwrap confinement world proof on Linux hosts that support user namespaces.

#![cfg(target_os = "linux")]

use std::{fs, path::Path, process::Command, time::Duration};

use seekdeep_sandbox::{ConfinedSandboxMode, SandboxEnforcement, SandboxPolicy, SandboxProvider};
use seekdeep_sandbox_local::{LocalSandboxConfig, LocalSandboxProvider, bwrap_profile_args};

fn policy(mode: ConfinedSandboxMode, workspace: &Path) -> SandboxPolicy {
    SandboxPolicy {
        mode,
        workspace_root: workspace.to_owned(),
        session_id: None,
    }
}

fn bwrap_usable() -> bool {
    let probe_policy = policy(ConfinedSandboxMode::ReadOnly, Path::new("/"));
    let Ok(args) = bwrap_profile_args(&probe_policy) else {
        return false;
    };
    Command::new("bwrap")
        .args(args)
        .args(["--", "true"])
        .spawn()
        .and_then(|mut child| {
            let start = std::time::Instant::now();
            loop {
                if let Some(status) = child.try_wait()? {
                    return Ok(status.success());
                }
                if start.elapsed() >= Duration::from_secs(5) {
                    child.kill()?;
                    child.wait()?;
                    return Ok(false);
                }
                std::thread::yield_now();
            }
        })
        .unwrap_or(false)
}

fn run(
    provider: &LocalSandboxProvider,
    script: &str,
    policy: &SandboxPolicy,
) -> (std::process::Output, seekdeep_sandbox::ConfinedArgv) {
    let confined = provider
        .confine(
            &["bash".to_owned(), "-c".to_owned(), script.to_owned()],
            policy,
        )
        .unwrap();
    let output = Command::new(&confined.argv[0])
        .args(&confined.argv[1..])
        .output()
        .unwrap();
    (output, confined)
}

#[test]
fn real_bwrap_denies_read_only_and_grants_workspace_with_ephemeral_tmp() {
    if !bwrap_usable() {
        assert_ne!(
            std::env::var("SEEKDEEP_REQUIRE_BWRAP").as_deref(),
            Ok("1"),
            "SEEKDEEP_REQUIRE_BWRAP=1 but bwrap/user namespaces are unusable"
        );
        return;
    }
    let provider = LocalSandboxProvider::new(&LocalSandboxConfig::default()).unwrap();
    let work = tempfile::tempdir().unwrap();
    let read_only = policy(ConfinedSandboxMode::ReadOnly, work.path());
    let denied_path = work.path().join("denied.txt");
    let (denied, confined) = run(
        &provider,
        &format!("echo hi > {}", denied_path.display()),
        &read_only,
    );
    assert!(!denied.status.success());
    assert!(!denied_path.exists());
    assert_eq!(confined.argv[0], "bwrap");
    assert_eq!(confined.enforcement, SandboxEnforcement::Full);
    assert_eq!(confined.denial_signatures, ["read-only file system"]);
    assert!(
        String::from_utf8_lossy(&denied.stderr)
            .to_lowercase()
            .contains("read-only file system")
    );

    let (readable, _) = run(&provider, "ls / > /dev/null && echo dev-ok", &read_only);
    assert!(readable.status.success());
    assert_eq!(readable.stdout, b"dev-ok\n");

    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap();
    let workspace = tempfile::Builder::new()
        .prefix("seekdeep-bwrap-work-")
        .tempdir_in(&home)
        .unwrap();
    let outside = tempfile::Builder::new()
        .prefix("seekdeep-bwrap-outside-")
        .tempdir_in(&home)
        .unwrap();
    let workspace_write = policy(ConfinedSandboxMode::WorkspaceWrite, workspace.path());
    let inside_path = workspace.path().join("allowed.txt");
    let (inside, _) = run(
        &provider,
        &format!("printf bwrap-ok > {}", inside_path.display()),
        &workspace_write,
    );
    assert!(inside.status.success(), "{:?}", inside.stderr);
    assert_eq!(fs::read_to_string(inside_path).unwrap(), "bwrap-ok");
    let outside_path = outside.path().join("denied.txt");
    let (outside_denied, _) = run(
        &provider,
        &format!("echo hi > {}", outside_path.display()),
        &workspace_write,
    );
    assert!(!outside_denied.status.success());
    assert!(!outside_path.exists());

    let host_tmp_marker = std::env::temp_dir().join(format!(
        "seekdeep-bwrap-ephemeral-{}.txt",
        std::process::id()
    ));
    let _ = fs::remove_file(&host_tmp_marker);
    let (temporary, _) = run(
        &provider,
        &format!(
            "printf tmp-ok > {} && cat {}",
            host_tmp_marker.display(),
            host_tmp_marker.display()
        ),
        &workspace_write,
    );
    assert!(temporary.status.success());
    assert_eq!(temporary.stdout, b"tmp-ok");
    assert!(!host_tmp_marker.exists());
}
