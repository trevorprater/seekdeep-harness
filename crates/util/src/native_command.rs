//! No-shell host-native command execution with captured UTF-8 output.

use std::{
    ffi::{OsStr, OsString},
    fmt, io,
    process::{ExitStatus, Stdio},
};

use thiserror::Error;
use tokio::{io::AsyncReadExt, process::Command, task::JoinError};

use crate::abort::AbortSignal;

/// Captured output from a successful native command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeCommandOutput {
    /// Standard output decoded as non-fatal UTF-8.
    pub stdout: String,
    /// Standard error decoded as non-fatal UTF-8.
    pub stderr: String,
}

/// Source-compatible command failure code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeCommandCode {
    /// Numeric process exit status.
    Exit(i32),
    /// Named operating-system or cancellation code.
    Named(&'static str),
}

impl fmt::Display for NativeCommandCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exit(code) => code.fmt(formatter),
            Self::Named(code) => formatter.write_str(code),
        }
    }
}

/// Failed spawn, capture, cancellation, or non-zero command exit.
#[derive(Debug, Error)]
#[error("native command failed ({code}): {message}")]
pub struct NativeCommandError {
    /// Source-compatible numeric or named code.
    pub code: NativeCommandCode,
    /// Captured standard output before failure.
    pub stdout: String,
    /// Captured standard error before failure.
    pub stderr: String,
    message: String,
    #[source]
    cause: io::Error,
}

impl NativeCommandError {
    /// Underlying process or I/O error, matching the source error's `cause`.
    #[must_use]
    pub fn cause(&self) -> &io::Error {
        &self.cause
    }
}

/// Runs an executable directly, never through a shell.
///
/// Output is captured and decoded using replacement characters for malformed
/// UTF-8. Cancellation terminates and reaps the child before returning.
///
/// # Errors
///
/// Returns a structured failure for spawn errors, cancellation, capture
/// failures, signals, and non-zero exits.
pub async fn run_native_command<C, A>(
    command: C,
    args: &[A],
    signal: &AbortSignal,
) -> Result<NativeCommandOutput, NativeCommandError>
where
    C: AsRef<OsStr>,
    A: AsRef<OsStr>,
{
    let command_name = command.as_ref().to_os_string();
    let mut process = Command::new(&command_name);
    process
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    hide_windows_console(&mut process);

    let mut child = process.spawn().map_err(|cause| NativeCommandError {
        code: os_error_code(&cause),
        stdout: String::new(),
        stderr: String::new(),
        message: cause.to_string(),
        cause,
    })?;
    let stdout = child.stdout.take().ok_or_else(|| pipe_error("stdout"))?;
    let stderr = child.stderr.take().ok_or_else(|| pipe_error("stderr"))?;
    let stdout_task = tokio::spawn(read_all(stdout));
    let stderr_task = tokio::spawn(read_all(stderr));

    let outcome = tokio::select! {
        biased;
        () = signal.cancelled() => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            CommandOutcome::Aborted
        }
        status = child.wait() => CommandOutcome::Exited(status),
    };
    let stdout = join_capture(stdout_task).await;
    let stderr = join_capture(stderr_task).await;
    let stdout_text = decode_capture(&stdout);
    let stderr_text = decode_capture(&stderr);

    if let Err(cause) = stdout {
        return Err(capture_error(cause, stdout_text, stderr_text));
    }
    if let Err(cause) = stderr {
        return Err(capture_error(cause, stdout_text, stderr_text));
    }

    match outcome {
        CommandOutcome::Aborted => Err(NativeCommandError {
            code: NativeCommandCode::Named("ABORT_ERR"),
            stdout: stdout_text,
            stderr: stderr_text,
            message: "The operation was aborted".to_owned(),
            cause: io::Error::new(io::ErrorKind::Interrupted, "The operation was aborted"),
        }),
        CommandOutcome::Exited(Err(cause)) => Err(NativeCommandError {
            code: os_error_code(&cause),
            stdout: stdout_text,
            stderr: stderr_text,
            message: cause.to_string(),
            cause,
        }),
        CommandOutcome::Exited(Ok(status)) if status.success() => Ok(NativeCommandOutput {
            stdout: stdout_text,
            stderr: stderr_text,
        }),
        CommandOutcome::Exited(Ok(status)) => {
            Err(exit_error(status, &command_name, stdout_text, stderr_text))
        }
    }
}

enum CommandOutcome {
    Aborted,
    Exited(io::Result<ExitStatus>),
}

async fn read_all(mut reader: impl tokio::io::AsyncRead + Unpin) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).await?;
    Ok(bytes)
}

