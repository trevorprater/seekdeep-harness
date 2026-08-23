//! Non-consuming background-job baselines and whole-set mux changes.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_client_connection::{HttpResponse, RpcResult};
use seekdeep_cordis::{Context, fiber::EffectHandle};
use seekdeep_core::{
    session::SessionId,
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_host_apiproxy::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, RpcId, RpcMethod, RpcReceipt,
    RpcReceiptReason, RpcRequest, RpcResponse, SessionApiProxyOptions, SessionApiProxyRuntime,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
    },
};
use seekdeep_jobs::{JobHooks, JobOutcome, JobRegistry, JobStart, JobTerminalStatus};
use seekdeep_jobs_local::{Config as JobsConfig, LocalJobRegistry};
use seekdeep_llm::AbortSignal;
use seekdeep_scope::{ScopeKey, create_scope};
use serde_json::{Value, json};
use tokio::sync::oneshot;

#[derive(Debug)]
struct TerminalDomains;

impl ApiProxyRuntime for TerminalDomains {
    fn unary(
        &self,
        _method: RpcMethod,
        request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcResponse<Value>>> {
        async move {
            Ok(RpcResponse::new(
                request.rpc_id,
                RpcResult::Success { value: None },
            ))
        }
        .boxed()
    }

    fn respond(
        &self,
        _message: ClientResponse,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcReceipt>> {
        async {
            Ok(RpcReceipt::Rejected {
                reason: RpcReceiptReason::NotPending,
            })
        }
        .boxed()
    }

    fn mux(
        &self,
        _request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> ApiDownlinkStream<MuxFrame> {
        futures::stream::empty().boxed()
    }

    fn host(
        &self,
        _request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> ApiDownlinkStream<HostFrame> {
        futures::stream::empty().boxed()
    }

    fn session_log(
        &self,
        _query: SessionLogQuery,
        _signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<HttpResponse>> {
        async { Ok(HttpResponse::text(501, "not used")) }.boxed()
    }
}

struct ControlledHooks {
    receiver: Mutex<Option<oneshot::Receiver<JobOutcome>>>,
    reads: Arc<AtomicUsize>,
}

impl JobHooks for ControlledHooks {
    fn cancel(&self, _reason: Option<&str>) {}

    fn done(&self) -> BoxFuture<'static, anyhow::Result<JobOutcome>> {
        let receiver = self.receiver.lock().take().expect("done called once");
        async move { Ok(receiver.await?) }.boxed()
    }

    fn read_output(&self) -> Option<String> {
        self.reads.fetch_add(1, Ordering::AcqRel);
        Some("stolen output".to_owned())
    }
}

fn producer(
    label: &str,
    owner: Option<Arc<Agent>>,
) -> (JobStart, oneshot::Sender<JobOutcome>, Arc<AtomicUsize>) {
    let (sender, receiver) = oneshot::channel();
    let reads = Arc::new(AtomicUsize::new(0));
    let hooks = ControlledHooks {
        receiver: Mutex::new(Some(receiver)),
        reads: reads.clone(),
    };
    (
        JobStart {
            kind: "bash".to_owned(),
            label: label.to_owned(),
            output_limit_bytes: Some(1_024),
            owner,
            run: Box::new(move || Box::new(hooks)),
        },
        sender,
        reads,
    )
}

struct Harness {
    context: Context,
    sessions: Arc<SessionStore>,
    agents: Arc<AgentRegistry>,
    jobs: Arc<LocalJobRegistry>,
    runtime: Arc<SessionApiProxyRuntime>,
    _controller: EffectHandle,
}

impl Harness {
    fn new() -> Self {
        let context = Context::new();
        let sessions = SessionStore::install(&context).unwrap();
        let agents = Arc::new(AgentRegistry::new(context.clone()));
        agents.provide(&context).unwrap();
        let jobs = LocalJobRegistry::new(&context, JobsConfig::default()).unwrap();
        let controller = jobs.attach_controller("api-proxy-test");
        let runtime = SessionApiProxyRuntime::from_context(
            &context,
            SessionApiProxyOptions::default(),
            Arc::new(TerminalDomains),
        )
        .unwrap();
        Self {
            context,
            sessions,
            agents,
            jobs,
            runtime,
            _controller: controller,
        }
    }

    fn agent(&self, id: &str) -> Arc<Agent> {
        let session = self
            .sessions
            .create(
                &self.context,
                Some(SessionId::new(id)),
                CreateSessionOptions {
                    cwd: Some("/project".to_owned()),
                    ..CreateSessionOptions::default()
                },
            )
            .unwrap();
        let scope = create_scope(&self.context, ScopeKey::new(), None).unwrap();
        let scope_key = seekdeep_scope::scope_of(&scope.context).unwrap();
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
        let agent = Arc::new(Agent::new(
            session.id().clone(),
            AgentOptions::default(),
            session,
            inbox,
            scope.context,
            scope_key,
        ));
        self.agents.register(&self.context, &agent, None).unwrap();
        agent
    }

    fn mux(&self, signal: AbortSignal) -> ApiDownlinkStream<MuxFrame> {
        self.runtime
            .mux(RpcRequest::new(RpcId::new("jobs-mux"), json!({})), signal)
    }
}

async fn next_jobs(stream: &mut ApiDownlinkStream<MuxFrame>) -> (SessionId, Vec<Value>) {
    loop {
        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let MuxFrame::SessionJobs { session_id, jobs } = frame.payload {
            return (
                session_id,
                jobs.into_iter()
                    .map(|job| serde_json::to_value(job).unwrap())
                    .collect(),
            );
        }
    }
}

#[tokio::test]
async fn baseline_is_absent_when_empty_and_carries_only_browser_safe_fields_when_nonempty() {
    let harness = Harness::new();
    let agent = harness.agent("baseline");
    let signal = AbortSignal::default();
    let mut empty = harness.mux(signal.clone());
    let subscribed = empty.next().await.unwrap().unwrap();
    assert!(matches!(
        subscribed.payload,
        MuxFrame::SessionSubscribed { .. }
    ));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), empty.next())
            .await
            .is_err()
    );
    signal.abort();

    let (spec, _settle, reads) = producer("pnpm run build", Some(agent.clone()));
    harness.jobs.start(spec);
    let signal = AbortSignal::default();
    let mut mux = harness.mux(signal.clone());
    let (session_id, jobs) = next_jobs(&mut mux).await;
    assert_eq!(session_id, *agent.id());
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["id"], "bash-1");
    assert_eq!(jobs[0]["status"], "running");
    assert_eq!(jobs[0]["label"], "pnpm run build");
    assert!(jobs[0].get("ownerSession").is_none());
    assert!(jobs[0].get("reported").is_none());
    assert!(jobs[0].get("outputLimitBytes").is_none());
    assert_eq!(reads.load(Ordering::Acquire), 0);
    signal.abort();
}

