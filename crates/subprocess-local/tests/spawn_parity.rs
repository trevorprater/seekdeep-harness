#![cfg(unix)]
//! Native ordinary-process parity tests against the source spawn contract.

use std::{
    collections::BTreeMap,
    os::unix::fs::PermissionsExt as _,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
use seekdeep_subprocess::{
    ProcessId, SubprocessCollect, SubprocessHandle as _, SubprocessOutputMode, SubprocessSpawnSpec,
    SubprocessSpill, SubprocessStdinMode, SubprocessStdio,
};
use seekdeep_subprocess_local::{
    SpawnInternals, SpawnPlatform, kill_group, spawn_subprocess, spawn_subprocess_with,
    taskkill_process_tree,
};
use tokio::io::AsyncReadExt as _;

fn spec(script: &str, cwd: &Path) -> SubprocessSpawnSpec {
    SubprocessSpawnSpec {
        argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
        cwd: cwd.to_path_buf(),
        stdio: SubprocessStdio {
            stdin: SubprocessStdinMode::Ignore,
            stdout: collect(64_000.0, Some(64.0 * 1024.0 * 1024.0)),
            stderr: collect(64_000.0, Some(64.0 * 1024.0 * 1024.0)),
        },
        grace_ms: 200.0,
        signal: None,
        env: None,
    }
}

fn collect(max_bytes: f64, spill: Option<f64>) -> SubprocessOutputMode {
    SubprocessOutputMode::Collect(SubprocessCollect {
        max_bytes,
        spill: spill.map(|max_bytes| SubprocessSpill { max_bytes }),
    })
}

#[tokio::test]
async fn raw_pipe_stdio_is_exposed_without_a_collector() {
    let temp = tempfile::tempdir().unwrap();
    let mut request = spec("cat", temp.path());
    request.stdio.stdin = SubprocessStdinMode::Pipe;
    request.stdio.stdout = SubprocessOutputMode::Pipe;
    let running = spawn_subprocess(request, Some(temp.path())).unwrap();
    assert!(running.collected().stdout.is_none());
    let input = running.stdin().expect("stdin pipe");
    input.write_all(b"through the pipe\n").await.unwrap();
    input.close().await.unwrap();
    let output = running.stdout().expect("stdout pipe");
    let mut text = String::new();
    output.lock().await.read_to_string(&mut text).await.unwrap();
    assert_eq!(running.done().await.unwrap().exit_code, Some(0));
    assert_eq!(text, "through the pipe\n");
}

#[tokio::test]
async fn child_stdio_has_the_source_posix_device_and_socket_types() {
    let temp = tempfile::tempdir().unwrap();
    let request = spec(
        "test -c /dev/stdin && printf 'stdin:char\n' || printf 'stdin:other\n'; test -S /dev/stdout && printf 'stdout:socket\n' || printf 'stdout:other\n'; test -S /dev/stderr && printf 'stderr:socket\n' >&2 || printf 'stderr:other\n' >&2",
        temp.path(),
    );
    let running = spawn_subprocess(request, Some(temp.path())).unwrap();
    running.done().await.unwrap();
    assert_eq!(
        running.collected().stdout.unwrap().read_from(0).text,
        "stdin:char\nstdout:socket\n"
    );
    assert_eq!(
        running.collected().stderr.unwrap().read_from(0).text,
        "stderr:socket\n"
    );

    let mut request = spec(
        "test -S /dev/stdin && printf 'stdin:socket\n' || printf 'stdin:other\n'",
        temp.path(),
    );
    request.stdio.stdin = SubprocessStdinMode::Data("batch\n".to_owned());
    let running = spawn_subprocess(request, Some(temp.path())).unwrap();
    running.done().await.unwrap();
    assert_eq!(
        running.collected().stdout.unwrap().read_from(0).text,
        "stdin:socket\n"
    );
}

#[tokio::test]
async fn collection_keeps_an_exact_tail_and_complete_private_spill() {
    let temp = tempfile::tempdir().unwrap();
    let mut request = spec(
        "i=1; while [ $i -le 200 ]; do printf 'line-%04d\\n' $i; i=$((i+1)); done",
        temp.path(),
    );
    request.stdio.stdout = collect(500.0, Some(64_000.0));
    let running = spawn_subprocess(request, Some(temp.path())).unwrap();
    assert_eq!(running.done().await.unwrap().exit_code, Some(0));
    let read = running.collected().stdout.unwrap().read_from(0);
    assert!(read.lossy);
    assert!(read.text.len() <= 500);
    assert!(read.text.contains("line-0200"));
    assert!(!read.text.contains("line-0001"));
    let spill = read.spill_path.unwrap();
    let full = std::fs::read_to_string(&spill).unwrap();
    assert!(full.contains("line-0001"));
    assert!(full.contains("line-0200"));
    assert_eq!(
        std::fs::metadata(spill).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[tokio::test]
async fn explicit_environment_tombstones_and_sensitive_overrides_apply_after_scrub() {
    let temp = tempfile::tempdir().unwrap();
    let mut request = spec(
        "printf '[%s|%s|%s]' \"${HOME-unset}\" \"$EXPLICIT_OVERRIDE_PASSWORD\" \"$SEEKDEEP_SESSION_ID\"",
        temp.path(),
    );
    request.env = Some(BTreeMap::from([
        ("HOME".to_owned(), None),
        (
            "EXPLICIT_OVERRIDE_PASSWORD".to_owned(),
            Some("deliberate-secret".to_owned()),
        ),
        (
            "SEEKDEEP_SESSION_ID".to_owned(),
            Some("current-session".to_owned()),
        ),
    ]));
    let running = spawn_subprocess(request, Some(temp.path())).unwrap();
    running.done().await.unwrap();
    assert_eq!(
        running.collected().stdout.unwrap().read_from(0).text,
        "[unset|deliberate-secret|current-session]"
    );
}

#[tokio::test]
async fn large_best_effort_batch_stdin_does_not_replace_the_exit_outcome() {
    let temp = tempfile::tempdir().unwrap();
    let mut request = spec("exit 7", temp.path());
    request.stdio.stdin = SubprocessStdinMode::Data("x".repeat(4 * 1024 * 1024));
    let running = spawn_subprocess(request, Some(temp.path())).unwrap();
    assert_eq!(running.done().await.unwrap().exit_code, Some(7));
}

#[tokio::test]
async fn caps_are_independent_exact_and_optional_spill_is_not_implied() {
    let temp = tempfile::tempdir().unwrap();
    let mut request = spec(
        "printf '%0500d' 0 | tr '0' x; printf '%0500d' 0 | tr '0' e >&2",
        temp.path(),
    );
    request.stdio.stdout = collect(500.0, Some(4_000.0));
    request.stdio.stderr = collect(100.0, Some(4_000.0));
    let running = spawn_subprocess(request, Some(temp.path())).unwrap();
    running.done().await.unwrap();
    let stdout = running.collected().stdout.unwrap().read_from(0);
    let stderr = running.collected().stderr.unwrap().read_from(0);
    assert_eq!(stdout.text, "x".repeat(500));
    assert!(!stdout.lossy);
    assert!(stdout.spill_path.is_none());
    assert_eq!(stderr.text, "e".repeat(100));
    assert!(stderr.lossy);
    assert!(stderr.spill_path.is_some());

    let mut request = spec("printf '%0200d' 0 | tr '0' z", temp.path());
    request.stdio.stdout = collect(32.0, None);
    let running = spawn_subprocess(request, Some(temp.path())).unwrap();
    running.done().await.unwrap();
    let output = running.collected().stdout.unwrap().read_from(0);
    assert_eq!(output.text, "z".repeat(32));
    assert!(output.lossy);
    assert!(output.spill_path.is_none());
}

#[tokio::test]
async fn inherit_and_collect_dispositions_are_wired_independently() {
    let temp = tempfile::tempdir().unwrap();
    let mut request = spec("printf 'to-parent'; printf 'captured-err' >&2", temp.path());
    request.stdio.stdout = SubprocessOutputMode::Inherit;
    let running = spawn_subprocess(request, Some(temp.path())).unwrap();
    running.done().await.unwrap();
    assert!(running.stdout().is_none());
    assert!(running.collected().stdout.is_none());
    assert_eq!(
        running.collected().stderr.unwrap().read_from(0).text,
        "captured-err"
    );

    let mut request = spec("printf 'captured-out'; printf 'to-parent' >&2", temp.path());
    request.stdio.stderr = SubprocessOutputMode::Inherit;
    let running = spawn_subprocess(request, Some(temp.path())).unwrap();
    running.done().await.unwrap();
    assert!(running.stderr().is_none());
    assert!(running.collected().stderr.is_none());
    assert_eq!(
        running.collected().stdout.unwrap().read_from(0).text,
        "captured-out"
    );
}

#[tokio::test]
async fn default_spills_use_private_directory_and_random_owner_only_files() {
    let temp = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for _ in 0..2 {
        let mut request = spec("printf 'abcdefgh'", temp.path());
        request.stdio.stdout = collect(4.0, Some(100.0));
        let running = spawn_subprocess(request, None).unwrap();
        running.done().await.unwrap();
        paths.push(
            running
                .collected()
                .stdout
                .unwrap()
                .read_from(0)
                .spill_path
                .unwrap(),
        );
    }
    assert_ne!(paths[0], paths[1]);
    assert_eq!(paths[0].parent(), paths[1].parent());
    assert_eq!(
        std::fs::metadata(paths[0].parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    for path in paths {
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_file(path).unwrap();
    }
}

#[tokio::test]
async fn argv_is_verbatim_and_a_self_signal_is_reported_without_classification() {
    let temp = tempfile::tempdir().unwrap();
    let mut request = spec("unused", temp.path());
    request.argv = vec![
        "/usr/bin/printf".to_owned(),
        "%s".to_owned(),
        "$HOME".to_owned(),
    ];
    let running = spawn_subprocess(request, Some(temp.path())).unwrap();
    assert_eq!(running.done().await.unwrap().exit_code, Some(0));
    assert_eq!(
        running.collected().stdout.unwrap().read_from(0).text,
        "$HOME"
    );

    let running = spawn_subprocess(spec("kill -TERM $$", temp.path()), Some(temp.path())).unwrap();
    let outcome = running.done().await.unwrap();
    assert_eq!(outcome.exit_code, None);
    assert_eq!(outcome.signal.unwrap().as_str(), "SIGTERM");
}

#[tokio::test]
async fn terminate_reaches_the_group_and_escalates_a_term_trap() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("tree.pid");
    let mut request = spec(
        "trap '' TERM; sleep 60 & printf '%s' \"$!\" > \"$1\"; wait",
        temp.path(),
    );
    request.argv.extend([
        "managed-tree".to_owned(),
        state.to_string_lossy().into_owned(),
    ]);
    request.grace_ms = 100.0;
    let running = spawn_subprocess(request, Some(temp.path())).unwrap();
    let descendant = wait_for_pid_file(&state).await;
    running.terminate();
    let outcome = tokio::time::timeout(Duration::from_secs(5), running.done())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.signal.unwrap().as_str(), "SIGKILL");
    assert!(running.wait_for_exit(None).await.unwrap());
    wait_for_gone(descendant).await;
}

#[tokio::test]
async fn collected_pipe_drain_is_bounded_after_the_leader_exits() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("holder.pid");
    let mut request = spec(
        "sleep 60 & printf '%s' \"$!\" > \"$1\"; printf 'shell-done\\n'",
        temp.path(),
    );
    request.argv.extend([
        "pipe-holder".to_owned(),
        state.to_string_lossy().into_owned(),
    ]);
    request.grace_ms = 100.0;
    let started = tokio::time::Instant::now();
    let running = spawn_subprocess(request, Some(temp.path())).unwrap();
    let descendant = wait_for_pid_file(&state).await;
    let outcome = running.done().await.unwrap();
    assert_eq!(outcome.exit_code, Some(0));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        running.collected().stdout.unwrap().read_from(0).text,
        "shell-done\n"
    );
    running.terminate();
    assert!(running.wait_for_exit(None).await.unwrap());
    wait_for_gone(descendant).await;
}

#[tokio::test]
async fn an_aborted_whole_tree_wait_reports_false_then_termination_quiesces() {
    let temp = tempfile::tempdir().unwrap();
    let running = spawn_subprocess(spec("sleep 60", temp.path()), Some(temp.path())).unwrap();
    let signal = seekdeep_llm::AbortSignal::default();
    signal.abort();
    assert!(!running.wait_for_exit(Some(signal)).await.unwrap());
    running.terminate();
    assert_eq!(
        running.done().await.unwrap().signal.unwrap().as_str(),
        "SIGTERM"
    );
    assert!(running.wait_for_exit(None).await.unwrap());
}

#[tokio::test]
async fn injected_windows_host_exit_routes_immediately_through_taskkill() {
    let temp = tempfile::tempdir().unwrap();
    let killed = Arc::new(Mutex::new(Vec::new()));
    let captured = killed.clone();
    let running = spawn_subprocess_with(
        spec("exec sleep 60", temp.path()),
        SpawnInternals {
            spill_dir: Some(temp.path().to_path_buf()),
            platform: Some(SpawnPlatform::Windows),
            taskkill: Some(Arc::new(move |pid| {
                captured.lock().unwrap().push(pid);
                kill_direct(pid);
            })),
            linux_group_has_live_members: None,
        },
    )
    .unwrap();
    running.terminate_for_host_exit();
    let outcome = running.done().await.unwrap();
    assert_eq!(outcome.signal.unwrap().as_str(), "SIGKILL");
    assert_eq!(*killed.lock().unwrap(), vec![running.pid()]);
}

#[tokio::test]
async fn injected_windows_terminate_routes_by_root_pid() {
    let temp = tempfile::tempdir().unwrap();
    let killed = Arc::new(Mutex::new(Vec::new()));
    let captured = killed.clone();
    let running = spawn_subprocess_with(
        spec("exec sleep 60", temp.path()),
        SpawnInternals {
            spill_dir: Some(temp.path().to_path_buf()),
            platform: Some(SpawnPlatform::Windows),
            taskkill: Some(Arc::new(move |pid| {
                captured.lock().unwrap().push(pid);
                kill_direct(pid);
            })),
            linux_group_has_live_members: None,
        },
    )
    .unwrap();
    running.terminate();
    let outcome = running.done().await.unwrap();
    assert_eq!(outcome.signal.unwrap().as_str(), "SIGKILL");
    assert!(killed.lock().unwrap().contains(&running.pid()));
}

#[tokio::test]
async fn injected_windows_wait_uses_direct_child_liveness() {
    let temp = tempfile::tempdir().unwrap();
    let running = spawn_subprocess_with(
        spec("true", temp.path()),
        SpawnInternals {
            spill_dir: Some(temp.path().to_path_buf()),
            platform: Some(SpawnPlatform::Windows),
            taskkill: Some(Arc::new(|_| {})),
            linux_group_has_live_members: None,
        },
    )
    .unwrap();
    running.done().await.unwrap();
    assert!(running.wait_for_exit(None).await.unwrap());
}

#[tokio::test]
async fn host_exit_after_direct_child_exit_does_not_retarget_the_pid() {
    let temp = tempfile::tempdir().unwrap();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let captured = calls.clone();
    let running = spawn_subprocess_with(
        spec("true", temp.path()),
        SpawnInternals {
            spill_dir: Some(temp.path().to_path_buf()),
            platform: Some(SpawnPlatform::Windows),
            taskkill: Some(Arc::new(move |_| {
                captured.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            })),
            linux_group_has_live_members: None,
        },
    )
    .unwrap();
    running.done().await.unwrap();
    running.terminate_for_host_exit();
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 0);
}

