//! CommandLineToArgvW-compatible quoting.

use std::{path::Path, time::Duration};

use crate::{NativeHandle, Win32Error, abi};

/// Quotes one argument using Microsoft's backslash-before-quote rules.
#[must_use]
pub fn quote_arg(argument: &str) -> String {
    if argument.is_empty() {
        return "\"\"".to_owned();
    }
    if !argument
        .chars()
        .any(|character| character.is_whitespace() || character == '"')
    {
        return argument.to_owned();
    }
    let mut quoted = String::from('"');
    let characters = argument.chars().collect::<Vec<_>>();
    let mut index = 0;
    while index < characters.len() {
        let mut backslashes = 0;
        while index < characters.len() && characters[index] == '\\' {
            backslashes += 1;
            index += 1;
        }
        if index == characters.len() {
            quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
        } else if characters[index] == '"' {
            quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
            quoted.push('"');
            index += 1;
        } else {
            quoted.extend(std::iter::repeat_n('\\', backslashes));
            quoted.push(characters[index]);
            index += 1;
        }
    }
    quoted.push('"');
    quoted
}

/// Joins program and arguments into the exact `CreateProcess` command line.
#[must_use]
pub fn build_command_line(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(quote_arg)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Standard handles encoded into `STARTUPINFOW`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartupHandles {
    /// Child stdin.
    pub stdin: NativeHandle,
    /// Child stdout.
    pub stdout: NativeHandle,
    /// Child stderr.
    pub stderr: NativeHandle,
}

/// Decoded `PROCESS_INFORMATION` subset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessInfo {
    /// Process handle, absent on a malformed successful result.
    pub process: Option<NativeHandle>,
    /// Primary thread handle, absent on a malformed successful result.
    pub thread: Option<NativeHandle>,
    /// Child process ID.
    pub process_id: u32,
    /// Child thread ID.
    pub thread_id: u32,
}

/// `PeekNamedPipe` result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeekResult {
    /// Whether the API succeeded.
    pub succeeded: bool,
    /// Bytes currently available without blocking.
    pub available: u32,
}

/// Safe call seam implemented by the native Windows adapter.
pub trait SpawnBindings: Send + Sync {
    /// Returns the calling thread's last Win32 error.
    fn last_error(&self) -> u32;
    /// Formats a Win32 error, or returns an empty string.
    fn format_message(&self, code: u32) -> String;
    /// Creates one anonymous pipe.
    fn create_pipe(&self) -> (bool, Option<NativeHandle>, Option<NativeHandle>);
    /// Changes a handle's inheritability mask.
    fn set_handle_information(&self, handle: NativeHandle, mask: u32, flags: u32) -> bool;
    /// Creates a process under the restricted token.
    fn create_process_as_user(
        &self,
        token: NativeHandle,
        command_line: &str,
        cwd: &Path,
        startup: StartupHandles,
        creation_flags: u32,
    ) -> (bool, ProcessInfo);
    /// Closes a kernel handle.
    fn close_handle(&self, handle: NativeHandle) -> bool;
    /// Non-blockingly checks one pipe.
    fn peek_named_pipe(&self, handle: NativeHandle) -> PeekResult;
    /// Reads at most the supplied buffer length.
    fn read_file(&self, handle: NativeHandle, buffer: &mut [u8]) -> (bool, u32);
    /// Waits for one handle.
    fn wait_for_single_object(&self, handle: NativeHandle, milliseconds: u32) -> u32;
    /// Reads the process exit code.
    fn get_exit_code_process(&self, process: NativeHandle) -> (bool, u32);
    /// Creates an unnamed job object.
    fn create_job_object(&self) -> NativeHandle;
    /// Applies one extended-limit information buffer.
    fn set_information_job_object(&self, job: NativeHandle, information: &[u8]) -> bool;
    /// Fetches a standard handle by selector.
    fn get_std_handle(&self, selector: i32) -> NativeHandle;
    /// Assigns a suspended process to a job.
    fn assign_process_to_job_object(&self, job: NativeHandle, process: NativeHandle) -> bool;
    /// Terminates a process best-effort.
    fn terminate_process(&self, process: NativeHandle, exit_code: u32) -> bool;
    /// Resumes a suspended thread, returning the previous suspend count.
    fn resume_thread(&self, thread: NativeHandle) -> u32;
}

/// Spawn orchestration failure.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum SpawnError {
    /// Checked Win32 failure.
    #[error(transparent)]
    Win32(#[from] Win32Error),
    /// `CreateProcessAsUserW` reported success without both owned handles.
    #[error("CreateProcessAsUserW succeeded but returned null process/thread handles (pid {pid})")]
    NullProcessHandles {
        /// Reported child PID.
        pid: u32,
    },
}

fn last_error(
    api: &dyn SpawnBindings,
    name: &'static str,
    detail: impl Into<String>,
) -> Win32Error {
    let code = api.last_error();
    let detail = detail.into();
    let detail = if detail.is_empty() {
        api.format_message(code)
    } else {
        detail
    };
    Win32Error::new(name, code, (!detail.is_empty()).then_some(detail))
}