async fn join_capture(
    task: tokio::task::JoinHandle<io::Result<Vec<u8>>>,
) -> Result<Vec<u8>, io::Error> {
    task.await.map_err(|error| join_error(&error))?
}

fn join_error(error: &JoinError) -> io::Error {
    io::Error::other(format!("command output task failed: {error}"))
}

fn pipe_error(stream: &str) -> NativeCommandError {
    let message = format!("spawned command did not expose piped {stream}");
    NativeCommandError {
        code: NativeCommandCode::Named("UNKNOWN"),
        stdout: String::new(),
        stderr: String::new(),
        cause: io::Error::other(message.clone()),
        message,
    }
}

fn decode_capture(capture: &Result<Vec<u8>, io::Error>) -> String {
    capture.as_ref().map_or_else(
        |_| String::new(),
        |bytes| String::from_utf8_lossy(bytes).into_owned(),
    )
}

fn capture_error(cause: io::Error, stdout: String, stderr: String) -> NativeCommandError {
    NativeCommandError {
        code: os_error_code(&cause),
        stdout,
        stderr,
        message: cause.to_string(),
        cause,
    }
}

fn exit_error(
    status: ExitStatus,
    command: &OsString,
    stdout: String,
    stderr: String,
) -> NativeCommandError {
    let code = status.code();
    let named = if code.is_none() {
        "PROCESS_SIGNAL"
    } else {
        "UNKNOWN"
    };
    let source_code = code.map_or(NativeCommandCode::Named(named), NativeCommandCode::Exit);
    let message = format!(
        "Command failed: {} (status {status})",
        command.to_string_lossy()
    );
    NativeCommandError {
        code: source_code,
        stdout,
        stderr,
        cause: io::Error::other(message.clone()),
        message,
    }
}

fn os_error_code(error: &io::Error) -> NativeCommandCode {
    let name = match error.kind() {
        io::ErrorKind::NotFound => "ENOENT",
        io::ErrorKind::PermissionDenied => "EACCES",
        io::ErrorKind::AlreadyExists => "EEXIST",
        io::ErrorKind::InvalidInput => "EINVAL",
        io::ErrorKind::Interrupted => "EINTR",
        _ => "UNKNOWN",
    };
    NativeCommandCode::Named(name)
}

#[cfg(windows)]
fn hide_windows_console(command: &mut Command) {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.as_std_mut().creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_windows_console(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell_command(script: &str) -> (String, Vec<String>) {
        #[cfg(windows)]
        {
            (
                "cmd.exe".to_owned(),
                vec!["/C".to_owned(), script.to_owned()],
            )
        }
        #[cfg(not(windows))]
        {
            ("sh".to_owned(), vec!["-c".to_owned(), script.to_owned()])
        }
    }

    #[tokio::test]
    async fn captures_utf8_stdout_and_stderr_on_zero_exit() {
        let (command, args) = shell_command("printf 'out✓'; printf 'err' >&2");
        let result = run_native_command(command, &args, &AbortSignal::default())
            .await
            .unwrap();
        assert_eq!(
            result,
            NativeCommandOutput {
                stdout: "out✓".into(),
                stderr: "err".into()
            }
        );
    }

    #[tokio::test]
    async fn nonzero_exit_carries_code_output_and_cause() {
        let (command, args) = shell_command("printf 'partial'; printf 'boom' >&2; exit 3");
        let error = run_native_command(command, &args, &AbortSignal::default())
            .await
            .unwrap_err();
        assert_eq!(error.code, NativeCommandCode::Exit(3));
        assert_eq!(error.stdout, "partial");
        assert_eq!(error.stderr, "boom");
        assert!(!error.cause().to_string().is_empty());
    }

    #[tokio::test]
    async fn missing_executable_reports_enoent() {
        let error = run_native_command(
            "seekdeep-definitely-missing-command",
            &[] as &[&str],
            &AbortSignal::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, NativeCommandCode::Named("ENOENT"));
    }

    #[tokio::test]
    async fn abort_terminates_and_reaps_child() {
        let (command, args) = shell_command("sleep 60");
        let signal = AbortSignal::default();
        let running_signal = signal.clone();
        let pending =
            tokio::spawn(async move { run_native_command(command, &args, &running_signal).await });
        signal.abort();
        let error = tokio::time::timeout(std::time::Duration::from_secs(3), pending)
            .await
            .expect("aborted child must terminate promptly")
            .unwrap()
            .unwrap_err();
        assert_eq!(error.code, NativeCommandCode::Named("ABORT_ERR"));
    }
}
