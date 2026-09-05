//! Killable worker-process session over newline-delimited JSON stdio.

use std::{collections::HashMap, sync::Arc};

use futures::{FutureExt as _, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_workflow::{WorkflowResult, WorkflowStopReason};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader},
    sync::{mpsc, oneshot},
};

use crate::{
    protocol::{HostToWorkerMessage, WorkerToHostMessage},
    runtime::{ExecutionObserver, WorkflowExecution},
    types::{ChildHandle, ChildPort, ChildResult, ChildStartRequest, WorkerInit},
};

type Started = Result<String, String>;
type Settled = Result<ChildResult, String>;

struct PendingChild {
    started: Mutex<Option<oneshot::Sender<Started>>>,
    settled: Mutex<Option<oneshot::Sender<Settled>>>,
    disposed: Mutex<Option<oneshot::Sender<()>>>,
}

struct RpcChildHandle {
    call_id: u64,
    id: String,
    outbound: mpsc::UnboundedSender<WorkerToHostMessage>,
    result: futures::future::Shared<BoxFuture<'static, Settled>>,
    disposal: futures::future::Shared<BoxFuture<'static, ()>>,
    dispose_requested: std::sync::atomic::AtomicBool,
}

impl RpcChildHandle {
    fn request_dispose(&self) {
        if self
            .dispose_requested
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let _ = self.outbound.send(WorkerToHostMessage::ChildDispose {
            call_id: self.call_id,
        });
    }
}

impl Drop for RpcChildHandle {
    fn drop(&mut self) {
        self.request_dispose();
    }
}

impl ChildHandle for RpcChildHandle {
    fn id(&self) -> &str {
        &self.id
    }

    fn result(&self) -> BoxFuture<'static, anyhow::Result<ChildResult>> {
        let result = self.result.clone();
        async move { result.await.map_err(|error| anyhow::anyhow!(error)) }.boxed()
    }

    fn dispose(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        self.request_dispose();
        let disposal = self.disposal.clone();
        async move {
            disposal.await;
            Ok(())
        }
        .boxed()
    }
}

struct ChildRpcBridge {
    outbound: mpsc::UnboundedSender<WorkerToHostMessage>,
    next_call_id: std::sync::atomic::AtomicU64,
    pending: Arc<Mutex<HashMap<u64, Arc<PendingChild>>>>,
}

impl ChildPort for ChildRpcBridge {
    fn start_agent(
        &self,
        request: ChildStartRequest,
    ) -> BoxFuture<'static, anyhow::Result<Arc<dyn ChildHandle>>> {
        let call_id = self
            .next_call_id
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
            + 1;
        let (started_send, started_receive) = oneshot::channel();
        let (settled_send, settled_receive) = oneshot::channel();
        let (disposed_send, disposed_receive) = oneshot::channel();
        let entry = Arc::new(PendingChild {
            started: Mutex::new(Some(started_send)),
            settled: Mutex::new(Some(settled_send)),
            disposed: Mutex::new(Some(disposed_send)),
        });
        self.pending.lock().insert(call_id, entry);
        let outbound = self.outbound.clone();
        let pending = self.pending.clone();
        let send = outbound.send(WorkerToHostMessage::ChildStart { call_id, request });
        Box::pin(async move {
            if send.is_err() {
                pending.lock().remove(&call_id);
                anyhow::bail!("workflow host is unavailable");
            }
            let id = started_receive
                .await
                .map_err(|_| anyhow::anyhow!("workflow host dropped child start"))?
                .map_err(|error| anyhow::anyhow!(error))?;
            let result = async move {
                settled_receive
                    .await
                    .unwrap_or_else(|_| Err("workflow host dropped child result".to_owned()))
            }
            .boxed()
            .shared();
            let disposal = async move {
                let _ = disposed_receive.await;
            }
            .boxed()
            .shared();
            Ok(Arc::new(RpcChildHandle {
                call_id,
                id,
                outbound,
                result,
                disposal,
                dispose_requested: std::sync::atomic::AtomicBool::new(false),
            }) as Arc<dyn ChildHandle>)
        })
    }
}

struct WireObserver {
    outbound: mpsc::UnboundedSender<WorkerToHostMessage>,
}

impl ExecutionObserver for WireObserver {
    fn phase(&self, title: &str) {
        let _ = self.outbound.send(WorkerToHostMessage::Phase {
            title: title.to_owned(),
        });
    }

    fn log(&self, message: &str) {
        let _ = self.outbound.send(WorkerToHostMessage::Log {
            message: message.to_owned(),
        });
    }

    fn agent_start(&self, info: &seekdeep_workflow::WorkflowAgentInfo) {
        let _ = self
            .outbound
            .send(WorkerToHostMessage::AgentStart { info: info.clone() });
    }

    fn agent_end(&self, info: &seekdeep_workflow::WorkflowAgentEndInfo) {
        let _ = self
            .outbound
            .send(WorkerToHostMessage::AgentEnd { info: info.clone() });
    }
}

