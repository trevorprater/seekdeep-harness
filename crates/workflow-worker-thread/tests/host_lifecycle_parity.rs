//! Host ownership, cancellation, grace, and child-reaping parity.

use std::{collections::VecDeque, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::{FutureExt, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::{Context, EventOptions, EventReply, Fiber};
use seekdeep_core::session::{Session, SessionId};
use seekdeep_llm::AbortSignal;
use seekdeep_scope::ScopeKey;
use seekdeep_subagent::{
    ResolvedSubagentStartRequest, SubagentCapabilities, SubagentProvider, SubagentResult,
    SubagentRun, SubagentRuntime, SubagentStopReason,
};
use seekdeep_workflow::{
    WORKFLOW_ENGINE, WorkflowAgentEndInfo, WorkflowEngine, WorkflowEngineService, WorkflowMeta,
    WorkflowStartRequest, WorkflowStopReason,
};
use seekdeep_workflow_worker_thread::{Config, WorkerThreadWorkflowEngine};
use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};

type SharedResult<T> = futures::future::Shared<BoxFuture<'static, Result<T, String>>>;
type StubResultSender = oneshot::Sender<Result<SubagentResult, String>>;
type StubDisposeSender = oneshot::Sender<Result<(), String>>;
type PendingStubRun = (Arc<StubRun>, StubResultSender, StubDisposeSender);

struct StubRun {
    id: SessionId,
    result: SharedResult<SubagentResult>,
    disposal: SharedResult<()>,
    dispose_count: std::sync::atomic::AtomicUsize,
}

impl StubRun {
    fn new(
        id: &str,
        result: SharedResult<SubagentResult>,
        disposal: SharedResult<()>,
    ) -> Arc<Self> {
        Arc::new(Self {
            id: SessionId::new(id),
            result,
            disposal,
            dispose_count: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn immediate(id: &str, result: SubagentResult) -> Arc<Self> {
        Self::new(
            id,
            futures::future::ready(Ok(result)).boxed().shared(),
            futures::future::ready(Ok(())).boxed().shared(),
        )
    }

    fn panicking_result(id: &str) -> Arc<Self> {
        Self::new(
            id,
            async { panic!("provider result task panicked") }
                .boxed()
                .shared(),
            futures::future::ready(Ok(())).boxed().shared(),
        )
    }

    fn pending(id: &str) -> PendingStubRun {
        let (result_send, result_receive) = oneshot::channel();
        let (dispose_send, dispose_receive) = oneshot::channel();
        let result = async move {
            result_receive
                .await
                .unwrap_or_else(|_| Err("result sender dropped".to_owned()))
        }
        .boxed()
        .shared();
        let disposal = async move {
            dispose_receive
                .await
                .unwrap_or_else(|_| Err("dispose sender dropped".to_owned()))
        }
        .boxed()
        .shared();
        (Self::new(id, result, disposal), result_send, dispose_send)
    }
}

impl SubagentRun for StubRun {
    fn id(&self) -> &SessionId {
        &self.id
    }

    fn local_agent(&self) -> Option<&Arc<Agent>> {
        None
    }

    fn result(&self) -> BoxFuture<'static, anyhow::Result<SubagentResult>> {
        let result = self.result.clone();
        async move { result.await.map_err(|error| anyhow::anyhow!(error)) }.boxed()
    }

    fn dispose(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        self.dispose_count
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let disposal = self.disposal.clone();
        async move { disposal.await.map_err(|error| anyhow::anyhow!(error)) }.boxed()
    }
}

enum StartBehavior {
    Run(Arc<StubRun>),
    Pending(oneshot::Receiver<Result<Arc<StubRun>, String>>),
    Refuse(String),
}

struct Provider {
    behaviors: Mutex<VecDeque<StartBehavior>>,
    starts: Mutex<Vec<ResolvedSubagentStartRequest>>,
    changed: Notify,
    capabilities: SubagentCapabilities,
}

impl Provider {
    fn new(behaviors: impl IntoIterator<Item = StartBehavior>) -> Arc<Self> {
        Arc::new(Self {
            behaviors: Mutex::new(behaviors.into_iter().collect()),
            starts: Mutex::new(Vec::new()),
            changed: Notify::new(),
            capabilities: SubagentCapabilities {
                output_schema: true,
                depth_limit: true,
                tool_filter: true,
                persona: true,
            },
        })
    }

    async fn wait_for_starts(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let notified = self.changed.notified();
                if self.starts.lock().len() >= count {
                    return;
                }
                notified.await;
            }
        })
        .await
        .expect("provider start");
    }
}

