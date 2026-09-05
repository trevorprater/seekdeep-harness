//! Production `session.list` cases ported from the blank and cold Session suites.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentOptions, AgentRegistry, AgentStatus, Inbox, NoopInboxNotifications,
};
use seekdeep_client_connection::{HttpResponse, RpcResult};
use seekdeep_cordis::{Context, Fiber};
use seekdeep_core::{
    session::{AppendOptions, Session, SessionEvent, SessionHeader, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_host_apiproxy::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, RpcId, RpcMethod, RpcReceipt,
    RpcReceiptReason, RpcRequest, RpcResponse, SessionApiProxyOptions, SessionApiProxyRuntime,
    SessionApiProxyServices, SessionProjectionReads,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
    },
};
use seekdeep_llm::{AbortSignal, ContentBlock, MessageSource, UserMessage};
use seekdeep_scope::ScopeKey;
use seekdeep_session_persistence::{
    SessionInspection, SessionLocation, SessionPersistence, SessionPersistenceRevision,
    SessionPersistenceSnapshot,
};
use seekdeep_session_projection::{ProjectionSnapshot, SessionProjectionRegistry};
use serde_json::{Value, json};

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

#[derive(Default)]
struct FakeProjectionReads {
    live: Mutex<HashMap<SessionId, ProjectionSnapshot>>,
    cached: Mutex<HashMap<SessionId, ProjectionSnapshot>>,
    fail_live: Mutex<HashSet<SessionId>>,
    fail_cached: Mutex<HashSet<SessionId>>,
}

impl SessionProjectionReads for FakeProjectionReads {
    fn live_snapshot(&self, session: &Arc<Session>) -> anyhow::Result<Option<ProjectionSnapshot>> {
        if self.fail_live.lock().contains(session.id()) {
            anyhow::bail!("hostile live projection")
        }
        Ok(self.live.lock().get(session.id()).cloned())
    }

    fn cached_snapshot(&self, meta: &SessionHeader) -> anyhow::Result<Option<ProjectionSnapshot>> {
        if self.fail_cached.lock().contains(&meta.id) {
            anyhow::bail!("hostile cached projection")
        }
        Ok(self.cached.lock().get(&meta.id).cloned())
    }

    fn snapshot_for_events(
        &self,
        _events: &[SessionEvent],
    ) -> anyhow::Result<Option<ProjectionSnapshot>> {
        Ok(None)
    }
}

#[derive(Default)]
struct FakePersistence {
    headers: Mutex<Vec<SessionHeader>>,
    inspections: Mutex<HashMap<SessionId, SessionInspection>>,
    locations: Mutex<HashMap<SessionId, PathBuf>>,
    read_failures: Mutex<HashSet<SessionId>>,
    reads: AtomicUsize,
}

#[async_trait]
impl SessionPersistence for FakePersistence {
    fn locate(&self, meta: &SessionHeader) -> Option<SessionLocation> {
        self.locations
            .lock()
            .get(&meta.id)
            .map(|path| SessionLocation {
                kind: "test".to_owned(),
                path: path.clone(),
            })
    }

    fn supports_raw_artifacts(&self) -> bool {
        false
    }

    async fn create(&self, _meta: &SessionHeader) -> anyhow::Result<()> {
        Ok(())
    }

    async fn append(&self, _id: &SessionId, _events: &[SessionEvent]) -> anyhow::Result<()> {
        Ok(())
    }

    async fn load(&self, id: &SessionId) -> anyhow::Result<SessionInspection> {
        self.inspect(id, None).await
    }

