//! Host ownership for one workflow run, its child registry, cancellation,
//! bounded disposal, and exactly-once lifecycle settlement.

use std::{
    collections::{BTreeMap, HashMap},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::{FutureExt as _, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions};
use seekdeep_cordis::{Context, EventArgs};
use seekdeep_llm::{AbortSignal, ContentBlock, ModelId, ProviderId};
use seekdeep_subagent::{SubagentRun, SubagentRuntime, SubagentStartRequest};
use seekdeep_workflow::{
    WorkflowAgentEndInfo, WorkflowAgentInfo, WorkflowAgentOutcome, WorkflowEventName, WorkflowMeta,
    WorkflowResult, WorkflowRun, WorkflowRunId, WorkflowRunInfo, WorkflowStopReason,
    emit_workflow_event,
};
use tokio::{
    sync::{Notify, OnceCell, oneshot},
    task::JoinHandle,
};

use crate::{
    process::{ProcessExecution, WorkerCommand},
    runtime::{ExecutionObserver, WorkflowExecution},
    types::{ChildHandle, ChildPort, ChildResult, ChildStartRequest, WorkerInit, WorkerLimits},
};

fn render_anyhow(error: &anyhow::Error) -> String {
    catch_unwind(AssertUnwindSafe(|| format!("{error:#}")))
        .unwrap_or_else(|_| "[unrenderable workflow child error]".to_owned())
}

#[derive(Default)]
struct RunState {
    cancel_reason: Option<String>,
    terminal_claimed: bool,
}

struct ResultState {
    value: Mutex<Option<WorkflowResult>>,
    notify: Notify,
}

impl ResultState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            value: Mutex::new(None),
            notify: Notify::new(),
        })
    }

    fn get(&self) -> Option<WorkflowResult> {
        self.value.lock().clone()
    }

    fn settle(&self, result: WorkflowResult) -> bool {
        let mut slot = self.value.lock();
        if slot.is_some() {
            return false;
        }
        *slot = Some(result);
        drop(slot);
        self.notify.notify_waiters();
        true
    }

    async fn wait(&self) -> WorkflowResult {
        loop {
            let notified = self.notify.notified();
            if let Some(result) = self.get() {
                return result;
            }
            notified.await;
        }
    }
}

pub(crate) struct RunObserver {
    context: Context,
    info: WorkflowRunInfo,
    state: Arc<Mutex<RunState>>,
    live_agents: Mutex<BTreeMap<u64, WorkflowAgentInfo>>,
}

impl RunObserver {
    fn emit(&self, event: WorkflowEventName, args: &EventArgs) {
        let _ = emit_workflow_event(&self.context, event, args);
    }

    fn end_stranded(&self) {
        let stranded = std::mem::take(&mut *self.live_agents.lock());
        for info in stranded.into_values() {
            self.emit(
                WorkflowEventName::AgentEnd,
                &EventArgs::from_values(vec![
                    Arc::new(self.info.clone()),
                    Arc::new(WorkflowAgentEndInfo {
                        info,
                        outcome: WorkflowAgentOutcome::Cancelled,
                    }),
                ]),
            );
        }
    }
}

impl ExecutionObserver for RunObserver {
    fn phase(&self, title: &str) {
        if self.state.lock().cancel_reason.is_some() {
            return;
        }
        self.emit(
            WorkflowEventName::Phase,
            &EventArgs::from_values(vec![
                Arc::new(self.info.clone()),
                Arc::new(title.to_owned()),
            ]),
        );
    }

    fn log(&self, message: &str) {
        if self.state.lock().cancel_reason.is_some() {
            return;
        }
        self.emit(
            WorkflowEventName::Log,
            &EventArgs::from_values(vec![
                Arc::new(self.info.clone()),
                Arc::new(message.to_owned()),
            ]),
        );
    }

    fn agent_start(&self, info: &WorkflowAgentInfo) {
        self.live_agents.lock().insert(info.seq, info.clone());
        self.emit(
            WorkflowEventName::AgentStart,
            &EventArgs::from_values(vec![Arc::new(self.info.clone()), Arc::new(info.clone())]),
        );
    }

