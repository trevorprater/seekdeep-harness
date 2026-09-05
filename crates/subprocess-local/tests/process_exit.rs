#![cfg(unix)]
//! End-to-end verification of synchronous managed-tree cleanup at host exit.

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use nix::{
    errno::Errno,
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use seekdeep_cordis::Context;
use seekdeep_subprocess::{
    SubprocessCollect, SubprocessOutputMode, SubprocessRuntime as _, SubprocessSpawnSpec,
    SubprocessStdinMode, SubprocessStdio, SubprocessTerminalSpawnSpec,
};
use seekdeep_subprocess_local::LocalSubprocessRuntime;

const ROOT_ENV: &str = "SEEKDEEP_PROCESS_EXIT_FIXTURE_ROOT";
const KIND_ENV: &str = "SEEKDEEP_PROCESS_EXIT_FIXTURE_KIND";

#[tokio::test]
#[ignore = "invoked as the child half of the process-exit integration test"]
async fn process_exit_fixture_child() {
    let root = PathBuf::from(std::env::var_os(ROOT_ENV).expect("fixture root"));
    let state = root.join("tree.txt");
    let context = Context::new();
    let runtime = LocalSubprocessRuntime::install(&context).expect("runtime install");
    let argv = vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        "trap '' TERM HUP; sleep 60 & printf '%s %s' \"$$\" \"$!\" > \"$1\"; wait".to_owned(),
        "managed-tree".to_owned(),
        state.to_string_lossy().into_owned(),
    ];
    if std::env::var(KIND_ENV).as_deref() == Ok("terminal") {
        runtime
            .spawn_terminal(SubprocessTerminalSpawnSpec {
                argv,
                cwd: root.clone(),
                env: None,
                rows: 24,
                cols: 80,
                grace_ms: 30_000.0,
                signal: None,
            })
            .await
            .expect("managed terminal spawn");
    } else {
        runtime
            .spawn(SubprocessSpawnSpec {
                argv,
                cwd: root.clone(),
                stdio: SubprocessStdio {
                    stdin: SubprocessStdinMode::Ignore,
                    stdout: SubprocessOutputMode::Collect(SubprocessCollect {
                        max_bytes: 1024.0,
                        spill: None,
                    }),
                    stderr: SubprocessOutputMode::Collect(SubprocessCollect {
                        max_bytes: 1024.0,
                        spill: None,
                    }),
                },
                grace_ms: 30_000.0,
                signal: None,
                env: None,
            })
            .expect("managed spawn");
    }
    wait_for_path(&state, Duration::from_secs(10));
    fs::write(root.join("ready"), b"ready").expect("ready marker");
    wait_for_path(&root.join("proceed"), Duration::from_secs(10));
    std::process::exit(23);
}

#[test]
fn native_process_exit_hook_force_stops_the_managed_tree() {
    run_native_process_exit_scenario("ordinary");
}

#[test]
fn native_process_exit_hook_force_stops_the_managed_terminal_tree() {
    run_native_process_exit_scenario("terminal");
}

fn run_native_process_exit_scenario(kind: &str) {
    let temp = tempfile::tempdir().unwrap();
    let executable = std::env::current_exe().unwrap();
    let mut child = Command::new(executable)
        .args([
            "--ignored",
            "--exact",
            "process_exit_fixture_child",
            "--nocapture",
        ])
        .env(ROOT_ENV, temp.path())
        .env(KIND_ENV, kind)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let state_path = temp.path().join("tree.txt");
    let mut identities = Vec::new();
    let outcome = run_parent_scenario(&mut child, temp.path(), &state_path, &mut identities);
    if outcome.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        for pid in identities {
            let _ = kill(Pid::from_raw(pid), Some(Signal::SIGKILL));
        }
    }
    outcome.unwrap();
}

fn run_parent_scenario(
    child: &mut std::process::Child,
    root: &Path,
    state_path: &Path,
    identities: &mut Vec<i32>,
) -> anyhow::Result<()> {
    wait_for_path(&root.join("ready"), Duration::from_secs(15));
    let state = fs::read_to_string(state_path)?;
    *identities = state
        .split_whitespace()
        .map(str::parse::<i32>)
        .collect::<Result<Vec<_>, _>>()?;
    anyhow::ensure!(identities.len() == 2, "expected root and descendant pids");
    anyhow::ensure!(identities[0] != identities[1], "managed pids must differ");
    fs::write(root.join("proceed"), b"proceed")?;
    let status = child.wait()?;
    anyhow::ensure!(
        status.code() == Some(23),
        "unexpected child status {status}"
    );
    for pid in identities {
        wait_for_gone(*pid, Duration::from_secs(10));
    }
    Ok(())
}

fn wait_for_path(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("{} was not created before timeout", path.display());
}

fn wait_for_gone(pid: i32, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match kill(Pid::from_raw(pid), None) {
            Err(Errno::ESRCH) => return,
            Ok(()) | Err(Errno::EPERM) => {}
            Err(error) => panic!("pid probe failed: {error}"),
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("managed pid {pid} remained alive after process exit");
}
