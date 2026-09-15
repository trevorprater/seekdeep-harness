use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

use super::{BaselineRunner, ReleaseBundle, bundle::write_json, npm_client_environment, strings};
use crate::release_process::ReleaseRunOptions;

/// Isolated installed-entry web startup and clean-shutdown request.
#[derive(Clone, Debug)]
pub struct InstalledWebProbe {
    /// Node executable used by the installed npm entry binding.
    pub node: PathBuf,
    /// Verified entry path beneath the consumer directory.
    pub bin: PathBuf,
    /// Consumer root, used as the process working directory.
    pub cwd: PathBuf,
    /// Complete child environment with workspace injection removed.
    pub environment: BTreeMap<OsString, OsString>,
    /// Maximum interval covering readiness and shutdown.
    pub timeout: Duration,
}

/// Builds an isolated installed-artifact environment without inherited Node hooks.
#[must_use]
pub fn installed_artifact_environment(
    parent: &BTreeMap<OsString, OsString>,
    consumer: &Path,
) -> BTreeMap<OsString, OsString> {
    let mut environment = npm_client_environment(parent);
    for key in ["NODE_OPTIONS", "NODE_PATH", "COLORTERM"] {
        environment.remove(std::ffi::OsStr::new(key));
    }
    for (key, value) in [
        ("SEEKDEEP_HOME", consumer.join(".seekdeep").into_os_string()),
        (
            "SEEKDEEP_AGENTS_HOME",
            consumer.join(".agents").into_os_string(),
        ),
        ("SEEKDEEP_TELEMETRY_DISABLED", "1".into()),
        ("DEEPSEEK_API_KEY", "keyless-installed-web-no-call".into()),
        ("LANG", "en_US.UTF-8".into()),
        ("LC_ALL", "en_US.UTF-8".into()),
        ("LC_CTYPE", "en_US.UTF-8".into()),
        ("TERM", "xterm-256color".into()),
        ("COLUMNS", "100".into()),
        ("LINES", "30".into()),
    ] {
        environment.insert(key.into(), value);
    }
    environment
}

/// Installs every local tarball in a temporary consumer and checks the shipped entry.
///
/// # Errors
/// Returns installation, escaped entry, version, terminal-readiness, or teardown errors.
pub fn smoke_installed_bundle(
    bundle: &ReleaseBundle,
    parent_environment: &BTreeMap<OsString, OsString>,
    runner: &mut impl BaselineRunner,
) -> anyhow::Result<()> {
    let consumer = tempfile::Builder::new()
        .prefix("seekdeep-npm-consumer-")
        .tempdir()?;
    let root = consumer.path();
    let mut dependencies = serde_json::Map::new();
    for package in &bundle.manifest.packages {
        let tarball = bundle.tarball_path(package);
        let url = url::Url::from_file_path(&tarball)
            .map_err(|()| anyhow::anyhow!("invalid local tarball path: {}", tarball.display()))?;
        dependencies.insert(
            package.name.to_string(),
            serde_json::Value::String(url.into()),
        );
    }
    write_json(
        &root.join("package.json"),
        &serde_json::json!({
            "name": "seekdeep-npm-baseline-consumer", "version": "0.0.0", "private": true, "dependencies": dependencies,
        }),
    )?;
    runner.log(&format!(
        "publish-npm-baseline: installing {} local tarballs",
        bundle.manifest.packages.len()
    ));
    runner.run(
        "npm",
        &strings(&[
            "install",
            "--no-audit",
            "--no-fund",
            "--package-lock=false",
            &format!("--registry={}", bundle.manifest.registry),
        ]),
        &ReleaseRunOptions {
            cwd: Some(root.to_owned()),
            env: Some(npm_client_environment(parent_environment)),
        },
    )?;
    let bin = root.join("node_modules/@seekdeep-ai/seekdeep/lib/bin.js");
    let canonical_root = root.canonicalize()?;
    let canonical_bin = bin.canonicalize()?;
    if !canonical_bin.starts_with(&canonical_root) {
        anyhow::bail!(
            "installed seekdeep bin resolved outside the isolated consumer: {}",
            canonical_bin.display()
        );
    }
    let environment = installed_artifact_environment(parent_environment, root);
    let version = runner.capture(
        "node",
        &strings(&[&bin.to_string_lossy(), "--version"]),
        &ReleaseRunOptions {
            cwd: Some(root.to_owned()),
            env: Some(environment.clone()),
        },
    )?;
    if version != bundle.manifest.version.as_str() {
        anyhow::bail!(
            "installed seekdeep --version returned {}; expected {}",
            serde_json::Value::String(version),
            bundle.manifest.version
        );
    }
    runner.web_probe(&InstalledWebProbe {
        node: PathBuf::from("node"),
        bin,
        cwd: root.to_owned(),
        environment,
        timeout: Duration::from_secs(60),
    })?;
    runner.log("publish-npm-baseline: installed seekdeep entry and Web startup probes passed");
    Ok(())
}