    fn agent_end(&self, info: &WorkflowAgentEndInfo) {
        if self.live_agents.lock().remove(&info.info.seq).is_none() {
            return;
        }
        self.emit(
            WorkflowEventName::AgentEnd,
            &EventArgs::from_values(vec![Arc::new(self.info.clone()), Arc::new(info.clone())]),
        );
    }
}

pub(crate) struct ChildRegistry {
    runtime: tokio::runtime::Handle,
    subagents: Arc<SubagentRuntime>,
    parent: Arc<Agent>,
    provider: String,
    signal: AbortSignal,
    state: Arc<Mutex<RunState>>,
    next_key: AtomicU64,
    started: AtomicU64,
    pending: AtomicUsize,
    children: Mutex<HashMap<u64, Arc<HostChildHandle>>>,
    changed: Notify,
}

impl ChildRegistry {
    fn admission_failure(&self) -> Option<String> {
        let state = self.state.lock();
        if let Some(reason) = &state.cancel_reason {
            return Some(format!("workflow run cancelled: {reason}"));
        }
        state
            .terminal_claimed
            .then(|| "workflow run already settled".to_owned())
    }

    pub(crate) async fn start(
        self: &Arc<Self>,
        request: ChildStartRequest,
    ) -> anyhow::Result<Arc<dyn ChildHandle>> {
        {
            let state = self.state.lock();
            if let Some(reason) = &state.cancel_reason {
                anyhow::bail!("workflow run cancelled: {reason}");
            }
            anyhow::ensure!(!state.terminal_claimed, "workflow run already settled");
            self.started.fetch_add(1, Ordering::AcqRel);
            self.pending.fetch_add(1, Ordering::AcqRel);
        }
        let pending = PendingStart::new(self);
        let agent_options = if request.provider.is_some() || request.model.is_some() {
            Some(AgentOptions {
                provider: request.provider.map(ProviderId::new),
                model: request.model.map(ModelId::new),
                max_tokens: None,
                subagent_depth: None,
            })
        } else {
            None
        };
        let run = self
            .subagents
            .start(
                &self.provider,
                SubagentStartRequest {
                    label: None,
                    prompt: vec![ContentBlock::Text {
                        text: request.prompt,
                    }],
                    parent: self.parent.clone(),
                    signal: self.signal.clone(),
                    agent_options,
                    output_schema: request.schema,
                    max_depth: None,
                    tool_filter: None,
                    persona: None,
                },
            )
            .await?;
        drop(pending);

        if let Some(failure) = self.admission_failure() {
            if let Err(error) = run.dispose().await {
                let error = render_anyhow(&error);
                tracing::warn!(%error, "refused workflow child disposal failed");
            }
            anyhow::bail!(failure);
        }

        let key = self.next_key.fetch_add(1, Ordering::AcqRel) + 1;
        let handle = Arc::new(HostChildHandle {
            run,
            key,
            registry: Arc::downgrade(self),
            disposal: Mutex::new(None),
        });
        let failure = {
            let state = self.state.lock();
            if let Some(reason) = state.cancel_reason.clone() {
                Some(format!("workflow run cancelled: {reason}"))
            } else if state.terminal_claimed {
                Some("workflow run already settled".to_owned())
            } else {
                self.children.lock().insert(key, handle.clone());
                None
            }
        };
        if let Some(failure) = failure {
            let _ = handle.dispose_once().await;
            anyhow::bail!(failure);
        }
        Ok(handle)
    }

    fn finish(&self, key: u64) {
        self.children.lock().remove(&key);
        self.changed.notify_waiters();
    }

    pub(crate) fn started(&self) -> u64 {
        self.started.load(Ordering::Acquire)
    }