#[async_trait]
impl SubagentProvider for Provider {
    fn name(&self) -> &'static str {
        "spawn"
    }

    fn capabilities(&self) -> &SubagentCapabilities {
        &self.capabilities
    }

    fn inherits_parent_context(&self) -> bool {
        false
    }

    async fn start(
        &self,
        request: ResolvedSubagentStartRequest,
    ) -> anyhow::Result<Arc<dyn SubagentRun>> {
        self.starts.lock().push(request);
        self.changed.notify_waiters();
        let behavior = self
            .behaviors
            .lock()
            .pop_front()
            .unwrap_or_else(|| StartBehavior::Refuse("no scripted run".to_owned()));
        match behavior {
            StartBehavior::Run(run) => Ok(run),
            StartBehavior::Refuse(error) => anyhow::bail!(error),
            StartBehavior::Pending(receiver) => receiver
                .await
                .map_err(|_| anyhow::anyhow!("pending start sender dropped"))?
                .map(|run| run as Arc<dyn SubagentRun>)
                .map_err(|error| anyhow::anyhow!(error)),
        }
    }
}

struct Harness {
    engine: Arc<WorkerThreadWorkflowEngine>,
    provider: Arc<Provider>,
    parent: Arc<Agent>,
    events: Arc<Mutex<Vec<String>>>,
    ends: Arc<Mutex<Vec<WorkflowAgentEndInfo>>>,
}

impl Harness {
    fn new(behaviors: impl IntoIterator<Item = StartBehavior>, grace_ms: u64) -> Self {
        let context = Context::new();
        let subagents = SubagentRuntime::install(&context).expect("subagents");
        let provider = Provider::new(behaviors);
        subagents
            .register_provider(provider.clone())
            .expect("provider");
        let engine = WorkerThreadWorkflowEngine::new(
            &context,
            Config {
                dispose_grace_ms: grace_ms,
                ..Config::default()
            },
        )
        .expect("engine");
        let events = Arc::new(Mutex::new(Vec::new()));
        for name in [
            "workflow/start",
            "workflow/phase",
            "workflow/log",
            "workflow/agent-start",
            "workflow/agent-end",
            "workflow/end",
        ] {
            let seen = events.clone();
            context
                .events()
                .on_sync(
                    &context,
                    name,
                    move |_, _| {
                        seen.lock().push(name.to_owned());
                        Ok(EventReply::Undefined)
                    },
                    EventOptions::default(),
                )
                .expect("event observer");
        }
        let ends = Arc::new(Mutex::new(Vec::new()));
        let seen_ends = ends.clone();
        context
            .events()
            .on_sync(
                &context,
                "workflow/agent-end",
                move |_, args| {
                    let end = args
                        .get::<WorkflowAgentEndInfo>(1)
                        .ok_or_else(|| anyhow::anyhow!("missing agent end"))?;
                    seen_ends.lock().push((*end).clone());
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .expect("end observer");
        Self {
            engine,
            provider,
            parent: parent(),
            events,
            ends,
        }
    }

    fn request(&self, script: &str, signal: Option<AbortSignal>) -> WorkflowStartRequest {
        WorkflowStartRequest {
            script: script.to_owned(),
            meta: WorkflowMeta {
                name: "host-lifecycle".to_owned(),
                description: "host lifecycle parity".to_owned(),
                when_to_use: None,
                phases: None,
            },
            args: None,
            subagent_provider: None,
            max_total_agents: None,
            parent: self.parent.clone(),
            signal,
        }
    }

    async fn wait_for_event(&self, name: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while !self.events.lock().iter().any(|event| event == name) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("workflow event");
    }
}

fn parent() -> Arc<Agent> {
    let id = SessionId::new("workflow-host-parent");
    let session = Session::create(&id, None, None).expect("session");
    let inbox =
        Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
    Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session,
        inbox,
        Context::new(),
        ScopeKey::new(),
    ))
}

fn completed_result() -> SubagentResult {
    SubagentResult {
        output: Vec::new(),
        structured: None,
        stop_reason: SubagentStopReason::Completed,
    }
}

#[tokio::test]
async fn completed_run_disposes_immediately_idempotently_without_arming_the_grace() {
    let harness = Harness::new([], 5_000);
    let signal = AbortSignal::default();
    let run = harness
        .engine
        .start(harness.request("return 6 * 7", Some(signal.clone())))
        .expect("start");
    let result = run.result().await;
    assert_eq!(result.stop_reason, WorkflowStopReason::Completed);
    assert_eq!(result.value, json!(42));
    signal.abort();
    assert_eq!(
        run.result().await.stop_reason,
        WorkflowStopReason::Completed
    );
    let started = std::time::Instant::now();
    run.dispose().await;
    run.dispose().await;
    assert!(started.elapsed() < Duration::from_millis(500));
    harness.wait_for_event("workflow/end").await;
    assert_eq!(
        harness.events.lock().first().map(String::as_str),
        Some("workflow/start")
    );
    assert_eq!(
        harness.events.lock().last().map(String::as_str),
        Some("workflow/end")
    );
}

