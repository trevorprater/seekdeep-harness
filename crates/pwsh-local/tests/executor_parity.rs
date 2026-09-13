#![cfg(not(windows))]

//! Real local-process parity for the Pwsh execution provider.

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt as _,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use seekdeep_cordis::{Context, Fiber};
use seekdeep_llm::AbortSignal;
use seekdeep_pwsh_local::{
    Config, ENCODING_PREAMBLE, LocalPwshExecutor, apply, assert_serviceable_pwsh_config,
};
use seekdeep_shell::{
    ProcessSignal, ShellExecRequest, ShellExecutor, ShellProcessHandle, ShellProcessStatus,
};
use seekdeep_subprocess::{SeekDeepEnvironment, SeekDeepEnvironmentKey};
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use tempfile::TempDir;

struct Harness {
    _root: Context,
    _spill: TempDir,
    runtime: Arc<LocalSubprocessRuntime>,
    pwsh: Arc<LocalPwshExecutor>,
}

fn pwsh_shim(directory: &Path) -> PathBuf {
    let path = directory.join("pwsh-test-shim");
    let script = format!(
        "#!/bin/bash\nprefix='{ENCODING_PREAMBLE}'\nlast=\"${{@: -1}}\"\ncommand=\"${{last#\"$prefix\"}}\"\nexec /bin/bash -c \"$command\"\n"
    );
    fs::write(&path, script).expect("write pwsh shim");
    let mut permissions = fs::metadata(&path).expect("shim metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("make pwsh shim executable");
    path
}

async fn setup(mut config: Config) -> Harness {
    config.grace_ms = 200.0;
    let root = Context::new();
    let spill = tempfile::tempdir().expect("spill directory");
    config.pwsh_path = Some(pwsh_shim(spill.path()).to_string_lossy().into_owned());
    let runtime = LocalSubprocessRuntime::install_runtime(
        &root,
        Arc::new(LocalSubprocessRuntime::with_spill_dir(spill.path())),
    )
    .expect("subprocess");
    let pwsh = apply(&root, config).await.expect("pwsh");
    Harness {
        _root: root,
        _spill: spill,
        runtime,
        pwsh,
    }
}

async fn read_until(process: &ShellProcessHandle, expected: &str) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut output = String::new();
    while tokio::time::Instant::now() < deadline {
        output.push_str(&process.read_output().delta);
        if output.contains(expected) {
            return output;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("process output never contained {expected:?}: {output:?}");
}

fn request(command: &str) -> ShellExecRequest {
    ShellExecRequest::new(command)
}

fn assert_number(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < f64::EPSILON,
        "expected {expected}, got {actual}"
    );
}

#[tokio::test]
async fn foreground_resolution_output_limits_and_environment_match_the_source() {
    let temporary = tempfile::tempdir().expect("cwd");
    let harness = setup(Config {
        cwd: Some(temporary.path().to_string_lossy().into_owned()),
        timeout_ms: 5_000.0,
        max_timeout_ms: 6_000.0,
        max_output_bytes: 100.0,
        ..Config::default()
    })
    .await;

    let result = harness
        .pwsh
        .run(harness.pwsh.resolve(request("echo hi")).expect("resolve"))
        .await
        .expect("run");
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout.text, "hi\n");
    assert_number(result.timeout_ms, 5_000.0);

    let pwd = harness
        .pwsh
        .run(harness.pwsh.resolve(request("pwd")).expect("resolve"))
        .await
        .expect("pwd");
    assert_eq!(
        Path::new(pwd.stdout.text.trim()).canonicalize().unwrap(),
        temporary.path().canonicalize().unwrap()
    );
    let mut root_request = request("pwd");
    root_request.workdir = Some("/".into());
    let root = harness
        .pwsh
        .run(harness.pwsh.resolve(root_request).expect("resolve root"))
        .await
        .expect("root pwd");
    assert_eq!(root.stdout.text.trim(), "/");

    let mut capped = request("true");
    capped.timeout_ms = Some(99_999.0);
    assert_number(
        harness.pwsh.resolve(capped).expect("capped").timeout_ms,
        6_000.0,
    );
    assert_number(
        harness
            .pwsh
            .resolve(request("true"))
            .expect("default output")
            .stdout_max_bytes,
        100.0,
    );

    let mut wide_stdout = request("printf '%0500d' 0 | tr '0' x; printf '%0500d' 0 | tr '0' e >&2");
    wide_stdout.stdout_max_bytes = Some(500.0);
    let output = harness
        .pwsh
        .run(harness.pwsh.resolve(wide_stdout).expect("resolve output"))
        .await
        .expect("output run");
    assert!(!output.stdout.truncated);
    assert_eq!(output.stdout.text, "x".repeat(500));
    assert!(output.stderr.truncated);
    assert!(output.stderr.text.len() <= 100);

    let mut seam = request("cat; echo \"[$SEAM_VAR][$SEEKDEEP_SEAM_VAR]\"");
    seam.stdin = Some("piped\n".to_owned());
    seam.env = Some(BTreeMap::from([(
        "SEAM_VAR".to_owned(),
        "env-ok".to_owned(),
    )]));
    seam.seekdeep_env = Some(SeekDeepEnvironment::new(BTreeMap::from([(
        SeekDeepEnvironmentKey::new("SEEKDEEP_SEAM_VAR").unwrap(),
        "seekdeep-ok".to_owned(),
    )])));
    let spec = harness.pwsh.resolve(seam).expect("seam resolve");
    assert_eq!(spec.stdin.as_deref(), Some("piped\n"));
    assert_eq!(spec.env.as_ref().unwrap()["SEAM_VAR"], "env-ok");
    let seam_result = harness.pwsh.run(spec).await.expect("seam run");
    assert_eq!(seam_result.stdout.text, "piped\n[env-ok][seekdeep-ok]\n");

    let plain = harness.pwsh.resolve(request("true")).expect("plain");
    assert!(plain.stdin.is_none());
    assert!(plain.env.is_none());
    assert!(plain.seekdeep_env.is_none());
}