    async fn inspect(
        &self,
        id: &SessionId,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        self.inspections
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("session not found"))
    }

    async fn read_from(
        &self,
        id: &SessionId,
        _from_seq: u64,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        self.reads.fetch_add(1, Ordering::AcqRel);
        if self.read_failures.lock().contains(id) {
            anyhow::bail!("simulated read failure")
        }
        self.inspect(id, None).await
    }

    async fn list(&self, _signal: Option<AbortSignal>) -> anyhow::Result<Vec<SessionHeader>> {
        Ok(self.headers.lock().clone())
    }

    async fn list_snapshots(
        &self,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SessionPersistenceSnapshot>> {
        Ok(self
            .headers
            .lock()
            .iter()
            .cloned()
            .map(|header| SessionPersistenceSnapshot {
                revision: SessionPersistenceRevision::new(format!("test:{}", header.id)),
                header,
            })
            .collect())
    }
}

struct Harness {
    context: Context,
    sessions: Arc<SessionStore>,
    agents: Arc<AgentRegistry>,
    projections: Arc<FakeProjectionReads>,
}

impl Harness {
    fn new() -> Self {
        let context = Context::new();
        let sessions = SessionStore::install(&context).unwrap();
        let agents = Arc::new(AgentRegistry::new(context.clone()));
        agents.provide(&context).unwrap();
        Self {
            context,
            sessions,
            agents,
            projections: Arc::new(FakeProjectionReads::default()),
        }
    }

    fn runtime(
        &self,
        persistence: Option<Arc<dyn SessionPersistence>>,
        options: SessionApiProxyOptions,
    ) -> Arc<SessionApiProxyRuntime> {
        SessionApiProxyRuntime::new(
            SessionApiProxyServices {
                context: self.context.clone(),
                sessions: self.sessions.clone(),
                agents: self.agents.clone(),
                persistence,
                query: None,
                projections: self.projections.clone(),
                projection_registry: None,
                tools: None,
                jobs: None,
                subagents: None,
            },
            options,
            Arc::new(TerminalDomains),
        )
    }

    fn session(&self, id: &str, created_at: u64) -> Arc<Session> {
        self.sessions
            .create(
                &self.context,
                Some(SessionId::new(id)),
                CreateSessionOptions {
                    created_at: Some(created_at),
                    cwd: Some("/project".to_owned()),
                    ..CreateSessionOptions::default()
                },
            )
            .unwrap()
    }

    fn attach_agent(&self, session: Arc<Session>) -> Arc<Agent> {
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
        let agent = Arc::new(Agent::new(
            session.id().clone(),
            AgentOptions::default(),
            session,
            inbox,
            self.context.clone(),
            ScopeKey::new(),
        ));
        self.agents.register(&self.context, &agent, None).unwrap();
        agent
    }
}

async fn list(runtime: &SessionApiProxyRuntime) -> Vec<Value> {
    let response = runtime
        .unary(
            RpcMethod::SessionList,
            RpcRequest::new(RpcId::new("list-test"), json!({})),
            AbortSignal::default(),
        )
        .await
        .unwrap();
    match response.result {
        RpcResult::Success { value: Some(value) } => value["items"].as_array().unwrap().clone(),
        other => panic!("expected list success, got {other:?}"),
    }
}

fn by_id<'a>(items: &'a [Value], id: &str) -> &'a Value {
    items
        .iter()
        .find(|item| item["sessionId"] == id)
        .unwrap_or_else(|| panic!("missing summary {id}"))
}

fn header(id: &str, created_at: u64) -> SessionHeader {
    let mut header = SessionHeader::new(SessionId::new(id));
    header.created_at = created_at;
    header.cwd = Some("/project".to_owned());
    header
}

fn event(event_type: &str, seq: u64, time: i64, data: Value) -> SessionEvent {
    SessionEvent {
        event_type: event_type.to_owned(),
        seq,
        time,
        data,
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    }
}

