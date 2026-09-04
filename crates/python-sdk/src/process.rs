//! Owned native child process, UTF-8 standard streams, and bounded reader teardown.

use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::Path,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use parking_lot::{Condvar, Mutex};
use serde_json::json;

use crate::{Error, ErrorKind, Result};

#[derive(Default)]
pub(crate) struct Completion {
    done: Mutex<bool>,
    changed: Condvar,
}

impl Completion {
    pub(crate) fn finish(&self) {
        *self.done.lock() = true;
        self.changed.notify_all();
    }

    fn wait(&self, timeout: Duration) -> bool {
        let started = Instant::now();
        let mut done = self.done.lock();
        while !*done {
            let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
                return false;
            };
            self.changed.wait_for(&mut done, remaining);
        }
        true
    }
}

/// Process identity remains inspectable after its owning client closes.
pub struct RuntimeProcess {
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    argv: Vec<String>,
    pub(crate) stdout_done: Completion,
    pub(crate) stderr_done: Completion,
    pub(crate) cancelled: AtomicBool,
    pub(crate) readers: Mutex<Vec<JoinHandle<()>>>,
}

impl RuntimeProcess {
    pub(crate) fn spawn(
        argv: Vec<String>,
        cwd: Option<&Path>,
        environment: &BTreeMap<String, String>,
    ) -> Result<(Arc<Self>, ChildStdout, ChildStderr)> {
        let Some(program) = argv.first() else {
            return Err(Error::new(ErrorKind::Value, "runtime argv is empty"));
        };
        if argv.iter().any(|value| value.contains('\0'))
            || environment
                .iter()
                .any(|(key, value)| key.contains('\0') || value.contains('\0'))
        {
            return Err(Error::new(ErrorKind::Value, "embedded null byte"));
        }
        let mut command = Command::new(program);
        command
            .args(&argv[1..])
            .env_clear()
            .envs(environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn().map_err(|error| {
            let filename = cwd
                .filter(|cwd| !cwd.is_dir())
                .map_or_else(|| program.clone(), |cwd| cwd.to_string_lossy().into_owned());
            Error::io(&error, Some(filename))
        })?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("piped child stdout");
        let stderr = child.stderr.take().expect("piped child stderr");
        Ok((
            Arc::new(Self {
                child: Mutex::new(child),
                stdin: Mutex::new(stdin),
                argv,
                stdout_done: Completion::default(),
                stderr_done: Completion::default(),
                cancelled: AtomicBool::new(false),
                readers: Mutex::new(Vec::new()),
            }),
            stdout,
            stderr,
        ))
    }

    /// Operating-system child process identifier.
    pub fn pid(&self) -> u32 {
        self.child.lock().id()
    }

    /// Returns an observed exit code; Unix signal exits are negative signal numbers.
    ///
    /// # Errors
    /// Propagates a native wait-status error.
    pub fn poll(&self) -> Result<Option<i32>> {
        self.child
            .lock()
            .try_wait()
            .map(|status| status.map(exit_code))
            .map_err(|error| Error::io(&error, None))
    }

    /// Sends the platform's ordinary termination request.
    ///
    /// # Errors
    /// Propagates native termination failures other than an already-gone process.
    pub fn terminate(&self) -> Result<()> {
        #[cfg(unix)]
        {
            use nix::{
                errno::Errno,
                sys::signal::{Signal, kill},
                unistd::Pid,
            };
            let mut child = self.child.lock();
            if child
                .try_wait()
                .map_err(|error| Error::io(&error, None))?
                .is_some()
            {
                return Ok(());
            }
            // Keep the wait-status lock through signal delivery so another reader cannot reap and release this PID.
            let pid = i32::try_from(child.id()).map_err(|_| {
                Error::new(ErrorKind::Value, "runtime PID exceeds the native PID range")
            })?;
            match kill(Pid::from_raw(pid), Signal::SIGTERM) {
                Ok(()) | Err(Errno::ESRCH) => Ok(()),
                Err(error) => Err(Error::io(
                    &std::io::Error::from_raw_os_error(error as i32),
                    None,
                )),
            }
        }
        #[cfg(not(unix))]
        self.kill()
    }

    /// Forcibly terminates the child process.
    ///
    /// # Errors
    /// Propagates a native kill failure.
    pub fn kill(&self) -> Result<()> {
        self.child
            .lock()
            .kill()
            .map_err(|error| Error::io(&error, None))
    }

    /// Reaps the child, using the same timeout independently of any request deadline.
    ///
    /// # Errors
    /// Returns a process-timeout exception when the interval elapses, or a native wait error.
    pub fn wait(&self, timeout: Option<f64>) -> Result<i32> {
        let started = Instant::now();
        loop {
            if let Some(code) = self.poll()? {
                return Ok(code);
            }
            if timeout.is_some_and(|timeout| started.elapsed().as_secs_f64() >= timeout) {
                let mut error = Error::new(
                    ErrorKind::SubprocessTimeout,
                    "runtime process wait timed out",
                );
                error.data = Some(json!({"cmd":self.argv,"timeout":timeout}));
                return Err(error);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub(crate) fn write(&self, payload: &[u8]) -> Result<()> {
        let mut stdin = self.stdin.lock();
        let stdin = stdin.as_mut().ok_or_else(|| {
            Error::new(
                ErrorKind::TransportClosed,
                "SeekDeep Harness runtime is not running",
            )
        })?;
        stdin
            .write_all(payload)
            .and_then(|()| stdin.flush())
            .map_err(|error| Error::io(&error, None))
    }

    pub(crate) fn close_stdin(&self) {
        self.stdin.lock().take();
    }

    pub(crate) fn collect_final_stderr(&self) {
        if self.poll().is_ok_and(|status| status.is_some()) {
            self.stderr_done.wait(Duration::from_millis(100));
        }
    }

    pub(crate) fn finish_readers(&self) {
        self.stdout_done.wait(Duration::from_millis(500));
        self.stderr_done.wait(Duration::from_millis(500));
        self.cancelled.store(true, Ordering::Release);
        for reader in self.readers.lock().drain(..) {
            if reader.is_finished() && reader.thread().id() != std::thread::current().id() {
                let _ = reader.join();
            }
        }
    }
}

fn exit_code(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        status
            .code()
            .unwrap_or_else(|| -status.signal().unwrap_or(1))
    }
    #[cfg(not(unix))]
    status.code().unwrap_or(1)
}

pub(crate) trait StreamReader: Read {
    fn read_ready(&mut self, bytes: &mut [u8], cancelled: &AtomicBool) -> std::io::Result<usize>;
}

#[cfg(unix)]
impl<T: Read + std::os::fd::AsFd> StreamReader for T {
    fn read_ready(&mut self, bytes: &mut [u8], cancelled: &AtomicBool) -> std::io::Result<usize> {
        use nix::poll::{PollFd, PollFlags, poll};
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Ok(0);
            }
            let mut descriptors = [PollFd::new(self.as_fd(), PollFlags::POLLIN)];
            match poll(&mut descriptors, 20_u16) {
                Ok(0) | Err(nix::errno::Errno::EINTR) => {}
                Ok(_) => return self.read(bytes),
                Err(error) => return Err(std::io::Error::from_raw_os_error(error as i32)),
            }
        }
    }
}

#[cfg(not(unix))]
impl<T: Read> StreamReader for T {
    fn read_ready(&mut self, bytes: &mut [u8], cancelled: &AtomicBool) -> std::io::Result<usize> {
        if cancelled.load(Ordering::Acquire) {
            Ok(0)
        } else {
            self.read(bytes)
        }
    }
}

pub(crate) fn read_lines(
    mut reader: impl StreamReader,
    cancelled: &AtomicBool,
    mut on_line: impl FnMut(String),
) -> Result<()> {
    let mut bytes = [0_u8; 8192];
    let mut line = Vec::new();
    let mut after_cr = false;
    loop {
        let count = reader
            .read_ready(&mut bytes, cancelled)
            .map_err(|error| Error::io(&error, None))?;
        if count == 0 {
            if !line.is_empty() && !cancelled.load(Ordering::Acquire) {
                on_line(decode_line(&line)?);
            }
            return Ok(());
        }
        for &byte in &bytes[..count] {
            if after_cr && byte == b'\n' {
                after_cr = false;
                continue;
            }
            after_cr = byte == b'\r';
            if byte == b'\n' || byte == b'\r' {
                on_line(decode_line(&line)?);
                line.clear();
            } else {
                line.push(byte);
            }
        }
    }
}

fn decode_line(bytes: &[u8]) -> Result<String> {
    String::from_utf8(bytes.to_vec()).map_err(|failure| {
        let invalid = failure.utf8_error();
        let start = invalid.valid_up_to();
        let end = invalid
            .error_len()
            .map_or(bytes.len(), |length| start + length);
        let reason = if invalid.error_len().is_none() {
            "unexpected end of data"
        } else if bytes
            .get(start)
            .is_some_and(|byte| matches!(byte, 0xc2..=0xf4))
        {
            "invalid continuation byte"
        } else {
            "invalid start byte"
        };
        let mut error = Error::new(ErrorKind::UnicodeDecode, reason);
        error.data =
            Some(json!({"encoding":"utf-8","bytes":bytes,"start":start,"end":end,"reason":reason}));
        error
    })
}
