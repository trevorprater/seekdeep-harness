//! Legacy flag validation happens before SDK imports, listening, or process launch.

use std::process::Command;

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_smoke-python-runtime")
}

#[test]
fn flag_dependencies_and_abbreviations_keep_source_error_boundaries() {
    for (arguments, message) in [
        (
            vec![],
            "--exe is required for custom, minimal, snapshot, and direct scenarios",
        ),
        (vec!["--scenario", "direct"], "--exe is required"),
        (vec!["--sce", "sdk-custom"], "--exe is required"),
        (
            vec!["--scenario", "sdk-default", "--update-snapshots"],
            "--update-snapshots requires",
        ),
        (
            vec![
                "--scenario",
                "sdk-default",
                "--exe",
                "/seekdeep/absent/runtime",
            ],
            "runtime executable does not exist",
        ),
        (
            vec!["--scenario", "sdk-default", "--scenario", "direct"],
            "--exe is required",
        ),
    ] {
        let output = Command::new(binary()).args(arguments).output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(message),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let help = Command::new(binary()).arg("--help").output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    for flag in [
        "--scenario",
        "--exe",
        "--update-snapshots",
        "--python",
        "--root",
    ] {
        assert!(help.contains(flag));
    }
}

#[cfg(unix)]
#[test]
fn interrupt_waits_for_owned_runtime_cleanup_before_exiting() {
    use std::{
        os::unix::fs::PermissionsExt as _,
        process::Stdio,
        time::{Duration, Instant},
    };

    struct ChildGuard(Option<std::process::Child>);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            if let Some(child) = &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let temporary = tempfile::tempdir().unwrap();
    let executable = temporary.path().join("fake-runtime");
    let ready = temporary.path().join("ready");
    let closed = temporary.path().join("closed");
    std::fs::write(
        &executable,
        r"#!/usr/bin/env python3
import json, os, pathlib, sys
for line in sys.stdin:
    message = json.loads(line)
    if message['method'] == 'initialize':
        print(json.dumps({'jsonrpc':'2.0','id':message['id'],'result':{}}),flush=True)
    elif message['method'] == 'session/prompt':
        pathlib.Path(os.environ['SEEKDEEP_SMOKE_TEST_READY']).write_text(str(os.getpid()))
pathlib.Path(os.environ['SEEKDEEP_SMOKE_TEST_CLOSED']).write_text('closed')
",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let child = Command::new(binary())
        .args(["--scenario", "direct", "--exe"])
        .arg(&executable)
        .arg("--root")
        .arg(temporary.path())
        .env("SEEKDEEP_SMOKE_TEST_READY", &ready)
        .env("SEEKDEEP_SMOKE_TEST_CLOSED", &closed)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut child = ChildGuard(Some(child));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.is_file() {
        assert!(Instant::now() < deadline, "runtime did not reach prompt");
        assert!(
            child.0.as_mut().unwrap().try_wait().unwrap().is_none(),
            "CLI exited before prompt"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let runtime_pid = std::fs::read_to_string(&ready).unwrap();
    assert!(
        Command::new("kill")
            .args(["-INT", &child.0.as_ref().unwrap().id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    while child.0.as_mut().unwrap().try_wait().unwrap().is_none() {
        assert!(
            Instant::now() < deadline,
            "interrupted CLI did not finish cleanup"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let output = child.0.take().unwrap().wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(130),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(closed.is_file());
    assert!(
        !Command::new("kill")
            .args(["-0", runtime_pid.trim()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success(),
        "runtime remains alive after CLI exit"
    );
}