    fn reap(&self, reason: &str) {
        if !self.signal.is_aborted() {
            self.signal
                .abort_with_reason(serde_json::Value::String(reason.to_owned()));
        }
        for child in self.children.lock().values().cloned().collect::<Vec<_>>() {
            drop(child.dispose_once());
        }
    }

    async fn quiescence(&self) {
        loop {
            let notified = self.changed.notified();
            if self.pending.load(Ordering::Acquire) == 0 && self.children.lock().is_empty() {
                return;
            }
            notified.await;
        }
    }
}

struct PendingStart {
    registry: Weak<ChildRegistry>,
}

impl PendingStart {
    fn new(registry: &Arc<ChildRegistry>) -> Self {
        Self {
            registry: Arc::downgrade(registry),
        }
    }
}

impl Drop for PendingStart {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.pending.fetch_sub(1, Ordering::AcqRel);
            registry.changed.notify_waiters();
        }
    }
}

struct HostChildHandle {
    run: Arc<dyn SubagentRun>,
    key: u64,
    registry: Weak<ChildRegistry>,
    disposal: Mutex<Option<SharedDisposal>>,
}

type SharedDisposal = futures::future::Shared<BoxFuture<'static, Result<(), String>>>;

impl HostChildHandle {
    fn dispose_once(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        let disposal = {
            let mut slot = self.disposal.lock();
            slot.get_or_insert_with(|| {
                let run = Arc::clone(&self.run);
                let key = self.key;
                let registry = self.registry.clone();
                let runtime = registry
                    .upgrade()
                    .map_or_else(tokio::runtime::Handle::current, |registry| {
                        registry.runtime.clone()
                    });
                let (send, receive) = oneshot::channel();
                runtime.spawn(async move {
                    let result = AssertUnwindSafe(run.dispose())
                        .catch_unwind()
                        .await
                        .map_or_else(
                            |_| Err("[panicked workflow child disposal]".to_owned()),
                            |result| result.map_err(|error| render_anyhow(&error)),
                        );
                    if let Some(registry) = registry.upgrade() {
                        registry.finish(key);
                    }
                    let _ = send.send(result);
                });
                async move {
                    receive
                        .await
                        .unwrap_or_else(|_| Err("workflow child disposal task stopped".to_owned()))
                }
                .boxed()
                .shared()
            })
            .clone()
        };
        Box::pin(async move { disposal.await.map_err(|error| anyhow::anyhow!(error)) })
    }
}

impl Drop for HostChildHandle {
    fn drop(&mut self) {
        drop(self.dispose_once());
    }
}

impl ChildHandle for HostChildHandle {
    fn id(&self) -> &str {
        self.run.id().as_str()
    }

    fn result(&self) -> BoxFuture<'static, anyhow::Result<ChildResult>> {
        let run = Arc::clone(&self.run);
        Box::pin(async move {
            let result = run.result().await?;
            Ok(ChildResult {
                output: result.output,
                structured: result.structured,
                stop_reason: result.stop_reason.as_str().to_owned(),
            })
        })
    }

    fn dispose(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        self.dispose_once()
    }
}

struct HostChildPort(Arc<ChildRegistry>);

impl ChildPort for HostChildPort {
    fn start_agent(
        &self,
        request: ChildStartRequest,
    ) -> BoxFuture<'static, anyhow::Result<Arc<dyn ChildHandle>>> {
        let registry = self.0.clone();
        Box::pin(async move { registry.start(request).await })
    }
}

enum ExecutionBackend {
    Thread(Arc<WorkflowExecution>),
    Process(Arc<ProcessExecution>),
}

impl ExecutionBackend {
    fn launch(&self) {
        if let Self::Process(process) = self {
            process.launch();
        }
    }

    fn cancel(&self, reason: &str) {
        match self {
            Self::Thread(execution) => execution.cancel(reason),
            Self::Process(process) => process.cancel(reason),
        }
    }

