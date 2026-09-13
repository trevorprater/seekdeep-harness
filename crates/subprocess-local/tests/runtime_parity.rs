#![cfg(unix)]
//! Service registration, automatic release, and normal disposal parity.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use seekdeep_cordis::Context;
use seekdeep_subprocess::{
    SUBPROCESS, SubprocessCollect, SubprocessOutputMode, SubprocessRuntime as _,
    SubprocessSpawnSpec, SubprocessStdinMode, SubprocessStdio, SubprocessTerminalSpawnSpec,
};
use seekdeep_subprocess_local::{
    LocalSubprocessRuntime, SpawnInternals, SpawnPlatform,
    process_inspector::{ProcessIdentity, ProcessInspector, ProcessStartIdentity},
};

#[derive(Debug)]
struct UnquiescentInspector;

impl ProcessInspector for UnquiescentInspector {
    fn foreground_pgid(
        &self,
        _shell_pid: seekdeep_subprocess::ProcessId,
    ) -> Option<seekdeep_subprocess::ProcessGroupId> {
        None
    }

    fn is_stdin_waiting(&self, _pgid: seekdeep_subprocess::ProcessGroupId) -> bool {
        false
    }

    fn process_tree(&self, root_pid: seekdeep_subprocess::ProcessId) -> Vec<ProcessIdentity> {
        vec![
            ProcessIdentity {
                pid: root_pid,
                started: ProcessStartIdentity::new("root"),
            },
            ProcessIdentity {
                pid: seekdeep_subprocess::ProcessId::new(987_654_321),
                started: ProcessStartIdentity::new("simulated-survivor"),
            },
        ]
    }

    fn process_session(&self, _session_id: seekdeep_subprocess::ProcessId) -> Vec<ProcessIdentity> {
        Vec::new()
    }

    fn is_alive(&self, identity: &ProcessIdentity) -> bool {
        identity.started.as_str() == "simulated-survivor"
    }

