//! Bounded Oxlint process orchestration for compatibility sources.

use std::{
    io::Write as _,
    path::Path,
    process::{Command, ExitStatus, Stdio},
};

/// Product-renamed worker-pool environment variable.
pub const OXLINT_THREADS_ENV: &str = "SEEKDEEP_OXLINT_THREADS";

const MAX_CAPTURED_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_SAFE_JAVASCRIPT_INTEGER: u64 = 9_007_199_254_740_991;
const FIX_FLAGS: &[&str] = &["--fix", "--fix-dangerously", "--fix-suggestions"];

/// Complete argument and worker-bound resolution for one Oxlint invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OxlintInvocation {
    /// Oxlint CLI arguments, including any environment-owned thread flag.
    pub args: Vec<String>,
    /// `GOMAXPROCS` override applied to the child, absent at default settings.
    pub go_max_procs: Option<String>,
}

/// Child completion plane preserved for the command-line wrapper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OxlintCompletion {
    /// Ordinary process exit code.
    Exit(i32),
    /// Unix signal number that terminated the child.
    Signaled(i32),
}

/// Applies the repository worker bound to both Oxlint backends.
///
/// # Errors
///
/// Returns source-compatible diagnostics for noncanonical, nonpositive,
/// JavaScript-unsafe, or competing thread bounds.
pub fn resolve_oxlint_invocation(
    args: &[String],
    thread_bound: Option<&str>,
) -> anyhow::Result<OxlintInvocation> {
    let Some(raw) = thread_bound.filter(|raw| !raw.is_empty()) else {
        return Ok(OxlintInvocation {
            args: args.to_vec(),
            go_max_procs: None,
        });
    };
    let parsed = raw.parse::<u64>().ok();
    if parsed.is_none_or(|parsed| {
        parsed == 0 || parsed > MAX_SAFE_JAVASCRIPT_INTEGER || parsed.to_string() != raw
    }) {
        anyhow::bail!(
            "run-oxlint: {OXLINT_THREADS_ENV} must be a positive integer, got {}.",
            json_string(raw)
        );
    }
    if args
        .iter()
        .any(|argument| argument == "--threads" || argument.starts_with("--threads="))
    {
        anyhow::bail!(
            "run-oxlint: use {OXLINT_THREADS_ENV} instead of passing --threads directly."
        );
    }
    let mut complete = args.to_vec();
    complete.push(format!("--threads={raw}"));
    Ok(OxlintInvocation {
        args: complete,
        go_max_procs: Some(raw.to_owned()),
    })
}

/// Runs Oxlint once for checks and at most twice for fix invocations.
///
/// # Errors
///
/// Returns invocation validation, process-spawn, output-cap, or output-forward
/// failures.
pub fn run_oxlint(
    root: &Path,
    args: &[String],
    thread_bound: Option<&str>,
) -> anyhow::Result<OxlintCompletion> {
    let invocation = resolve_oxlint_invocation(args, thread_bound)?;
    let executable = root.join("node_modules/oxlint/bin/oxlint");
    if !is_fix_invocation(&invocation.args) {
        return Ok(completion_from(run_inherited(&executable, &invocation)?));
    }

    let first = run_captured(&executable, &invocation)?;
    if let Some(signal) = signal_of(first.status) {
        return Ok(OxlintCompletion::Signaled(signal));
    }
    if first.status.success() {
        std::io::stdout().write_all(&first.stdout)?;
        std::io::stderr().write_all(&first.stderr)?;
        return Ok(OxlintCompletion::Exit(0));
    }
    Ok(completion_from(run_inherited(&executable, &invocation)?))
}

/// Re-raises a child signal or converts an ordinary status to a process code.
///
/// # Errors
///
/// Returns signal conversion or delivery failures.
pub fn complete_oxlint_process(completion: OxlintCompletion) -> anyhow::Result<u8> {
    match completion {
        OxlintCompletion::Exit(code) => Ok(u8::try_from(code).unwrap_or(1)),
        OxlintCompletion::Signaled(signal) => {
            #[cfg(unix)]
            {
                let signal = nix::sys::signal::Signal::try_from(signal)?;
                nix::sys::signal::raise(signal)?;
                Ok(0)
            }
            #[cfg(not(unix))]
            {
                let _ = signal;
                Ok(1)
            }
        }
    }
}

fn run_inherited(executable: &Path, invocation: &OxlintInvocation) -> anyhow::Result<ExitStatus> {
    let mut command = oxlint_command(executable, invocation);
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    Ok(command.status()?)
}

fn run_captured(
    executable: &Path,
    invocation: &OxlintInvocation,
) -> anyhow::Result<std::process::Output> {
    let output = oxlint_command(executable, invocation).output()?;
    if output.stdout.len() > MAX_CAPTURED_OUTPUT_BYTES
        || output.stderr.len() > MAX_CAPTURED_OUTPUT_BYTES
    {
        anyhow::bail!(
            "run-oxlint: captured Oxlint output exceeded {MAX_CAPTURED_OUTPUT_BYTES} bytes"
        );
    }
    Ok(output)
}

fn oxlint_command(executable: &Path, invocation: &OxlintInvocation) -> Command {
    let mut command = Command::new(node_executable());
    command.arg(executable).args(&invocation.args);
    if let Some(bound) = &invocation.go_max_procs {
        command.env("GOMAXPROCS", bound);
    }
    command
}

fn node_executable() -> std::ffi::OsString {
    std::env::var_os("npm_node_execpath").unwrap_or_else(|| "node".into())
}

fn is_fix_invocation(args: &[String]) -> bool {
    args.iter()
        .any(|argument| FIX_FLAGS.contains(&argument.as_str()))
}

fn completion_from(status: ExitStatus) -> OxlintCompletion {
    if let Some(signal) = signal_of(status) {
        return OxlintCompletion::Signaled(signal);
    }
    OxlintCompletion::Exit(status.code().unwrap_or(1))
}

#[cfg(unix)]
fn signal_of(status: ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(&status)
}

#[cfg(not(unix))]
fn signal_of(_status: ExitStatus) -> Option<i32> {
    None
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_owned())
}