fn error_result(error: &anyhow::Error) -> WorkflowResult {
    WorkflowResult {
        value: serde_json::Value::Null,
        stop_reason: WorkflowStopReason::Error,
        error: Some(format!("{error:#}")),
        agents_started: 0,
    }
}

/// Runs one worker session over typed in-memory channels.
pub async fn run_worker_session(
    init: WorkerInit,
    outbound: mpsc::UnboundedSender<WorkerToHostMessage>,
    mut inbound: mpsc::UnboundedReceiver<HostToWorkerMessage>,
) {
    let pending = Arc::new(Mutex::new(HashMap::<u64, Arc<PendingChild>>::new()));
    let children: Arc<dyn ChildPort> = Arc::new(ChildRpcBridge {
        outbound: outbound.clone(),
        next_call_id: std::sync::atomic::AtomicU64::new(0),
        pending: pending.clone(),
    });
    let observer: Arc<dyn ExecutionObserver> = Arc::new(WireObserver {
        outbound: outbound.clone(),
    });
    let execution = match WorkflowExecution::new(
        &init.meta,
        init.body,
        init.args,
        init.limits,
        observer,
        children,
    ) {
        Ok(execution) => Arc::new(execution),
        Err(error) => {
            let _ = outbound.send(WorkerToHostMessage::Result {
                result: error_result(&error),
            });
            return;
        }
    };
    let (gate_send, gate_receive) = oneshot::channel();
    let gate = Arc::new(Mutex::new(Some(gate_send)));
    let reader_execution = execution.clone();
    let reader_gate = gate.clone();
    let reader_pending = pending;
    let reader = tokio::spawn(async move {
        while let Some(message) = inbound.recv().await {
            match message {
                HostToWorkerMessage::Go => {
                    if let Some(gate) = reader_gate.lock().take() {
                        let _ = gate.send(());
                    }
                }
                HostToWorkerMessage::Cancel { reason } => {
                    reader_execution.cancel(&reason);
                    if let Some(gate) = reader_gate.lock().take() {
                        let _ = gate.send(());
                    }
                }
                HostToWorkerMessage::ChildStarted { call_id, child_id } => {
                    if let Some(entry) = reader_pending.lock().get(&call_id)
                        && let Some(started) = entry.started.lock().take()
                    {
                        let _ = started.send(Ok(child_id));
                    }
                }
                HostToWorkerMessage::ChildStartError { call_id, rendered } => {
                    if let Some(entry) = reader_pending.lock().remove(&call_id)
                        && let Some(started) = entry.started.lock().take()
                    {
                        let _ = started.send(Err(rendered));
                    }
                }
                HostToWorkerMessage::ChildSettled { call_id, result } => {
                    if let Some(entry) = reader_pending.lock().get(&call_id)
                        && let Some(settled) = entry.settled.lock().take()
                    {
                        let _ = settled.send(Ok(result));
                    }
                }
                HostToWorkerMessage::ChildFailed { call_id, rendered } => {
                    if let Some(entry) = reader_pending.lock().get(&call_id)
                        && let Some(settled) = entry.settled.lock().take()
                    {
                        let _ = settled.send(Err(rendered));
                    }
                }
                HostToWorkerMessage::ChildDisposed { call_id } => {
                    if let Some(entry) = reader_pending.lock().remove(&call_id)
                        && let Some(disposed) = entry.disposed.lock().take()
                    {
                        let _ = disposed.send(());
                    }
                }
            }
        }
        reader_execution.cancel("workflow host disconnected");
        if let Some(gate) = reader_gate.lock().take() {
            let _ = gate.send(());
        }
    });

    let _ = outbound.send(WorkerToHostMessage::Ready);
    let _ = gate_receive.await;
    let result = execution.drive().await;
    let _ = outbound.send(WorkerToHostMessage::Result { result });
    reader.abort();
}

/// Runs the stdio worker entry until one terminal result is flushed.
///
/// # Errors
///
/// Returns init decoding, stdio, serialization, or writer-task failures.
pub async fn run_stdio_worker() -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let init = lines
        .next_line()
        .await?
        .ok_or_else(|| anyhow::anyhow!("workflow worker did not receive its init payload"))?;
    let init: WorkerInit = serde_json::from_str(&init)?;
    let (host_send, host_receive) = mpsc::unbounded_channel();
    let reader = tokio::spawn(async move {
        while let Ok(Some(line)) = lines.next_line().await {
            match serde_json::from_str::<HostToWorkerMessage>(&line) {
                Ok(message) => {
                    if host_send.send(message).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = host_send.send(HostToWorkerMessage::Cancel {
                        reason: format!("workflow host sent an invalid message: {error}"),
                    });
                    break;
                }
            }
        }
    });
    let (worker_send, mut worker_receive) = mpsc::unbounded_channel();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(message) = worker_receive.recv().await {
            let line = serde_json::to_vec(&message)?;
            stdout.write_all(&line).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
        Ok::<(), anyhow::Error>(())
    });
    run_worker_session(init, worker_send, host_receive).await;
    reader.abort();
    writer.await??;
    Ok(())
}