    fn result(&self) -> BoxFuture<'static, WorkflowResult> {
        match self {
            Self::Thread(execution) => {
                let execution = execution.clone();
                Box::pin(async move { execution.drive().await })
            }
            Self::Process(process) => process.result(),
        }
    }

    fn terminate(&self) {
        match self {
            Self::Thread(execution) => execution.cancel("workflow worker terminated"),
            Self::Process(process) => process.terminate(),
        }
    }

    fn wait_terminated(&self) -> BoxFuture<'static, ()> {
        match self {
            Self::Thread(_) => Box::pin(async {}),
            Self::Process(process) => process.wait_terminated(),
        }
    }
}

/// One live worker-engine run.
pub struct WorkerRun {
    self_weak: Weak<Self>,
    id: WorkflowRunId,
    meta: WorkflowMeta,
    execution: Arc<ExecutionBackend>,
    registry: Arc<ChildRegistry>,
    observer: Arc<RunObserver>,
    state: Arc<Mutex<RunState>>,
    result: Arc<ResultState>,
    dispose_grace: Duration,
    launched: AtomicBool,
    input_signal_task: Mutex<Option<JoinHandle<()>>>,
    disposal: Arc<OnceCell<()>>,
}

impl WorkerRun {
    /// Constructs one unpublished run. Call [`launch`](Self::launch) only
    /// after the paired `workflow/start` event is visible.
    ///
    /// # Errors
    ///
    /// Returns execution-context or script construction failures.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        context: &Context,
        subagents: Arc<SubagentRuntime>,
        id: WorkflowRunId,
        meta: WorkflowMeta,
        parent: Arc<Agent>,
        body: String,
        args: Option<serde_json::Value>,
        limits: WorkerLimits,
        provider: String,
        info: WorkflowRunInfo,
        dispose_grace_ms: u64,
        worker: Option<&WorkerCommand>,
    ) -> anyhow::Result<Arc<Self>> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|error| {
            anyhow::anyhow!("workflow engine requires an async runtime: {error}")
        })?;
        let state = Arc::new(Mutex::new(RunState::default()));
        let observer = Arc::new(RunObserver {
            context: context.clone(),
            info,
            state: state.clone(),
            live_agents: Mutex::new(BTreeMap::new()),
        });
        let registry = Arc::new(ChildRegistry {
            runtime,
            subagents,
            parent,
            provider,
            signal: AbortSignal::default(),
            state: state.clone(),
            next_key: AtomicU64::new(0),
            started: AtomicU64::new(0),
            pending: AtomicUsize::new(0),
            children: Mutex::new(HashMap::new()),
            changed: Notify::new(),
        });
        let init = WorkerInit {
            meta: meta.clone(),
            body: body.clone(),
            args: args.clone(),
            limits: limits.clone(),
        };
        let execution = match worker {
            Some(worker) => ExecutionBackend::Process(ProcessExecution::new(
                worker,
                init,
                registry.clone(),
                observer.clone(),
            )?),
            None => ExecutionBackend::Thread(Arc::new(WorkflowExecution::new(
                &meta,
                body,
                args,
                limits,
                observer.clone(),
                Arc::new(HostChildPort(registry.clone())),
            )?)),
        };
        let execution = Arc::new(execution);
        let result = ResultState::new();
        Ok(Arc::new_cyclic(|self_weak| Self {
            self_weak: self_weak.clone(),
            id,
            meta,
            execution,
            registry,
            observer,
            state,
            result,
            dispose_grace: Duration::from_millis(dispose_grace_ms),
            launched: AtomicBool::new(false),
            input_signal_task: Mutex::new(None),
            disposal: Arc::new(OnceCell::new()),
        }))
    }

    /// Starts execution and attaches the caller's cancellation signal.
    pub fn launch(self: &Arc<Self>, input_signal: Option<AbortSignal>) {
        if self.launched.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(signal) = input_signal {
            if signal.is_aborted() {
                self.cancel_with_reason("workflow start signal already aborted");
            } else {
                let weak = self.self_weak.clone();
                *self.input_signal_task.lock() = Some(tokio::spawn(async move {
                    signal.cancelled().await;
                    if let Some(run) = weak.upgrade() {
                        run.cancel_with_reason("workflow signal aborted");
                    }
                }));
            }
        }
        self.execution.launch();
        let execution = self.execution.clone();
        let weak = self.self_weak.clone();
        tokio::spawn(async move {
            let outcome = execution.result().await;
            if let Some(run) = weak.upgrade() {
                run.observer.end_stranded();
                run.accept_result(outcome);
            }
        });
    }

    fn cancelled_result(reason: &str, agents_started: u64) -> WorkflowResult {
        WorkflowResult {
            value: serde_json::Value::Null,
            stop_reason: WorkflowStopReason::Cancelled,
            error: Some(format!("workflow run cancelled: {reason}")),
            agents_started,
        }
    }

    fn accept_result(&self, outcome: WorkflowResult) {
        let cancellation = {
            let mut state = self.state.lock();
            if state.terminal_claimed {
                return;
            }
            state.terminal_claimed = true;
            state.cancel_reason.clone()
        };
        self.registry.reap("workflow settled");
        let outcome = match cancellation {
            Some(reason) if outcome.stop_reason != WorkflowStopReason::Cancelled => {
                Self::cancelled_result(&reason, outcome.agents_started)
            }
            Some(_) | None => outcome,
        };
        self.finish_settle(outcome);
    }

    fn force_cancel(&self) {
        let reason = {
            let mut state = self.state.lock();
            if state.terminal_claimed {
                return;
            }
            state.terminal_claimed = true;
            state
                .cancel_reason
                .clone()
                .unwrap_or_else(|| "workflow cancelled".to_owned())
        };
        self.registry.reap(&reason);
        self.observer.end_stranded();
        self.execution.terminate();
        self.finish_settle(Self::cancelled_result(
            &reason,
            self.registry.started.load(Ordering::Acquire),
        ));
    }

    fn finish_settle(&self, outcome: WorkflowResult) {
        if self.result.settle(outcome)
            && let Some(task) = self.input_signal_task.lock().take()
        {
            task.abort();
        }
    }

    fn cancel_with_reason(&self, reason: &str) {
        {
            let mut state = self.state.lock();
            if state.terminal_claimed || state.cancel_reason.is_some() {
                return;
            }
            state.cancel_reason = Some(reason.to_owned());
        }
        self.execution.cancel(reason);
        self.registry.reap(reason);
        let weak = self.self_weak.clone();
        let grace = self.dispose_grace;
        tokio::spawn(async move {
            tokio::time::sleep(grace).await;
            if let Some(run) = weak.upgrade() {
                run.force_cancel();
            }
        });
    }
}