#[tokio::test]
async fn owner_changes_push_running_stopping_and_terminal_whole_sets_without_reading_output() {
    let harness = Harness::new();
    let agent = harness.agent("changes");
    let signal = AbortSignal::default();
    let mut mux = harness.mux(signal.clone());
    let _subscribed = mux.next().await.unwrap().unwrap();
    let (spec, settle, reads) = producer("sleep 60", Some(agent.clone()));
    let id = harness.jobs.start(spec);
    let (_, running) = next_jobs(&mut mux).await;
    harness.jobs.kill(&id, Some(&agent), Some("test")).unwrap();
    let (_, stopping) = next_jobs(&mut mux).await;
    settle
        .send(JobOutcome {
            status: JobTerminalStatus::Killed,
            detail: Some("signal: SIGTERM".to_owned()),
            output: None,
        })
        .unwrap();
    let (_, killed) = next_jobs(&mut mux).await;
    assert_eq!(running[0]["status"], "running");
    assert_eq!(stopping[0]["status"], "stopping");
    assert_eq!(killed[0]["status"], "killed");
    assert_eq!(killed[0]["detail"], "signal: SIGTERM");
    assert!(killed[0]["finishedAt"].is_number());
    assert_eq!(reads.load(Ordering::Acquire), 0);
    signal.abort();
}

#[tokio::test]
async fn unowned_changes_fan_out_and_new_sessions_receive_the_existing_unowned_set() {
    let harness = Harness::new();
    harness.agent("first");
    harness
        .sessions
        .create(
            &harness.context,
            Some(SessionId::new("second")),
            CreateSessionOptions {
                cwd: Some("/project".to_owned()),
                ..CreateSessionOptions::default()
            },
        )
        .unwrap();
    let signal = AbortSignal::default();
    let mut mux = harness.mux(signal.clone());
    let _ = mux.next().await;
    let _ = mux.next().await;
    let (spec, _settle, _reads) = producer("visible to every caller", None);
    harness.jobs.start(spec);
    let first = next_jobs(&mut mux).await.0;
    let second = next_jobs(&mut mux).await.0;
    assert_ne!(first, second);

    harness
        .sessions
        .create(
            &harness.context,
            Some(SessionId::new("third")),
            CreateSessionOptions {
                cwd: Some("/project".to_owned()),
                ..CreateSessionOptions::default()
            },
        )
        .unwrap();
    let (third, jobs) = next_jobs(&mut mux).await;
    assert_eq!(third, SessionId::new("third"));
    assert_eq!(jobs[0]["label"], "visible to every caller");
    signal.abort();
}