#[test]
fn configuration_validation_names_every_unserviceable_field() {
    for (config, field) in [
        (
            Config {
                timeout_ms: f64::NAN,
                ..Config::default()
            },
            "timeoutMs",
        ),
        (
            Config {
                max_timeout_ms: 0.0,
                ..Config::default()
            },
            "maxTimeoutMs",
        ),
        (
            Config {
                max_output_bytes: -1.0,
                ..Config::default()
            },
            "maxOutputBytes",
        ),
        (
            Config {
                max_spill_bytes: 0.0,
                ..Config::default()
            },
            "maxSpillBytes",
        ),
        (
            Config {
                grace_ms: 0.0,
                ..Config::default()
            },
            "graceMs",
        ),
        (
            Config {
                grace_ms: seekdeep_util::timeout::MAX_TIMER_DELAY_MS + 1.0,
                ..Config::default()
            },
            "graceMs",
        ),
    ] {
        let error = assert_serviceable_pwsh_config(&config).unwrap_err();
        assert!(error.to_string().contains(field), "{error:#}");
    }
}

#[tokio::test]
async fn request_validation_timeout_abort_self_signal_and_spawn_failure_are_distinct() {
    let harness = setup(Config {
        timeout_ms: 60_000.0,
        ..Config::default()
    })
    .await;
    for invalid in [f64::NAN, -1.0] {
        let mut invalid_timeout = request("true");
        invalid_timeout.timeout_ms = Some(invalid);
        assert!(
            harness
                .pwsh
                .resolve(invalid_timeout)
                .unwrap_err()
                .to_string()
                .contains("request.timeoutMs")
        );
        let mut invalid_output = request("true");
        invalid_output.stdout_max_bytes = Some(invalid);
        assert!(
            harness
                .pwsh
                .resolve(invalid_output)
                .unwrap_err()
                .to_string()
                .contains("request.stdoutMaxBytes")
        );
    }

    let mut timed = request("sleep 60");
    timed.timeout_ms = Some(100.0);
    let timed = harness
        .pwsh
        .run(harness.pwsh.resolve(timed).expect("timed resolve"))
        .await
        .expect("timed run");
    assert!(timed.timed_out);
    assert!(!timed.aborted);
    assert_number(timed.timeout_ms, 100.0);

    let signal = AbortSignal::default();
    let mut aborted = request("sleep 60");
    aborted.signal = Some(signal.clone());
    let spec = harness.pwsh.resolve(aborted).expect("abort resolve");
    let pwsh = harness.pwsh.clone();
    let pending = tokio::spawn(async move { pwsh.run(spec).await });
    tokio::time::sleep(Duration::from_millis(50)).await;
    signal.abort();
    let aborted = pending.await.unwrap().expect("abort result");
    assert!(aborted.aborted);
    assert!(!aborted.timed_out);

    let self_killed = harness
        .pwsh
        .run(
            harness
                .pwsh
                .resolve(request("kill -TERM $$"))
                .expect("self kill resolve"),
        )
        .await
        .expect("self kill");
    assert_eq!(
        self_killed.signal.as_ref().map(ProcessSignal::as_str),
        Some("SIGTERM")
    );
    assert!(!self_killed.timed_out);
    assert!(!self_killed.aborted);

    let mut missing = request("true");
    missing.workdir = Some("/nonexistent-seekdeep-pwsh-local".into());
    let error = harness
        .pwsh
        .run(harness.pwsh.resolve(missing).expect("missing resolve"))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("No such file") || error.to_string().contains("os error 2"),
        "{error:#}"
    );
}

