//! Compiled-process acceptance for long-lived generic profile shutdown.

#![cfg(unix)]

use std::{
    io::Read as _,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use seekdeep_app_boot::{init_profile, resolve_profile_dir};

#[test]
fn empty_custom_profile_boots_and_sigterm_disposes_with_zero() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let home = temporary.path().join("home");
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let profile = resolve_profile_dir("minimal", &home)?;
    init_profile(&profile, &[])?;
    let root_config = profile.join("cordis.yml");
    let mut child = Command::new(env!("CARGO_BIN_EXE_seekdeep"))
        .args(["--profile", "minimal"])
        .env_clear()
        .env("SEEKDEEP_HOME", &home)
        .current_dir(&workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let pid = Pid::from_raw(i32::try_from(child.id())?);
    let startup_deadline = Instant::now() + Duration::from_secs(15);
    while !root_config.exists() {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("profile exited before readiness with {status}");
        }
        anyhow::ensure!(
            Instant::now() < startup_deadline,
            "profile startup timed out"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(250));
    anyhow::ensure!(child.try_wait()?.is_none(), "profile exited before SIGTERM");
    kill(pid, Signal::SIGTERM)?;

    let shutdown_deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= shutdown_deadline {
            let _ = kill(pid, Signal::SIGKILL);
            let _ = child.wait();
            anyhow::bail!("profile SIGTERM shutdown timed out");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut stdout = String::new();
    child.stdout.take().unwrap().read_to_string(&mut stdout)?;
    let mut stderr = String::new();
    child.stderr.take().unwrap().read_to_string(&mut stderr)?;
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    assert!(stdout.is_empty(), "unexpected stdout: {stdout:?}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr:?}");
    Ok(())
}
