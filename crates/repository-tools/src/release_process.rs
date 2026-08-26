//! Captured and inherited child-process helpers for release tooling.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::PathBuf,
    process::{Command, Stdio},
};

/// Working directory and complete child environment for a release command.
#[derive(Clone, Debug, Default)]
pub struct ReleaseRunOptions {
    /// Working directory; inherited when absent.
    pub cwd: Option<PathBuf>,
    /// Complete child environment; inherited when absent.
    pub env: Option<BTreeMap<OsString, OsString>>,
}

/// Captured output from a command whose status the caller will judge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseCommandResult {
    /// Exit status, absent when a signal ended the process.
    pub status: Option<i32>,
    /// Lossily decoded standard output.
    pub stdout: String,
    /// Lossily decoded standard error.
    pub stderr: String,
}

/// Runs a command and captures both streams without judging its exit status.
///
/// # Errors
///
/// Returns process-spawn or wait failures.
pub fn attempt(
    command: &str,
    args: &[String],
    options: &ReleaseRunOptions,
) -> anyhow::Result<ReleaseCommandResult> {
    let mut child = release_command(command, args, options);
    let output = child.output()?;
    Ok(ReleaseCommandResult {
        status: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Runs a command, requires status zero, and returns trimmed standard output.
///
/// # Errors
///
/// Returns process failures or the source-compatible nonzero diagnostic.
pub fn capture(
    command: &str,
    args: &[String],
    options: &ReleaseRunOptions,
) -> anyhow::Result<String> {
    let result = attempt(command, args, options)?;
    if result.status != Some(0) {
        anyhow::bail!(
            "{} {} exited with {}:\n{}\n{}",
            command,
            args.join(" "),
            status_text(result.status),
            result.stdout,
            result.stderr
        );
    }
    Ok(result.stdout.trim().to_owned())
}

/// Runs a command with inherited streams and requires status zero.
///
/// # Errors
///
/// Returns process failures or the source-compatible nonzero diagnostic.
pub fn run(command: &str, args: &[String], options: &ReleaseRunOptions) -> anyhow::Result<()> {
    let mut child = release_command(command, args, options);
    let status = child
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if status.code() != Some(0) {
        anyhow::bail!(
            "{} {} exited with {}",
            command,
            args.join(" "),
            status_text(status.code())
        );
    }
    Ok(())
}

fn release_command(command: &str, args: &[String], options: &ReleaseRunOptions) -> Command {
    let mut child = Command::new(command);
    child.args(args);
    if let Some(cwd) = &options.cwd {
        child.current_dir(cwd);
    }
    if let Some(environment) = &options.env {
        child.env_clear().envs(environment);
    }
    child
}

fn status_text(status: Option<i32>) -> String {
    status.map_or_else(|| "null".to_owned(), |status| status.to_string())
}