#[tokio::test]
async fn background_streams_are_consuming_and_report_both_spill_paths() {
    let harness = setup(Config {
        max_output_bytes: 100.0,
        ..Config::default()
    })
    .await;

    let process = harness
        .pwsh
        .start(
            harness
                .pwsh
                .resolve(request("echo first; sleep 0.2; echo second"))
                .expect("stream resolve"),
        )
        .expect("stream start");
    assert_eq!(process.status(), ShellProcessStatus::Running);
    assert_eq!(read_until(&process, "first\n").await, "first\n");
    process.done().await;
    assert_eq!(process.status(), ShellProcessStatus::Completed);
    assert_eq!(process.exit_code(), Some(0));
    assert_eq!(process.read_output().delta, "second\n");
    assert_eq!(process.read_output().delta, "");

    for (command, expected) in [
        ("echo out; echo err >&2", "out\n[stderr]\nerr\n"),
        ("echo err >&2", "[stderr]\nerr\n"),
        ("printf out; echo err >&2", "out\n[stderr]\nerr\n"),
    ] {
        let process = harness
            .pwsh
            .start(harness.pwsh.resolve(request(command)).expect("resolve"))
            .expect("start");
        process.done().await;
        assert_eq!(process.read_output().delta, expected);
    }

    let stdout_spill = harness
        .pwsh
        .start(
            harness
                .pwsh
                .resolve(request(
                    "for i in $(seq 1 100); do printf 'line-%04d\\n' $i; done",
                ))
                .expect("spill resolve"),
        )
        .expect("spill start");
    stdout_spill.done().await;
    let read = stdout_spill.read_output();
    assert!(read.lossy);
    assert!(read.stdout_spill_path.is_some());

    let stderr_spill = harness
        .pwsh
        .start(
            harness
                .pwsh
                .resolve(request(
                    "for i in $(seq 1 100); do printf 'line-%04d\\n' $i >&2; done",
                ))
                .expect("stderr spill resolve"),
        )
        .expect("stderr spill start");
    stderr_spill.done().await;
    let read = stderr_spill.read_output();
    assert!(read.lossy);
    assert!(read.stderr_spill_path.is_some());
    assert!(read.delta.contains("[stderr]"));
}

