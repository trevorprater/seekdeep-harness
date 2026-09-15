//! Process execution seams for build and release commands.

mod capture;

use std::{
    collections::BTreeMap,
    fmt, io,
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::Result;

/// One external command, without shell interpolation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    /// Executable name or path.
    pub program: String,
    /// Ordered command arguments.
    pub args: Vec<String>,
    /// Child working directory.
    pub cwd: PathBuf,
    /// Additional child environment variables.
    pub env: BTreeMap<String, String>,
    /// Whether to return output instead of inheriting the terminal.
    pub capture: bool,
    /// Maximum combined stdout/stderr bytes when capturing, before terminating the child.
    pub max_buffer: usize,
    /// Whether the child receives the caller's standard input.
    pub inherit_stdin: bool,
}

/// External command outcome, including unsuccessful exit statuses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandOutput {
    /// Exit code, or `None` when a signal terminated the child or spawning failed.
    pub status: Option<i32>,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
    /// Spawn or capture failure exposed separately from the child's termination status.
    pub spawn_error: Option<String>,
    /// Whether process creation failed before stdout and stderr streams existed.
    pub unstarted: bool,
}

/// Process, retry-delay, and output boundaries used by orchestration tests.
pub trait Runner {
    /// Run a command to completion.
    ///
    /// # Errors
    ///
    /// Returns I/O and wait failures. Process creation failures appear in `spawn_error`.
    fn run(&mut self, command: &CommandSpec) -> Result<CommandOutput>;

    /// Wait between attempts.
    fn sleep(&mut self, delay: Duration);

    /// Write one progress line.
    fn log(&mut self, message: &str);
}

/// Operating-system implementation of [`Runner`].
#[derive(Debug, Default)]
pub struct NativeRunner;

impl Runner for NativeRunner {
    fn run(&mut self, command: &CommandSpec) -> Result<CommandOutput> {
        let mut child = Command::new(&command.program);
        child
            .args(&command.args)
            .current_dir(&command.cwd)
            .envs(&command.env)
            .stdin(if command.inherit_stdin {
                Stdio::inherit()
            } else {
                Stdio::null()
            });
        if command.capture {
            capture::run(&mut child, &command.program, command.max_buffer)
        } else {
            let result = child
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status();
            let status = match result {
                Ok(status) => status,
                Err(error) => return Ok(unstarted(&command.program, &error)),
            };
            Ok(CommandOutput {
                status: status.code(),
                ..CommandOutput::default()
            })
        }
    }

    fn sleep(&mut self, delay: Duration) {
        std::thread::sleep(delay);
    }

    fn log(&mut self, message: &str) {
        println!("{message}");
    }
}

fn unstarted(program: &str, error: &io::Error) -> CommandOutput {
    #[cfg(unix)]
    let code = error
        .raw_os_error()
        .map(nix::errno::Errno::from_raw)
        .filter(|error| *error != nix::errno::Errno::UnknownErrno)
        .map(|error| format!("{error:?}"));
    #[cfg(not(unix))]
    let code = match error.kind() {
        io::ErrorKind::NotFound => Some("ENOENT".to_owned()),
        io::ErrorKind::PermissionDenied => Some("EACCES".to_owned()),
        io::ErrorKind::InvalidInput => Some("EINVAL".to_owned()),
        _ => None,
    };
    CommandOutput {
        spawn_error: Some(format!(
            "spawnSync {program} {}",
            code.unwrap_or_else(|| error.to_string())
        )),
        unstarted: true,
        ..CommandOutput::default()
    }
}

/// Failure that preserves a child command's exit code at the CLI boundary.
#[derive(Debug)]
pub struct ProcessFailure {
    /// Child exit code, or `None` for a signal.
    pub status: Option<i32>,
    /// Diagnostic to print for the failed operation.
    pub message: String,
}

impl fmt::Display for ProcessFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProcessFailure {}

/// Determine the command-line exit code carried by an error chain.
pub fn exit_code(error: &anyhow::Error) -> i32 {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ProcessFailure>())
        .and_then(|failure| failure.status)
        .filter(|status| *status != 0)
        .unwrap_or(1)
}

/// Run a command and preserve a failed child's status in the returned error.
///
/// # Errors
///
/// Returns execution failures and unsuccessful child exit statuses.
pub fn run_checked(runner: &mut dyn Runner, command: &CommandSpec) -> Result<CommandOutput> {
    let output = runner.run(command)?;
    if let Some(error) = &output.spawn_error {
        return Err(ProcessFailure {
            status: output.status,
            message: error.clone(),
        }
        .into());
    }
    if output.status != Some(0) {
        return Err(ProcessFailure {
            status: output.status,
            message: format!("{} {} failed", command.program, command.args.join(" ")),
        }
        .into());
    }
    Ok(output)
}
