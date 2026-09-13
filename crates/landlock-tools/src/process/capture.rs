use std::{
    io,
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc::{self, SyncSender},
    thread,
};

use anyhow::{Context as _, Result};

use super::{CommandOutput, unstarted};

#[derive(Clone, Copy)]
enum Pipe {
    Stdout,
    Stderr,
}

struct Chunk {
    pipe: Pipe,
    bytes: Vec<u8>,
}

struct ChildOwner {
    child: Child,
    reaped: bool,
}

impl ChildOwner {
    fn terminate(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use nix::{
                errno::Errno,
                sys::signal::{Signal, kill},
                unistd::Pid,
            };
            let pid = i32::try_from(self.child.id()).map_err(io::Error::other)?;
            match kill(Pid::from_raw(pid), Signal::SIGTERM) {
                Ok(()) | Err(Errno::ESRCH) => Ok(()),
                Err(error) => Err(io::Error::from_raw_os_error(error as i32)),
            }
        }
        #[cfg(not(unix))]
        {
            self.child.kill()
        }
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }
}

impl Drop for ChildOwner {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn read_pipe(mut reader: impl io::Read, pipe: Pipe, sender: &SyncSender<io::Result<Chunk>>) {
    let mut buffer = vec![0; 65_536];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                let chunk = Chunk {
                    pipe,
                    bytes: buffer[..count].to_owned(),
                };
                if sender.send(Ok(chunk)).is_err() {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                let _ = sender.send(Err(error));
                break;
            }
        }
    }
}

pub(super) fn run(command: &mut Command, program: &str, limit: usize) -> Result<CommandOutput> {
    thread::scope(|scope| {
        let child = match command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => return Ok(unstarted(program, &error)),
        };
        let mut owner = ChildOwner {
            child,
            reaped: false,
        };
        let stdout = owner
            .child
            .stdout
            .take()
            .context("missing captured stdout")?;
        let stderr = owner
            .child
            .stderr
            .take()
            .context("missing captured stderr")?;
        let (sender, receiver) = mpsc::sync_channel(4);
        let stderr_sender = sender.clone();
        scope.spawn(move || read_pipe(stdout, Pipe::Stdout, &sender));
        scope.spawn(move || read_pipe(stderr, Pipe::Stderr, &stderr_sender));
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut overflow = false;
        let mut failure = None;
        for chunk in receiver {
            match chunk {
                Ok(chunk) if !overflow && failure.is_none() => {
                    match chunk.pipe {
                        Pipe::Stdout => stdout.extend(chunk.bytes),
                        Pipe::Stderr => stderr.extend(chunk.bytes),
                    }
                    if stdout.len() + stderr.len() > limit {
                        overflow = true;
                        if let Err(error) = owner.terminate() {
                            failure = Some(error);
                            let _ = owner.child.kill();
                        }
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    failure.get_or_insert(error);
                    let _ = owner.child.kill();
                }
            }
        }
        let status = owner.wait()?;
        if let Some(error) = failure {
            return Err(error.into());
        }
        Ok(CommandOutput {
            status: status.code(),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            spawn_error: overflow.then(|| format!("spawnSync {program} ENOBUFS")),
            unstarted: false,
        })
    })
}