#[tokio::test]
async fn already_aborted_input_signal_prevents_body_progress_and_reports_cancelled() {
    let harness = Harness::new([], 20);
    let signal = AbortSignal::default();
    signal.abort();
    let run = harness
        .engine
        .start(harness.request(
            "phase('must-not-run'); log('must-not-run'); return 1",
            Some(signal),
        ))
        .expect("start");
    let result = run.result().await;
    assert_eq!(result.stop_reason, WorkflowStopReason::Cancelled);
    assert!(
        result
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("start signal already aborted")
    );
    assert!(
        !harness
            .events
            .lock()
            .iter()
            .any(|event| event == "workflow/phase")
    );
    assert!(
        !harness
            .events
            .lock()
            .iter()
            .any(|event| event == "workflow/log")
    );
    run.dispose().await;
}

#[tokio::test]
async fn mid_run_external_signal_cancels_a_hookless_parked_promise() {
    let harness = Harness::new([], 25);
    let signal = AbortSignal::default();
    let run = harness
        .engine
        .start(harness.request(
            "await new Promise(() => {}); return 'unreachable'",
            Some(signal.clone()),
        ))
        .expect("start");
    tokio::task::yield_now().await;
    signal.abort();
    let result = tokio::time::timeout(Duration::from_secs(1), run.result())
        .await
        .expect("parked cancellation");
    assert_eq!(result.stop_reason, WorkflowStopReason::Cancelled);
    assert!(result.error.unwrap().contains("workflow signal aborted"));
    run.dispose().await;
}

#[tokio::test]
async fn grace_force_settles_pairs_stranded_agent_and_bounds_slow_child_disposal() {
    let (child, result_send, dispose_send) = StubRun::pending("stuck-child");
    let harness = Harness::new([StartBehavior::Run(child.clone())], 25);
    let run = harness
        .engine
        .start(harness.request("return await agent('hang')", None))
        .expect("start");
    harness.wait_for_event("workflow/agent-start").await;
    run.cancel(Some("user stopped"));
    let result = tokio::time::timeout(Duration::from_secs(1), run.result())
        .await
        .expect("forced result");
    assert_eq!(result.stop_reason, WorkflowStopReason::Cancelled);
    assert!(result.error.unwrap().contains("user stopped"));
    harness.wait_for_event("workflow/end").await;
    assert_eq!(harness.ends.lock().len(), 1);
    assert_eq!(
        harness.ends.lock()[0].outcome,
        seekdeep_workflow::WorkflowAgentOutcome::Cancelled
    );
    let events = harness.events.lock().clone();
    let agent_end = events
        .iter()
        .position(|event| event == "workflow/agent-end")
        .unwrap();
    let workflow_end = events
        .iter()
        .position(|event| event == "workflow/end")
        .unwrap();
    assert!(agent_end < workflow_end);

    let started = std::time::Instant::now();
    run.dispose().await;
    assert!(started.elapsed() < Duration::from_millis(250));
    assert_eq!(
        child
            .dispose_count
            .load(std::sync::atomic::Ordering::Acquire),
        1
    );
    assert!(result_send.send(Ok(completed_result())).is_ok());
    assert!(dispose_send.send(Ok(())).is_ok());
}

