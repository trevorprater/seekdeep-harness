//! Verify cancellation through an installed SDK while its runtime initialization is pending.

#[cfg(unix)]
use std::{
    os::unix::{fs::PermissionsExt as _, process::CommandExt as _},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[cfg(unix)]
struct ChildGuard(Option<std::process::Child>);

#[cfg(unix)]
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(unix)]
fn main() -> anyhow::Result<()> {
    let python = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("usage: python_runtime_sdk_interrupt <installed-python>"))?;
    let binary = std::env::current_exe()?
        .parent()
        .and_then(|path| path.parent())
        .ok_or_else(|| anyhow::anyhow!("example output directory absent"))?
        .join("smoke-python-runtime");
    anyhow::ensure!(
        binary.is_file(),
        "build smoke-python-runtime before this installed-SDK probe"
    );
    for group in [false, true] {
        check_interrupt(&binary, &python, group)?;
    }
    println!(
        "installed SDK cancellation sends shutdown and reaps its initializing runtime for owner-only and foreground-group interrupts"
    );
    Ok(())
}

/// Kills the CLI and folds its captured output into the failure, so a deadline miss on a
/// CI runner explains what the SDK and runtime printed instead of only naming the phase.
#[cfg(unix)]
fn failure(child: &mut ChildGuard, phase: &str) -> anyhow::Error {
    let Some(mut cli) = child.0.take() else {
        return anyhow::anyhow!("{phase}");
    };
    let _ = cli.kill();
    match cli.wait_with_output() {
        Ok(output) => anyhow::anyhow!(
            "{phase}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(error) => anyhow::anyhow!("{phase} (and its output could not be read: {error})"),
    }
}

/// Writes the fake runtime the CLI drives: it ignores SIGINT itself, records its pid once
/// initialized, and acknowledges the SDK's shutdown request so the teardown can be observed.
#[cfg(unix)]
fn write_fake_runtime(directory: &Path) -> anyhow::Result<std::path::PathBuf> {
    let executable = directory.join("fake-runtime");
    std::fs::write(
        &executable,
        r"#!/usr/bin/env python3
import json, os, pathlib, signal, sys
signal.signal(signal.SIGINT, signal.SIG_IGN)
for line in sys.stdin:
    message = json.loads(line)
    if message['method'] == 'initialize':
        pathlib.Path(os.environ['SEEKDEEP_SMOKE_TEST_READY']).write_text(str(os.getpid()))
    elif message['method'] == 'shutdown':
        pathlib.Path(os.environ['SEEKDEEP_SMOKE_TEST_CLOSED']).write_text('closed')
        print(json.dumps({'jsonrpc':'2.0','id':message['id'],'result':{}}),flush=True)
        break
",
    )?;
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
    Ok(executable)
}

#[cfg(unix)]
fn check_interrupt(binary: &Path, python: &Path, group: bool) -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let executable = write_fake_runtime(temporary.path())?;
    let ready = temporary.path().join("ready");
    let closed = temporary.path().join("closed");
    let mut command = Command::new(binary);
    if group {
        command.process_group(0);
    }
    let child = command
        .args(["--scenario", "sdk-custom", "--exe"])
        .arg(&executable)
        .arg("--root")
        .arg(temporary.path())
        .arg("--python")
        .arg(python)
        .env("SEEKDEEP_SMOKE_TEST_READY", &ready)
        .env("SEEKDEEP_SMOKE_TEST_CLOSED", &closed)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut child = ChildGuard(Some(child));
    // A cold runner spends seconds starting Python and importing the SDK before the
    // fake runtime reports readiness; that budget is separate from the cleanup budget.
    let started = Instant::now();
    let phase = if group {
        "foreground group"
    } else {
        "owner only"
    };
    let deadline = started + Duration::from_secs(30);
    while !ready.is_file() {
        if Instant::now() >= deadline {
            return Err(failure(
                &mut child,
                &format!(
                    "{phase}: runtime did not reach initialization within {:?}",
                    started.elapsed()
                ),
            ));
        }
        anyhow::ensure!(
            child.0.as_mut().expect("owned CLI").try_wait()?.is_none(),
            "CLI exited before initialization"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let runtime_pid = std::fs::read_to_string(&ready)?;
    let target = if group {
        format!("-{}", child.0.as_ref().expect("owned CLI").id())
    } else {
        child.0.as_ref().expect("owned CLI").id().to_string()
    };
    anyhow::ensure!(
        Command::new("kill")
            .args(["-INT", "--", &target])
            .status()?
            .success(),
        "could not interrupt owned CLI"
    );
    // The SDK's own runtime teardown allows ten seconds before it kills the runtime, and a
    // loaded runner adds interpreter shutdown and scheduling latency on top; the cleanup
    // budget starts at the interrupt and leaves generous room for both. A miss reports how
    // far the teardown got so the next failure carries a signal.
    let interrupted = Instant::now();
    let deadline = interrupted + Duration::from_secs(60);
    while child.0.as_mut().expect("owned CLI").try_wait()?.is_none() {
        if Instant::now() >= deadline {
            let miss = format!(
                "{phase}: interrupted CLI did not finish SDK cleanup within {:?} (ready after {:?}, shutdown reached the runtime: {}, runtime still alive: {})",
                interrupted.elapsed(),
                interrupted.duration_since(started),
                closed.is_file(),
                process_alive(runtime_pid.trim())?
            );
            return Err(failure(&mut child, &miss));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let output = child.0.take().expect("owned CLI").wait_with_output()?;
    anyhow::ensure!(
        output.status.code() == Some(130),
        "{phase}: interrupt status {:?} after {:?} (ready after {:?}, shutdown reached the runtime: {}); stderr:\n{}",
        output.status,
        interrupted.elapsed(),
        interrupted.duration_since(started),
        closed.is_file(),
        String::from_utf8_lossy(&output.stderr)
    );
    anyhow::ensure!(closed.is_file(), "SDK did not send runtime shutdown");
    anyhow::ensure!(
        !process_alive(runtime_pid.trim())?,
        "runtime remains alive after CLI exit"
    );
    Ok(())
}

/// Whether `pid` still answers a null signal.
#[cfg(unix)]
fn process_alive(pid: &str) -> anyhow::Result<bool> {
    Ok(Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()?
        .success())
}

#[cfg(not(unix))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("the packaged SDK runtime platform matrix is Unix-only")
}