#[tokio::test]
async fn inert_injected_windows_taskkill_leaves_live_child_for_bounded_wait() {
    let temp = tempfile::tempdir().unwrap();
    let running = spawn_subprocess_with(
        spec("sleep 60", temp.path()),
        SpawnInternals {
            spill_dir: Some(temp.path().to_path_buf()),
            platform: Some(SpawnPlatform::Windows),
            taskkill: Some(Arc::new(|_| {})),
            linux_group_has_live_members: None,
        },
    )
    .unwrap();
    running.terminate();
    let bound = seekdeep_llm::AbortSignal::default();
    let abort = bound.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(60)).await;
        abort.abort();
    });
    assert!(!running.wait_for_exit(Some(bound)).await.unwrap());
    kill_direct(running.pid());
    running.done().await.unwrap();
}

#[tokio::test]
async fn injected_linux_live_member_probe_can_classify_a_zombie_only_group() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("probe-descendant.pid");
    let mut request = spec("sleep 60 & printf '%s' \"$!\" > \"$1\"", temp.path());
    request.argv.extend([
        "probe-group".to_owned(),
        state.to_string_lossy().into_owned(),
    ]);
    request.grace_ms = 200.0;
    let probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let captured = probes.clone();
    let running = spawn_subprocess_with(
        request,
        SpawnInternals {
            spill_dir: Some(temp.path().to_path_buf()),
            platform: Some(SpawnPlatform::Linux),
            taskkill: None,
            linux_group_has_live_members: Some(Arc::new(move |_| {
                captured.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Some(false)
            })),
        },
    )
    .unwrap();
    let descendant = wait_for_pid_file(&state).await;
    let bound = seekdeep_llm::AbortSignal::default();
    let abort = bound.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        abort.abort();
    });
    assert!(!running.wait_for_exit(Some(bound)).await.unwrap());
    assert_eq!(probes.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(running.done().await.unwrap().exit_code, Some(0));
    assert!(running.wait_for_exit(None).await.unwrap());
    assert!(probes.load(std::sync::atomic::Ordering::Relaxed) > 0);
    let _ = kill(Pid::from_raw(descendant), nix::sys::signal::Signal::SIGKILL);
    wait_for_gone(descendant).await;
}