#[tokio::test]
async fn child_becoming_ready_after_cancel_is_refused_disposed_and_never_announced() {
    let (start_send, start_receive) = oneshot::channel();
    let harness = Harness::new([StartBehavior::Pending(start_receive)], 20);
    let run = harness
        .engine
        .start(harness.request("return await agent('late')", None))
        .expect("start");
    harness.provider.wait_for_starts(1).await;
    run.cancel(Some("cancel pending start"));
    let result = run.result().await;
    assert_eq!(result.stop_reason, WorkflowStopReason::Cancelled);
    let child = StubRun::immediate("late-child", completed_result());
    assert!(start_send.send(Ok(child.clone())).is_ok());
    tokio::time::timeout(Duration::from_secs(2), async {
        while child
            .dispose_count
            .load(std::sync::atomic::Ordering::Acquire)
            == 0
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("late child disposed");
    assert_eq!(
        child
            .dispose_count
            .load(std::sync::atomic::Ordering::Acquire),
        1
    );
    assert!(
        !harness
            .events
            .lock()
            .iter()
            .any(|event| event == "workflow/agent-start")
    );
    run.dispose().await;
}

#[tokio::test]
async fn child_dispose_failure_is_contained_and_cannot_wedge_success() {
    let child = StubRun::new(
        "bad-dispose",
        futures::future::ready(Ok(completed_result()))
            .boxed()
            .shared(),
        futures::future::ready(Err("dispose exploded".to_owned()))
            .boxed()
            .shared(),
    );
    let harness = Harness::new([StartBehavior::Run(child.clone())], 20);
    let run = harness
        .engine
        .start(harness.request("await agent('done'); return 'ok'", None))
        .expect("start");
    let result = run.result().await;
    assert_eq!(result.stop_reason, WorkflowStopReason::Completed);
    assert_eq!(result.value, Value::String("ok".to_owned()));
    run.dispose().await;
    assert_eq!(
        child
            .dispose_count
            .load(std::sync::atomic::Ordering::Acquire),
        1
    );
}

#[tokio::test]
async fn run_ids_are_unique_and_a_holder_owned_run_survives_engine_hmr_unload() {
    let context = Context::new();
    let subagents = SubagentRuntime::install(&context).expect("subagents");
    let (start_send, start_receive) = oneshot::channel();
    let provider = Provider::new([StartBehavior::Pending(start_receive)]);
    subagents
        .register_provider(provider.clone())
        .expect("provider");
    let fiber = Fiber::active_child("workflow-engine-owner");
    let child_context = context.with_fiber(fiber.clone());
    let engine = WorkerThreadWorkflowEngine::new(&child_context, Config::default()).unwrap();
    WorkflowEngineService::new(engine)
        .provide(&child_context)
        .unwrap();
    let service = context.get(WORKFLOW_ENGINE).expect("workflow service");
    let owner = parent();
    let request = |script: &str| WorkflowStartRequest {
        script: script.to_owned(),
        meta: WorkflowMeta {
            name: "hmr-holder".to_owned(),
            description: "holder-owned run".to_owned(),
            when_to_use: None,
            phases: None,
        },
        args: None,
        subagent_provider: None,
        max_total_agents: None,
        parent: owner.clone(),
        signal: None,
    };
    let run = service
        .start(request("return await agent('after unload')"))
        .unwrap();
    provider.wait_for_starts(1).await;
    fiber.dispose().await.unwrap();
    assert!(context.get(WORKFLOW_ENGINE).is_none());
    let started = StubRun::immediate("hmr-child", completed_result());
    assert!(start_send.send(Ok(started)).is_ok());
    let result = run.result().await;
    assert_eq!(result.stop_reason, WorkflowStopReason::Completed);
    run.dispose().await;

    let replacement = WorkerThreadWorkflowEngine::new(&context, Config::default()).unwrap();
    let first = replacement.start(request("return 1")).unwrap();
    let second = replacement.start(request("return 2")).unwrap();
    assert_ne!(first.id(), second.id());
    assert_eq!(first.meta().name, "hmr-holder");
    assert_eq!(first.result().await.value, json!(1));
    assert_eq!(second.result().await.value, json!(2));
    first.dispose().await;
    second.dispose().await;
}

#[tokio::test]
async fn worker_thread_death_reaps_children_and_pairs_stranded_lifecycle_before_error_end() {
    let child = StubRun::panicking_result("panicking-child");
    let harness = Harness::new([StartBehavior::Run(child.clone())], 20);
    let run = harness
        .engine
        .start(harness.request("return await agent('panic')", None))
        .expect("start");
    let result = tokio::time::timeout(Duration::from_secs(2), run.result())
        .await
        .expect("worker death result");
    assert_eq!(result.stop_reason, WorkflowStopReason::Error);
    assert_eq!(result.agents_started, 1);
    assert!(
        result
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("worker exited before completing")
    );
    harness.wait_for_event("workflow/end").await;
    let events = harness.events.lock().clone();
    assert!(
        events
            .iter()
            .position(|event| event == "workflow/agent-end")
            .unwrap()
            < events
                .iter()
                .position(|event| event == "workflow/end")
                .unwrap()
    );
    assert_eq!(harness.ends.lock().len(), 1);
    assert_eq!(
        harness.ends.lock()[0].outcome,
        seekdeep_workflow::WorkflowAgentOutcome::Cancelled
    );
    run.dispose().await;
    assert_eq!(
        child
            .dispose_count
            .load(std::sync::atomic::Ordering::Acquire),
        1
    );
}