fn projection(
    as_of_seq: i64,
    values: impl IntoIterator<Item = (&'static str, Value)>,
) -> ProjectionSnapshot {
    ProjectionSnapshot {
        as_of_seq,
        values: values
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    }
}

#[tokio::test]
async fn standalone_events_keep_blank_and_the_first_turn_clears_it() {
    let harness = Harness::new();
    let session = harness.session("blank", 500);
    let agent = harness.attach_agent(session.clone());
    harness.projections.live.lock().insert(
        session.id().clone(),
        projection(-1, [("constant", json!({ "enabled": true }))]),
    );
    let runtime = harness.runtime(None, SessionApiProxyOptions::default());
    let initial = list(&runtime).await;
    assert_eq!(by_id(&initial, "blank")["blank"], true);
    assert_eq!(by_id(&initial, "blank")["updatedAt"].as_f64(), Some(500.0));
    assert_eq!(
        by_id(&initial, "blank")["projections"]["values"]["constant"],
        json!({ "enabled": true })
    );

    session
        .append(
            "session/title",
            json!({ "title": "Standalone", "messageSeqs": [], "source": { "kind": "fallback" } }),
            AppendOptions::default(),
        )
        .unwrap();
    assert_eq!(by_id(&list(&runtime).await, "blank")["blank"], true);
    session
        .append("turn/start", json!({ "turn": 1 }), AppendOptions::default())
        .unwrap();
    agent.set_status(AgentStatus::Running);
    let running = list(&runtime).await;
    assert_eq!(by_id(&running, "blank")["blank"], false);
    assert_eq!(by_id(&running, "blank")["running"], true);

    let message = UserMessage::new(
        vec![ContentBlock::Text {
            text: "work".to_owned(),
        }],
        MessageSource::user(),
    );
    let prompt = session
        .append(
            "user/message",
            serde_json::to_value(message).unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    session
        .append(
            "session/title",
            json!({ "title": "Later", "messageSeqs": [], "source": { "kind": "fallback" } }),
            AppendOptions::default(),
        )
        .unwrap();
    assert_eq!(
        by_id(&list(&runtime).await, "blank")["updatedAt"].as_f64(),
        serde_json::Number::from(prompt.time).as_f64()
    );
}

fn cold_listing_fixture(harness: &Harness) -> (Arc<FakePersistence>, tempfile::TempDir) {
    let persistence = Arc::new(FakePersistence::default());
    let directory = tempfile::tempdir().unwrap();
    let small = directory.path().join("small.log");
    let large = directory.path().join("large.log");
    std::fs::write(&small, vec![b'x'; 1024]).unwrap();
    std::fs::write(&large, vec![b'x'; 1025]).unwrap();
    let ids = [
        ("small-blank", 100),
        ("small-conversation", 200),
        ("large-unknown", 300),
        ("cached-nonblank", 400),
        ("locationless", 500),
        ("read-failure", 600),
        ("cwdless", 700),
    ];
    let mut headers = ids
        .into_iter()
        .map(|(id, time)| header(id, time))
        .collect::<Vec<_>>();
    headers.last_mut().unwrap().cwd = None;
    persistence.headers.lock().clone_from(&headers);
    for id in ["small-blank", "small-conversation", "read-failure"] {
        persistence
            .locations
            .lock()
            .insert(SessionId::new(id), small.clone());
    }
    persistence
        .locations
        .lock()
        .insert(SessionId::new("large-unknown"), large);
    persistence
        .read_failures
        .lock()
        .insert(SessionId::new("read-failure"));
    persistence.inspections.lock().insert(
        SessionId::new("small-blank"),
        SessionInspection {
            meta: headers[0].clone(),
            events: vec![event("session/end-seed", 0, 700, json!({}))],
        },
    );
    persistence.inspections.lock().insert(
        SessionId::new("small-conversation"),
        SessionInspection {
            meta: headers[1].clone(),
            events: vec![
                event("turn/start", 0, 800, json!({ "turn": 1 })),
                event(
                    "user/message",
                    1,
                    1200,
                    json!({ "source": { "kind": "user" } }),
                ),
            ],
        },
    );
    harness.projections.cached.lock().insert(
        SessionId::new("small-blank"),
        projection(
            0,
            [(
                "sessionListMetadata",
                json!({
                    "blank": true, "lastPromptAt": null
                }),
            )],
        ),
    );
    harness.projections.cached.lock().insert(
        SessionId::new("small-conversation"),
        projection(
            0,
            [(
                "sessionListMetadata",
                json!({
                    "blank": true, "lastPromptAt": 900
                }),
            )],
        ),
    );
    harness.projections.cached.lock().insert(
        SessionId::new("cached-nonblank"),
        projection(
            1,
            [(
                "sessionListMetadata",
                json!({
                    "blank": false, "lastPromptAt": 1000
                }),
            )],
        ),
    );
    (persistence, directory)
}

#[tokio::test]
async fn cold_merge_bounds_blank_probes_and_keeps_unavailable_rows_visible() {
    let harness = Harness::new();
    let (persistence, _directory) = cold_listing_fixture(&harness);
    let runtime = harness.runtime(Some(persistence.clone()), SessionApiProxyOptions::default());
    let items = list(&runtime).await;
    assert_eq!(by_id(&items, "small-blank")["blank"], true);
    assert_eq!(by_id(&items, "small-conversation")["blank"], false);
    assert_eq!(
        by_id(&items, "small-conversation")["updatedAt"].as_f64(),
        Some(1200.0)
    );
    assert_eq!(by_id(&items, "large-unknown")["blank"], false);
    assert_eq!(
        by_id(&items, "cached-nonblank")["updatedAt"].as_f64(),
        Some(1000.0)
    );
    assert_eq!(by_id(&items, "locationless")["blank"], false);
    assert_eq!(by_id(&items, "read-failure")["blank"], false);
    assert!(!items.iter().any(|item| item["sessionId"] == "cwdless"));
    assert_eq!(persistence.reads.load(Ordering::Acquire), 3);
}

#[tokio::test]
async fn disabled_probe_and_projection_failure_degrade_without_hiding_rows() {
    let harness = Harness::new();
    let persistence = Arc::new(FakePersistence::default());
    let meta = header("probe-disabled", 100);
    persistence.headers.lock().push(meta.clone());
    persistence
        .locations
        .lock()
        .insert(meta.id.clone(), PathBuf::from("/must-not-read"));
    harness
        .projections
        .fail_cached
        .lock()
        .insert(meta.id.clone());
    let runtime = harness.runtime(
        Some(persistence.clone()),
        SessionApiProxyOptions {
            cold_blank_probe_max_bytes: Some(0),
            ..SessionApiProxyOptions::default()
        },
    );
    let items = list(&runtime).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["blank"], false);
    assert!(items[0].get("projections").is_none());
    assert_eq!(persistence.reads.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn gateway_lifecycle_owns_the_session_list_projection_definition() {
    let harness = Harness::new();
    let projections = SessionProjectionRegistry::install(&harness.context).unwrap();
    let fiber = Fiber::active_child("session list runtime");
    let child = harness.context.with_fiber(fiber.clone());
    let _runtime = SessionApiProxyRuntime::from_context(
        &child,
        SessionApiProxyOptions::default(),
        Arc::new(TerminalDomains),
    )
    .unwrap();
    let session = harness.session("projected", 100);
    let initial = projections.snapshot(&session).unwrap();
    assert_eq!(
        initial.values["sessionListMetadata"],
        json!({ "blank": true, "lastPromptAt": null })
    );
    session
        .append("turn/start", json!({ "turn": 1 }), AppendOptions::default())
        .unwrap();
    assert_eq!(
        projections.snapshot(&session).unwrap().values["sessionListMetadata"]["blank"],
        false
    );
    fiber.dispose().await.unwrap();
    assert!(
        !projections
            .snapshot(&session)
            .unwrap()
            .values
            .contains_key("sessionListMetadata")
    );
}