    fn signal_group(
        &self,
        _pgid: seekdeep_subprocess::ProcessGroupId,
        _signal: seekdeep_subprocess::SubprocessTerminalSignal,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    fn signal_process(&self, _identity: &ProcessIdentity, _force: bool) -> anyhow::Result<()> {
        Ok(())
    }
}

fn process_spec(script: &str) -> SubprocessSpawnSpec {
    SubprocessSpawnSpec {
        argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
        cwd: PathBuf::from("/tmp"),
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
        grace_ms: 100.0,
        signal: None,
        env: None,
    }
}

fn terminal_spec(script: &str) -> SubprocessTerminalSpawnSpec {
    SubprocessTerminalSpawnSpec {
        argv: vec!["/bin/sh".to_owned(), "-c".to_owned(), script.to_owned()],
        cwd: PathBuf::from("/tmp"),
        env: None,
        rows: 24,
        cols: 80,
        grace_ms: 100.0,
        signal: None,
    }
}

#[tokio::test]
async fn disposal_terminates_and_joins_a_live_ordinary_tree() {
    let context = Context::new();
    let runtime = LocalSubprocessRuntime::install(&context).unwrap();
    let handle = runtime.spawn(process_spec("sleep 60")).unwrap();
    assert_eq!(runtime.live_process_count(), 1);
    context.fiber().dispose().await.unwrap();
    assert!(context.get(SUBPROCESS).is_none());
    assert_eq!(runtime.live_process_count(), 0);
    assert_eq!(
        handle.done().await.unwrap().signal.unwrap().as_str(),
        "SIGTERM"
    );
}

#[tokio::test]
async fn settled_ordinary_and_terminal_handles_release_only_after_quiescence() {
    let context = Context::new();
    let runtime = LocalSubprocessRuntime::install(&context).unwrap();
    let ordinary = runtime.spawn(process_spec("true")).unwrap();
    assert_eq!(ordinary.done().await.unwrap().exit_code, Some(0));
    wait_until(|| runtime.live_process_count() == 0).await;

    let terminal = runtime
        .spawn_terminal(terminal_spec("printf done"))
        .await
        .unwrap();
    assert_eq!(terminal.done().await.unwrap().exit_code, Some(0));
    wait_until(|| runtime.live_terminal_count() == 0).await;
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn disposal_terminates_and_joins_a_live_terminal() {
    let context = Context::new();
    let runtime = LocalSubprocessRuntime::install(&context).unwrap();
    let terminal = runtime
        .spawn_terminal(terminal_spec("sleep 60"))
        .await
        .unwrap();
    assert_eq!(runtime.live_terminal_count(), 1);
    context.fiber().dispose().await.unwrap();
    assert_eq!(runtime.live_terminal_count(), 0);
    assert!(terminal.done().await.is_ok());
}

#[tokio::test]
async fn failed_automatic_terminal_cleanup_retains_runtime_ownership() {
    let context = Context::new();
    let runtime = Arc::new(LocalSubprocessRuntime::with_terminal_inspector(Arc::new(
        UnquiescentInspector,
    )));
    LocalSubprocessRuntime::install_runtime(&context, runtime.clone()).unwrap();
    let terminal = runtime.spawn_terminal(terminal_spec("true")).await.unwrap();
    terminal.done().await.unwrap();
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(runtime.live_terminal_count(), 1);
    let _ = context.fiber().dispose().await;
    assert_eq!(runtime.live_terminal_count(), 0);
}

#[tokio::test]
async fn disposal_waits_all_terminal_failures_concurrently_and_aggregates() {
    let context = Context::new();
    let runtime = Arc::new(LocalSubprocessRuntime::with_terminal_inspector(Arc::new(
        UnquiescentInspector,
    )));
    LocalSubprocessRuntime::install_runtime(&context, runtime.clone()).unwrap();
    let first = runtime.spawn_terminal(terminal_spec("true")).await.unwrap();
    let second = runtime.spawn_terminal(terminal_spec("true")).await.unwrap();
    first.done().await.unwrap();
    second.done().await.unwrap();
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(runtime.live_terminal_count(), 2);
    let started = tokio::time::Instant::now();
    let error = context.fiber().dispose().await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "local subprocess teardown: local subprocess teardown failed"
    );
    assert!(started.elapsed() < Duration::from_millis(350));
    assert_eq!(runtime.live_terminal_count(), 0);
}

#[tokio::test]
async fn host_exit_contains_each_target_failure_and_continues() {
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = calls.clone();
    let runtime = LocalSubprocessRuntime::with_spawn_internals(SpawnInternals {
        spill_dir: None,
        platform: Some(SpawnPlatform::Windows),
        taskkill: Some(Arc::new(move |pid| {
            let call = captured.fetch_add(1, Ordering::AcqRel);
            assert_ne!(call, 0, "injected taskkill failure");
            kill_pid(pid);
        })),
        linux_group_has_live_members: None,
    });
    let first = runtime.spawn(process_spec("sleep 60")).unwrap();
    let second = runtime.spawn(process_spec("sleep 60")).unwrap();
    seekdeep_process_exit_hook::ProcessExitTarget::terminate_for_process_exit(&runtime);
    assert_eq!(calls.load(Ordering::Acquire), 2);
    assert_eq!(
        second.done().await.unwrap().signal.unwrap().as_str(),
        "SIGKILL"
    );
    kill_pid(first.pid());
    first.done().await.unwrap();
}

fn kill_pid(pid: seekdeep_subprocess::ProcessId) {
    let Ok(pid) = i32::try_from(pid.as_i64()) else {
        return;
    };
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGKILL,
    );
}

#[test]
fn second_provider_fails_without_replacing_the_first() {
    let context = Context::new();
    let first = LocalSubprocessRuntime::install(&context).unwrap();
    let error = LocalSubprocessRuntime::install(&context).unwrap_err();
    assert_eq!(
        error.to_string(),
        "service \"subprocess\" has been registered"
    );
    assert_eq!(first.live_process_count(), 0);
}

async fn wait_until(predicate: impl Fn() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(tokio::time::Instant::now() < deadline, "condition timeout");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
