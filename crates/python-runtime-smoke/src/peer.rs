//! Line-oriented child ownership for direct JSON-RPC and the installed-Python adapter.

use std::{
    process::{ExitStatus, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::Value;
use tokio::{
    io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{mpsc, watch},
    task::JoinHandle,
};

#[derive(Debug)]
pub(crate) struct Interrupted;

impl std::fmt::Display for Interrupted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("KeyboardInterrupt")
    }
}

impl std::error::Error for Interrupted {}

pub(crate) struct Peer {
    child: Child,
    input: Option<ChildStdin>,
    output: mpsc::UnboundedReceiver<std::io::Result<String>>,
    stderr: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
    cancellation: Option<watch::Receiver<bool>>,
    accept_interrupt_exit: bool,
}

impl Peer {
    pub(crate) fn spawn(command: &mut Command) -> anyhow::Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let input = child.stdin.take();
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (sender, output) = mpsc::unbounded_channel();
        let observation = Arc::new(Mutex::new(String::new()));
        let text = Arc::clone(&observation);
        let readers = vec![
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                loop {
                    match lines.next_line().await {
                        Ok(Some(line)) => {
                            if sender.send(Ok(line)).is_err() {
                                break;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let _ = sender.send(Err(error));
                            break;
                        }
                    }
                }
            }),
            tokio::spawn(async move {
                let mut stderr = BufReader::new(stderr);
                loop {
                    let mut line = String::new();
                    match stderr.read_line(&mut line).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => text
                            .lock()
                            .expect("stderr observation lock")
                            .push_str(&line),
                    }
                }
            }),
        ];
        Ok(Self {
            child,
            input,
            output,
            stderr: observation,
            readers,
            cancellation: None,
            accept_interrupt_exit: false,
        })
    }

    pub(crate) fn cancel_on(&mut self, cancellation: watch::Receiver<bool>) {
        self.cancellation = Some(cancellation);
    }

    pub(crate) fn begin_interrupt_cleanup(&mut self) {
        self.cancellation.take();
        // The adapter can receive the foreground Ctrl-C along with its Rust owner.
        self.accept_interrupt_exit = true;
    }

    pub(crate) async fn send(&mut self, value: &Value) -> anyhow::Result<()> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("runtime stdin is unavailable"))?;
        let write = async {
            input
                .write_all(crate::json::dumps(value, false, true).as_bytes())
                .await?;
            input.write_all(b"\n").await?;
            input.flush().await?;
            Ok(())
        };
        tokio::select! {
            biased;
            () = cancelled(&mut self.cancellation) => Err(Interrupted.into()),
            result = write => result,
        }
    }

    pub(crate) fn finish_input(&mut self) {
        self.input.take();
    }

    pub(crate) async fn read_until(
        &mut self,
        predicate: impl Fn(&Value) -> bool,
    ) -> anyhow::Result<Vec<Value>> {
        self.read_matching(
            predicate,
            Some(tokio::time::Instant::now() + Duration::from_secs(60)),
        )
        .await
    }

    pub(crate) async fn read_adapter(&mut self) -> anyhow::Result<Vec<Value>> {
        // Each SDK request owns its timeout; several turns must not share a new deadline.
        self.read_matching(
            |message| message["results"].is_array() || message["error"].is_string(),
            None,
        )
        .await
    }

    async fn read_matching(
        &mut self,
        predicate: impl Fn(&Value) -> bool,
        deadline: Option<tokio::time::Instant>,
    ) -> anyhow::Result<Vec<Value>> {
        let mut messages = Vec::new();
        loop {
            let output = &mut self.output;
            let read = async {
                if let Some(deadline) = deadline {
                    tokio::time::timeout_at(deadline, output.recv()).await
                } else {
                    Ok(output.recv().await)
                }
            };
            let received = tokio::select! {
                biased;
                () = cancelled(&mut self.cancellation) => return Err(Interrupted.into()),
                result = read => result,
            }
            .map_err(|_| {
                anyhow::anyhow!(
                    "runtime timed out; messages={messages:?}; stderr={}",
                    self.stderr()
                )
            })?;
            let line = received.ok_or_else(|| {
                anyhow::anyhow!(
                    "runtime exited before expected message; stderr: {}",
                    self.stderr()
                )
            })??;
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            anyhow::ensure!(
                message.is_object(),
                "runtime emitted a non-object JSON message: {message}"
            );
            let done = predicate(&message);
            messages.push(message);
            if done {
                return Ok(messages);
            }
        }
    }

    pub(crate) async fn close(mut self) -> anyhow::Result<()> {
        self.finish_input();
        let status = if let Ok(status) =
            tokio::time::timeout(Duration::from_secs(10), self.child.wait()).await
        {
            status?
        } else {
            self.child.kill().await?;
            self.child.wait().await?
        };
        for mut reader in self.readers.drain(..) {
            if tokio::time::timeout(Duration::from_secs(5), &mut reader)
                .await
                .is_err()
            {
                reader.abort();
                let _ = reader.await;
            }
        }
        anyhow::ensure!(
            accepted_exit(status, self.accept_interrupt_exit),
            "runtime exited {status}; stderr: {}",
            self.stderr()
        );
        Ok(())
    }

    fn stderr(&self) -> String {
        self.stderr.lock().expect("stderr observation lock").clone()
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        for reader in &self.readers {
            reader.abort();
        }
    }
}

async fn cancelled(cancellation: &mut Option<watch::Receiver<bool>>) {
    let Some(cancellation) = cancellation else {
        return std::future::pending().await;
    };
    loop {
        if *cancellation.borrow() {
            return;
        }
        if cancellation.changed().await.is_err() {
            return std::future::pending().await;
        }
    }
}

fn accepted_exit(status: ExitStatus, interrupted: bool) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        status.success()
            || status.signal() == Some(15)
            || interrupted && (status.signal() == Some(2) || status.code() == Some(130))
    }
    #[cfg(not(unix))]
    {
        status.success() || interrupted && status.code() == Some(130)
    }
}