impl WorkflowRun for WorkerRun {
    fn id(&self) -> &WorkflowRunId {
        &self.id
    }

    fn meta(&self) -> &WorkflowMeta {
        &self.meta
    }

    fn result(&self) -> BoxFuture<'static, WorkflowResult> {
        let result = self.result.clone();
        Box::pin(async move { result.wait().await })
    }

    fn cancel(&self, reason: Option<&str>) {
        self.cancel_with_reason(reason.unwrap_or("workflow cancelled"));
    }

    fn dispose(&self) -> BoxFuture<'static, ()> {
        let run = self.self_weak.upgrade();
        let disposal = self.disposal.clone();
        Box::pin(async move {
            disposal
                .get_or_init(|| async move {
                    let Some(run) = run else {
                        return;
                    };
                    run.cancel_with_reason("workflow disposed");
                    run.registry.reap("workflow disposed");
                    let settled = async {
                        let _ = run.result.wait().await;
                        run.registry.quiescence().await;
                    };
                    if tokio::time::timeout(run.dispose_grace, settled)
                        .await
                        .is_err()
                    {
                        run.force_cancel();
                    }
                    run.execution.terminate();
                    let _ =
                        tokio::time::timeout(run.dispose_grace, run.execution.wait_terminated())
                            .await;
                    run.registry.reap("workflow disposed");
                    run.observer.end_stranded();
                })
                .await;
        })
    }
}
