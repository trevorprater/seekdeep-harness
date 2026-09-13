//! Host transport for the killable compiled workflow worker process.

use std::{collections::HashMap, path::PathBuf, process::Stdio, sync::Arc};

use futures::{FutureExt as _, future::BoxFuture};
use parking_lot::Mutex;
use tokio::{
    io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
    sync::{mpsc, oneshot},
};

use crate::{
    host::{ChildRegistry, RunObserver},
    protocol::{HostToWorkerMessage, WorkerToHostMessage},
    runtime::ExecutionObserver,
    types::WorkerInit,
};
use seekdeep_workflow::{WorkflowResult, WorkflowStopReason};

fn safe_error(error: &anyhow::Error) -> String {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| format!("{error:#}")))
        .unwrap_or_else(|_| "[unrenderable workflow process error]".to_owned())
}

fn process_error(message: impl Into<String>, agents_started: u64) -> WorkflowResult {
    WorkflowResult {
        value: serde_json::Value::Null,
        stop_reason: WorkflowStopReason::Error,
        error: Some(message.into()),
        agents_started,
    }
}

/// One compiled helper process and its typed host bridge.
#[derive(Clone, Debug)]
pub(crate) struct WorkerCommand {
    pub(crate) path: PathBuf,
    pub(crate) integrated: bool,
}

pub(crate) struct ProcessExecution {
    outbound: mpsc::UnboundedSender<HostToWorkerMessage>,
    result: futures::future::Shared<BoxFuture<'static, WorkflowResult>>,
    kill: mpsc::UnboundedSender<()>,
    terminated: futures::future::Shared<BoxFuture<'static, ()>>,
    ready: Arc<std::sync::atomic::AtomicBool>,
    launched: Arc<std::sync::atomic::AtomicBool>,
}

impl ProcessExecution {
    #[allow(clippy::too_many_lines)] // one closed process/protocol ownership transaction
    pub(crate) fn new(
        worker: &WorkerCommand,
        init: WorkerInit,
        registry: Arc<ChildRegistry>,
        observer: Arc<RunObserver>,
    ) -> anyhow::Result<Arc<Self>> {
        let mut command = tokio::process::Command::new(&worker.path);
        command
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if worker.integrated {
            command.env("SEEKDEEP_INTERNAL_WORKFLOW_WORKER", "1");
        }
        #[cfg(windows)]
        {
            let temp = std::env::temp_dir();
            command.env("TMP", &temp).env("TEMP", temp);
        }
        let mut child = command.spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("workflow worker lacks stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("workflow worker lacks stdout"))?;
        let (outbound, mut outbound_receive) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let Ok(init) = serde_json::to_vec(&init) else {
                return;
            };
            if stdin.write_all(&init).await.is_err()
                || stdin.write_all(b"\n").await.is_err()
                || stdin.flush().await.is_err()
            {
                return;
            }
            while let Some(message) = outbound_receive.recv().await {
                let Ok(line) = serde_json::to_vec(&message) else {
                    break;
                };
                if stdin.write_all(&line).await.is_err()
                    || stdin.write_all(b"\n").await.is_err()
                    || stdin.flush().await.is_err()
                {
                    break;
                }
            }
        });

        let (result_send, result_receive) = oneshot::channel();
        let result_send = Arc::new(Mutex::new(Some(result_send)));
        let missing_result_registry = registry.clone();
        let result = async move {
            result_receive.await.unwrap_or_else(|_| {
                process_error(
                    "workflow worker result channel closed",
                    missing_result_registry.started(),
                )
            })
        }
        .boxed()
        .shared();
        let (kill, mut kill_receive) = mpsc::unbounded_channel();
        let (exit_send, exit_receive) = oneshot::channel();
        tokio::spawn(async move {
            let _ = tokio::select! {
                status = child.wait() => status,
                _ = kill_receive.recv() => {
                    let _ = child.start_kill();
                    child.wait().await
                }
            };
            let _ = exit_send.send(());
        });
        let terminated = async move {
            let _ = exit_receive.await;
        }
        .boxed()
        .shared();