fn returned_error(
    api: &dyn SpawnBindings,
    name: &'static str,
    code: u32,
    detail: impl Into<String>,
) -> Win32Error {
    let detail = detail.into();
    let detail = if detail.is_empty() {
        api.format_message(code)
    } else {
        detail
    };
    Win32Error::new(name, code, (!detail.is_empty()).then_some(detail))
}

fn create_pipe(api: &dyn SpawnBindings) -> Result<(NativeHandle, NativeHandle), SpawnError> {
    let (created, read, write) = api.create_pipe();
    if !created {
        return Err(last_error(api, "CreatePipe", "").into());
    }
    match (read, write) {
        (Some(read), Some(write)) if !read.is_null() && !write.is_null() => Ok((read, write)),
        _ => Err(last_error(api, "CreatePipe", "null pipe handle").into()),
    }
}

fn set_inheritable(
    api: &dyn SpawnBindings,
    handle: NativeHandle,
    label: &str,
) -> Result<(), SpawnError> {
    if api.set_handle_information(handle, abi::HANDLE_FLAG_INHERIT, abi::HANDLE_FLAG_INHERIT) {
        Ok(())
    } else {
        Err(last_error(api, "SetHandleInformation", label).into())
    }
}

/// Command, argv, and working directory for one native spawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnOptions<'a> {
    /// Program to resolve using Windows process lookup.
    pub command: &'a str,
    /// Remaining argv.
    pub args: &'a [String],
    /// Child working directory.
    pub cwd: &'a Path,
}

/// A piped child and the two host read ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpawnedNative {
    /// Child PID.
    pub pid: u32,
    /// Child process handle.
    pub process: NativeHandle,
    /// Host stdout pipe read end.
    pub stdout_read: NativeHandle,
    /// Host stderr pipe read end.
    pub stderr_read: NativeHandle,
}

/// Spawns under a restricted token with closed stdin and captured output.
///
/// # Errors
///
/// Returns exact pipe, inheritance, creation, or malformed-success failures.
pub fn spawn_sandboxed(
    api: &dyn SpawnBindings,
    token: NativeHandle,
    options: &SpawnOptions<'_>,
) -> Result<SpawnedNative, SpawnError> {
    let stdin = create_pipe(api)?;
    let stdout = create_pipe(api)?;
    let stderr = create_pipe(api)?;
    set_inheritable(api, stdin.0, "stdin read end")?;
    set_inheritable(api, stdout.1, "stdout write end")?;
    set_inheritable(api, stderr.1, "stderr write end")?;
    let startup = StartupHandles {
        stdin: stdin.0,
        stdout: stdout.1,
        stderr: stderr.1,
    };
    let command_line = build_command_line(options.command, options.args);
    let (created, info) = api.create_process_as_user(token, &command_line, options.cwd, startup, 0);
    if !created {
        let code = api.last_error();
        for handle in [stdin.0, stdin.1, stdout.0, stdout.1, stderr.0, stderr.1] {
            let _ = api.close_handle(handle);
        }
        return Err(returned_error(
            api,
            "CreateProcessAsUserW",
            code,
            format!(
                "command: {}, cwd: {}",
                options.command,
                options.cwd.display()
            ),
        )
        .into());
    }
    let (Some(process), Some(thread)) = (info.process, info.thread) else {
        return Err(SpawnError::NullProcessHandles {
            pid: info.process_id,
        });
    };
    for handle in [stdin.0, stdout.1, stderr.1, stdin.1, thread] {
        let _ = api.close_handle(handle);
    }
    Ok(SpawnedNative {
        pid: info.process_id,
        process,
        stdout_read: stdout.0,
        stderr_read: stderr.0,
    })
}

