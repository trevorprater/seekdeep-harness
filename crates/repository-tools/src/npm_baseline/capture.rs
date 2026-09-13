use std::{
    io::{self, Read},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
};

use crate::release_process::{ReleaseCommandResult, ReleaseRunOptions};

const MAX_CAPTURE_BYTES: usize = 16 * 1024 * 1024;

pub(super) fn capture_process(
    command: &str,
    arguments: &[String],
    options: &ReleaseRunOptions,
) -> anyhow::Result<ReleaseCommandResult> {
    let mut builder = Command::new(command);
    builder
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &options.cwd {
        builder.current_dir(cwd);
    }
    if let Some(environment) = &options.env {
        builder.env_clear().envs(environment);
    }
    let mut owner = CapturedChild {
        child: builder
            .spawn()
            .map_err(|error| spawn_failure(command, &error))?,
        reaped: false,
    };
    let stdout = owner
        .child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("captured process has no stdout pipe"))?;
    let stderr = owner
        .child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("captured process has no stderr pipe"))?;
    let (sender, receiver) = mpsc::sync_channel(4);
    std::thread::scope(|scope| {
        let stdout_sender = sender.clone();
        scope.spawn(move || read_stream(stdout, Stream::Stdout, &stdout_sender));
        scope.spawn(move || read_stream(stderr, Stream::Stderr, &sender));
        collect_output(command, &mut owner, &receiver)
    })
}

#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

enum StreamRead {
    Bytes(Stream, Vec<u8>),
    Error(io::Error),
}

fn read_stream(mut stream: impl Read, kind: Stream, sender: &SyncSender<StreamRead>) {
    let mut buffer = vec![0_u8; 65_536];
    loop {
        let item = match stream.read(&mut buffer) {
            Ok(0) => return,
            Ok(length) => StreamRead::Bytes(kind, buffer[..length].to_owned()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                let _ = sender.send(StreamRead::Error(error));
                return;
            }
        };
        if sender.send(item).is_err() {
            return;
        }
    }
}

fn collect_output(
    command: &str,
    owner: &mut CapturedChild,
    receiver: &Receiver<StreamRead>,
) -> anyhow::Result<ReleaseCommandResult> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut error = None;
    for item in receiver {
        if error.is_some() {
            continue;
        }
        match item {
            StreamRead::Bytes(stream, bytes) => {
                if stdout.len() + stderr.len() + bytes.len() > MAX_CAPTURE_BYTES {
                    error = Some(anyhow::anyhow!("spawnSync {command} ENOBUFS"));
                    if owner.terminate().is_err() {
                        let _ = owner.child.kill();
                    }
                } else {
                    match stream {
                        Stream::Stdout => stdout.extend(bytes),
                        Stream::Stderr => stderr.extend(bytes),
                    }
                }
            }
            StreamRead::Error(read_error) => {
                error = Some(read_error.into());
                let _ = owner.child.kill();
            }
        }
    }
    let status = owner.child.wait();
    owner.reaped = status.is_ok();
    if let Some(error) = error {
        return Err(error);
    }
    Ok(ReleaseCommandResult {
        status: Some(status?.code().unwrap_or(1)),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

struct CapturedChild {
    child: Child,
    reaped: bool,
}

pub(super) fn spawn_failure(command: &str, error: &io::Error) -> anyhow::Error {
    #[cfg(unix)]
    if let Some(code) = error.raw_os_error() {
        let errno = nix::errno::Errno::from_raw(code);
        if errno != nix::errno::Errno::UnknownErrno {
            return anyhow::anyhow!("spawnSync {command} {errno:?}");
        }
    }
    anyhow::anyhow!("spawnSync {command} {error}")
}

impl CapturedChild {
    fn terminate(&mut self) -> anyhow::Result<()> {
        #[cfg(unix)]
        {
            use nix::{sys::signal::Signal, unistd::Pid};
            nix::sys::signal::kill(
                Pid::from_raw(i32::try_from(self.child.id())?),
                Signal::SIGTERM,
            )?;
        }
        #[cfg(not(unix))]
        self.child.kill()?;
        Ok(())
    }
}

impl Drop for CapturedChild {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