#[tokio::test]
async fn background_kill_abort_self_signal_and_spawn_failure_settle_without_orphans() {
    let harness = setup(Config::default()).await;
    let killed = harness
        .pwsh
        .start(
            harness
                .pwsh
                .resolve(request("sleep 60"))
                .expect("kill resolve"),
        )
        .expect("kill start");
    assert!(killed.kill());
    killed.done().await;
    assert_eq!(killed.status(), ShellProcessStatus::Killed);
    assert_eq!(
        killed.signal().as_ref().map(ProcessSignal::as_str),
        Some("SIGTERM")
    );
    assert!(!killed.kill());

    let signal = AbortSignal::default();
    let mut abort_request = request("sleep 60");
    abort_request.signal = Some(signal.clone());
    let aborted = harness
        .pwsh
        .start(harness.pwsh.resolve(abort_request).expect("abort resolve"))
        .expect("abort start");
    signal.abort();
    aborted.done().await;
    assert_eq!(aborted.status(), ShellProcessStatus::Killed);

    let self_killed = harness
        .pwsh
        .start(
            harness
                .pwsh
                .resolve(request("kill -TERM $$"))
                .expect("self resolve"),
        )
        .expect("self start");
    self_killed.done().await;
    assert_eq!(self_killed.status(), ShellProcessStatus::Killed);
    assert_eq!(self_killed.exit_code(), None);

    let mut missing = request("true");
    missing.workdir = Some("/nonexistent-seekdeep-pwsh-local".into());
    let failed = harness
        .pwsh
        .start(harness.pwsh.resolve(missing).expect("missing resolve"))
        .expect("failed handle");
    failed.done().await;
    assert_eq!(failed.status(), ShellProcessStatus::Killed);
    assert!(failed.read_output().delta.contains("spawn failed:"));

    assert_eq!(harness.runtime.live_process_count(), 0);
}

#[tokio::test]
async fn subprocess_owner_outlives_executor_and_disposal_joins_live_process_trees() {
    let root = Context::new();
    let spill = tempfile::tempdir().unwrap();
    let manager_fiber = Fiber::active_child("subprocess-test");
    let manager_context = root.with_fiber(manager_fiber.clone());
    LocalSubprocessRuntime::install_runtime(
        &manager_context,
        Arc::new(LocalSubprocessRuntime::with_spill_dir(spill.path())),
    )
    .unwrap();
    let executor_fiber = Fiber::active_child("pwsh-test");
    let executor_context = root.with_fiber(executor_fiber.clone());
    let pwsh_path = pwsh_shim(spill.path());
    let pwsh = apply(
        &executor_context,
        Config {
            grace_ms: 200.0,
            pwsh_path: Some(pwsh_path.to_string_lossy().into_owned()),
            ..Config::default()
        },
    )
    .await
    .unwrap();
    let process = pwsh
        .start(pwsh.resolve(request("echo $$; sleep 60")).unwrap())
        .unwrap();
    let pid: i32 = read_until(&process, "\n")
        .await
        .trim()
        .parse()
        .expect("pid");

    executor_fiber.dispose().await.unwrap();
    assert_eq!(process.status(), ShellProcessStatus::Running);
    assert_eq!(process_probe(pid), 0);

    manager_fiber.dispose().await.unwrap();
    process.done().await;
    assert_eq!(process.status(), ShellProcessStatus::Killed);
    assert_ne!(process_probe(pid), 0);
}

#[cfg(unix)]
fn process_probe(pid: i32) -> i32 {
    // Use the safe system `kill` command rather than an unsafe libc call; signal
    // zero observes liveness without mutating the process.
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_or(-1, |status| status.code().unwrap_or(-1))
}
