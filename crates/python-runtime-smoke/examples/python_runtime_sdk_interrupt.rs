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

#[cfg(unix)]
fn check_interrupt(binary: &Path, python: &Path, group: bool) -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let executable = temporary.path().join("fake-runtime");
    let ready = temporary.path().join("ready");
    let closed = temporary.path().join("closed");
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
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.is_file() {
        anyhow::ensure!(
            Instant::now() < deadline,
            "runtime did not reach initialization"
        );
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
            .args(["-INT", &target])
            .status()?
            .success(),
        "could not interrupt owned CLI"
    );
    while child.0.as_mut().expect("owned CLI").try_wait()?.is_none() {
        anyhow::ensure!(
            Instant::now() < deadline,
            "interrupted CLI did not finish SDK cleanup"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let output = child.0.take().expect("owned CLI").wait_with_output()?;
    anyhow::ensure!(
        output.status.code() == Some(130),
        "interrupt status {:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    anyhow::ensure!(closed.is_file(), "SDK did not send runtime shutdown");
    anyhow::ensure!(
        !Command::new("kill")
            .args(["-0", runtime_pid.trim()])
            .stderr(Stdio::null())
            .status()?
            .success(),
        "runtime remains alive after CLI exit"
    );
    Ok(())
}

#[cfg(not(unix))]
fn main() -> anyhow::Result<()> {
    anyhow::bail!("the packaged SDK runtime platform matrix is Unix-only")
}