/// Drains one pipe with bounded non-blocking polling and closes it at EOF.
///
/// # Errors
///
/// Returns exact peek or read failures. Like the source, those exceptional
/// paths leave handle recovery to process teardown.
pub async fn drain_pipe(
    api: &dyn SpawnBindings,
    handle: NativeHandle,
) -> Result<Vec<u8>, SpawnError> {
    let mut output = Vec::new();
    let mut chunks = 0_usize;
    loop {
        let peek = api.peek_named_pipe(handle);
        if !peek.succeeded {
            let code = api.last_error();
            if code == abi::ERROR_BROKEN_PIPE || code == abi::ERROR_NO_DATA {
                break;
            }
            return Err(returned_error(
                api,
                "PeekNamedPipe",
                code,
                format!("drain failure after {chunks} chunk(s)"),
            )
            .into());
        }
        if peek.available > 0 {
            let mut chunk = vec![0_u8; peek.available as usize];
            let (read, count) = api.read_file(handle, &mut chunk);
            if !read {
                return Err(last_error(
                    api,
                    "ReadFile",
                    format!("drain failure after {chunks} chunk(s)"),
                )
                .into());
            }
            let count = usize::try_from(count)
                .unwrap_or(usize::MAX)
                .min(chunk.len());
            output.extend_from_slice(&chunk[..count]);
            chunks += 1;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let _ = api.close_handle(handle);
    Ok(output)
}

/// Waits for process exit, obtains its full 32-bit code, and closes the handle.
///
/// # Errors
///
/// Returns exact wait or exit-code failures.
pub fn wait_for_exit(api: &dyn SpawnBindings, process: NativeHandle) -> Result<u32, SpawnError> {
    if api.wait_for_single_object(process, abi::INFINITE) == u32::MAX {
        return Err(last_error(api, "WaitForSingleObject", "").into());
    }
    let (read, code) = api.get_exit_code_process(process);
    if !read {
        return Err(last_error(api, "GetExitCodeProcess", "").into());
    }
    let _ = api.close_handle(process);
    Ok(code)
}

fn create_kill_on_close_job(api: &dyn SpawnBindings) -> Result<NativeHandle, SpawnError> {
    let job = api.create_job_object();
    if job.is_null() {
        return Err(last_error(api, "CreateJobObjectW", "").into());
    }
    let mut information = vec![0_u8; abi::JOBOBJECT_EXTENDED_LIMIT_SIZE];
    information[abi::JOBOBJECT_EXTENDED_LIMIT_FLAGS_OFFSET
        ..abi::JOBOBJECT_EXTENDED_LIMIT_FLAGS_OFFSET + 4]
        .copy_from_slice(&abi::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE.to_le_bytes());
    if !api.set_information_job_object(job, &information) {
        let code = api.last_error();
        let _ = api.close_handle(job);
        return Err(returned_error(api, "SetInformationJobObject", code, "").into());
    }
    Ok(job)
}

/// A child whose inherited stdio and kill-on-close job are runner-owned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpawnedInherited {
    /// Child PID.
    pub pid: u32,
    /// Child process handle.
    pub process: NativeHandle,
    /// Kill-on-close job retained for the child lifetime.
    pub job: NativeHandle,
}

/// Spawns suspended with inherited stdio, assigns a kill-on-close job, then resumes.
///
/// # Errors
///
/// Returns exact job, stdio, creation, assignment, resume, or malformed-success failures.
pub fn spawn_sandboxed_inherited(
    api: &dyn SpawnBindings,
    token: NativeHandle,
    options: &SpawnOptions<'_>,
) -> Result<SpawnedInherited, SpawnError> {
    let job = create_kill_on_close_job(api)?;
    let stdin = api.get_std_handle(abi::STD_INPUT_HANDLE);
    let stdout = api.get_std_handle(abi::STD_OUTPUT_HANDLE);
    let stderr = api.get_std_handle(abi::STD_ERROR_HANDLE);
    if stdin.is_null() || stdout.is_null() || stderr.is_null() {
        let _ = api.close_handle(job);
        return Err(last_error(api, "GetStdHandle", "null standard handle").into());
    }
    for (handle, label) in [(stdin, "stdin"), (stdout, "stdout"), (stderr, "stderr")] {
        if !api.set_handle_information(handle, abi::HANDLE_FLAG_INHERIT, abi::HANDLE_FLAG_INHERIT) {
            return Err(last_error(
                api,
                "SetHandleInformation",
                format!("{label} (enable inherit)"),
            )
            .into());
        }
    }
    let startup = StartupHandles {
        stdin,
        stdout,
        stderr,
    };
    let command_line = build_command_line(options.command, options.args);
    let (created, info) = api.create_process_as_user(
        token,
        &command_line,
        options.cwd,
        startup,
        abi::CREATE_SUSPENDED,
    );
    for handle in [stdin, stdout, stderr] {
        let _ = api.set_handle_information(handle, abi::HANDLE_FLAG_INHERIT, 0);
    }
    if !created {
        let code = api.last_error();
        let _ = api.close_handle(job);
        return Err(returned_error(
            api,
            "CreateProcessAsUserW",
            code,
            format!(
                "command: {}, cwd: {}",
                options.command,
                options.cwd.display()
            ),
        )
        .into());
    }
    let (Some(process), Some(thread)) = (info.process, info.thread) else {
        let _ = api.close_handle(job);
        return Err(SpawnError::NullProcessHandles {
            pid: info.process_id,
        });
    };
    if !api.assign_process_to_job_object(job, process) {
        let code = api.last_error();
        let _ = api.terminate_process(process, 1);
        let _ = api.close_handle(thread);
        let _ = api.close_handle(process);
        let _ = api.close_handle(job);
        return Err(returned_error(
            api,
            "AssignProcessToJobObject",
            code,
            format!("pid {}", info.process_id),
        )
        .into());
    }
    if api.resume_thread(thread) == u32::MAX {
        let code = api.last_error();
        let _ = api.close_handle(thread);
        let _ = api.close_handle(process);
        let _ = api.close_handle(job);
        return Err(returned_error(
            api,
            "ResumeThread",
            code,
            format!("pid {}", info.process_id),
        )
        .into());
    }
    let _ = api.close_handle(thread);
    Ok(SpawnedInherited {
        pid: info.process_id,
        process,
        job,
    })
}