/// Runs the source POSIX PTY readiness/SIGTERM contract using native Rust APIs.
///
/// # Errors
/// Returns unsupported-host, PTY, process, timeout, missing readiness, or unclean exit failures.
pub fn probe_installed_web(probe: &InstalledWebProbe) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        native_probe(probe)
    }
    #[cfg(not(unix))]
    {
        let _ = probe;
        anyhow::bail!("installed seekdeep Web probe requires a POSIX host");
    }
}

#[cfg(unix)]
fn native_probe(probe: &InstalledWebProbe) -> anyhow::Result<()> {
    use nix::{
        sys::{
            signal::{Signal, kill},
            wait::{WaitPidFlag, WaitStatus, waitpid},
        },
        unistd::Pid,
    };
    use std::{io::Read as _, time::Instant};

    let (mut owner, master) = spawn_probe(probe)?;
    let pid =
        Pid::from_raw(i32::try_from(owner.child.process_id().ok_or_else(
            || anyhow::anyhow!("installed seekdeep Web process has no pid"),
        )?)?);
    let descriptor = master
        .as_raw_fd()
        .ok_or_else(|| anyhow::anyhow!("installed seekdeep Web terminal has no descriptor"))?;
    let mut reader = master.try_clone_reader()?;
    let mut poll = [filedescriptor::pollfd {
        fd: descriptor,
        events: filedescriptor::POLLIN,
        revents: 0,
    }];
    let mut output = Vec::new();
    let mut ready_seen = false;
    let deadline = Instant::now() + probe.timeout;
    let mut status = None;
    let mut buffer = vec![0_u8; 65_536];
    while Instant::now() < deadline {
        if filedescriptor::poll(&mut poll, Some(Duration::from_millis(50)))? > 0 {
            match reader.read(&mut buffer) {
                Ok(length) => output.extend_from_slice(&buffer[..length]),
                Err(error) if error.raw_os_error() == Some(nix::errno::Errno::EIO as i32) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let marker = b"seekdeep web: http://127.0.0.1:";
        if !ready_seen && output.windows(marker.len()).any(|window| window == marker) {
            ready_seen = true;
            kill(pid, Signal::SIGTERM)?;
        }
        match waitpid(pid, Some(WaitPidFlag::WNOHANG))? {
            WaitStatus::Exited(_, code) => {
                status = Some(code);
                break;
            }
            WaitStatus::Signaled(_, signal, _) => {
                status = Some(-(signal as i32));
                break;
            }
            WaitStatus::StillAlive | WaitStatus::Stopped(_, _) | WaitStatus::Continued(_) => {}
            #[cfg(any(target_os = "linux", target_os = "android"))]
            WaitStatus::PtraceEvent(_, _, _) | WaitStatus::PtraceSyscall(_) => {}
        }
    }
    let status = if let Some(status) = status {
        owner.reaped = true;
        status
    } else {
        kill(pid, Signal::SIGKILL)?;
        let status = waitpid(pid, None)?;
        owner.reaped = true;
        match status {
            WaitStatus::Exited(_, code) => code,
            WaitStatus::Signaled(_, signal, _) => -(signal as i32),
            _ => anyhow::bail!("installed seekdeep Web process did not terminate"),
        }
    };
    if !ready_seen {
        return Err(probe_failure(
            &output,
            "installed seekdeep web did not reach its ready URL",
        ));
    }
    if status != 0 {
        return Err(probe_failure(
            &output,
            &format!("installed seekdeep web exited {status}, expected 0"),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn probe_failure(output: &[u8], message: &str) -> anyhow::Error {
    let output = String::from_utf8_lossy(output);
    if output.trim().is_empty() {
        anyhow::anyhow!("{message}")
    } else {
        anyhow::anyhow!("{}\n{message}", output.trim())
    }
}

#[cfg(unix)]
fn spawn_probe(
    probe: &InstalledWebProbe,
) -> anyhow::Result<(PtyChildOwner, Box<dyn portable_pty::MasterPty + Send>)> {
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};

    let pair = native_pty_system().openpty(PtySize {
        rows: 0,
        cols: 0,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let mut command = CommandBuilder::new(&probe.node);
    command.args([
        probe.bin.as_os_str(),
        std::ffi::OsStr::new("web"),
        std::ffi::OsStr::new("--host"),
        std::ffi::OsStr::new("127.0.0.1"),
        std::ffi::OsStr::new("--port"),
        std::ffi::OsStr::new("0"),
    ]);
    command.cwd(&probe.cwd);
    command.env_clear();
    for (key, value) in &probe.environment {
        command.env(key, value);
    }
    let child = pair.slave.spawn_command(command)?;
    drop(pair.slave);
    Ok((
        PtyChildOwner {
            child,
            reaped: false,
        },
        pair.master,
    ))
}

#[cfg(unix)]
struct PtyChildOwner {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    reaped: bool,
}

#[cfg(unix)]
impl Drop for PtyChildOwner {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