fn kill_direct(pid: ProcessId) {
    let Ok(pid) = i32::try_from(pid.as_i64()) else {
        return;
    };
    let _ = nix::sys::signal::kill(Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL);
}

async fn wait_for_pid_file(path: &Path) -> i32 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(text) = tokio::fs::read_to_string(path).await
            && let Ok(pid) = text.trim().parse::<i32>()
            && pid > 0
        {
            return pid;
        }
        assert!(tokio::time::Instant::now() < deadline, "pid file timeout");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_for_gone(pid: i32) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match kill(Pid::from_raw(pid), None) {
            Err(Errno::ESRCH) => return,
            Ok(()) | Err(Errno::EPERM) => {}
            Err(error) => panic!("pid probe failed: {error}"),
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "pid {pid} remained alive"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[test]
fn failed_spawn_uses_the_public_pid_sentinel() {
    assert_eq!(ProcessId::new(-1).as_i64(), -1);
}

#[test]
fn contained_tree_signal_helpers_ignore_invalid_and_absent_targets() {
    let term = seekdeep_subprocess::ProcessSignal::new("SIGTERM");
    kill_group(ProcessId::new(-1), &term);
    kill_group(ProcessId::new(0), &term);
    kill_group(ProcessId::new(1_073_741_824), &term);
    kill_group(
        ProcessId::new(1_073_741_824),
        &seekdeep_subprocess::ProcessSignal::new("NOT_A_SIGNAL"),
    );
    taskkill_process_tree(ProcessId::new(-1));
    taskkill_process_tree(ProcessId::new(0));
    taskkill_process_tree(ProcessId::new(1_073_741_824));
}