        let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let launched = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let read_ready = ready.clone();
        let read_launched = launched.clone();
        let read_outbound = outbound.clone();
        let read_result = result_send;
        let read_registry = registry;
        tokio::spawn(async move {
            let calls = Arc::new(Mutex::new(
                HashMap::<u64, Arc<dyn crate::types::ChildHandle>>::new(),
            ));
            let mut lines = BufReader::new(stdout).lines();
            loop {
                let line = match lines.next_line().await {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(error) => {
                        if let Some(send) = read_result.lock().take() {
                            let _ = send.send(process_error(
                                format!("workflow worker output failed: {error}"),
                                read_registry.started(),
                            ));
                        }
                        break;
                    }
                };
                let message = match serde_json::from_str::<WorkerToHostMessage>(&line) {
                    Ok(message) => message,
                    Err(error) => {
                        if let Some(send) = read_result.lock().take() {
                            let _ = send.send(process_error(
                                format!("workflow worker sent invalid JSON: {error}"),
                                read_registry.started(),
                            ));
                        }
                        break;
                    }
                };
                match message {
                    WorkerToHostMessage::Ready => {
                        read_ready.store(true, std::sync::atomic::Ordering::Release);
                        if read_launched.load(std::sync::atomic::Ordering::Acquire) {
                            let _ = read_outbound.send(HostToWorkerMessage::Go);
                        }
                    }
                    WorkerToHostMessage::Phase { title } => observer.phase(&title),
                    WorkerToHostMessage::Log { message } => observer.log(&message),
                    WorkerToHostMessage::AgentStart { info } => observer.agent_start(&info),
                    WorkerToHostMessage::AgentEnd { info } => observer.agent_end(&info),
                    WorkerToHostMessage::ChildStart { call_id, request } => {
                        let registry = read_registry.clone();
                        let outbound = read_outbound.clone();
                        let calls = calls.clone();
                        tokio::spawn(async move {
                            match registry.start(request).await {
                                Ok(run) => {
                                    calls.lock().insert(call_id, run.clone());
                                    let _ = outbound.send(HostToWorkerMessage::ChildStarted {
                                        call_id,
                                        child_id: run.id().to_owned(),
                                    });
                                    let outbound_result = outbound.clone();
                                    tokio::spawn(async move {
                                        let message = match run.result().await {
                                            Ok(result) => HostToWorkerMessage::ChildSettled {
                                                call_id,
                                                result,
                                            },
                                            Err(error) => HostToWorkerMessage::ChildFailed {
                                                call_id,
                                                rendered: safe_error(&error),
                                            },
                                        };
                                        let _ = outbound_result.send(message);
                                    });
                                }
                                Err(error) => {
                                    let _ = outbound.send(HostToWorkerMessage::ChildStartError {
                                        call_id,
                                        rendered: safe_error(&error),
                                    });
                                }
                            }
                        });
                    }
                    WorkerToHostMessage::ChildDispose { call_id } => {
                        let run = calls.lock().remove(&call_id);
                        let outbound = read_outbound.clone();
                        tokio::spawn(async move {
                            if let Some(run) = run {
                                let _ = run.dispose().await;
                            }
                            let _ = outbound.send(HostToWorkerMessage::ChildDisposed { call_id });
                        });
                    }
                    WorkerToHostMessage::Result { result } => {
                        if let Some(send) = read_result.lock().take() {
                            let _ = send.send(result);
                        }
                    }
                }
            }
            if let Some(send) = read_result.lock().take() {
                let _ = send.send(process_error(
                    "workflow worker exited before the run settled",
                    read_registry.started(),
                ));
            }
        });

        Ok(Arc::new(Self {
            outbound,
            result,
            kill,
            terminated,
            ready,
            launched,
        }))
    }

    pub(crate) fn launch(&self) {
        if self
            .launched
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        if self.ready.load(std::sync::atomic::Ordering::Acquire) {
            let _ = self.outbound.send(HostToWorkerMessage::Go);
        }
    }

    pub(crate) fn cancel(&self, reason: &str) {
        let _ = self.outbound.send(HostToWorkerMessage::Cancel {
            reason: reason.to_owned(),
        });
    }

    pub(crate) fn result(&self) -> BoxFuture<'static, WorkflowResult> {
        self.result.clone().boxed()
    }

    pub(crate) fn terminate(&self) {
        let _ = self.kill.send(());
    }

    pub(crate) fn wait_terminated(&self) -> BoxFuture<'static, ()> {
        self.terminated.clone().boxed()
    }
}
