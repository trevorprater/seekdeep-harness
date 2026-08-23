//! Production `session.search` cases ported from the Host search suite.

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_agent::{AGENTS, AgentRegistry};
use seekdeep_client_connection::{HttpResponse, RpcResult};
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{SessionHeader, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_host_apiproxy::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, ColdArtifactMetadata, RpcId, RpcMethod,
    RpcReceipt, RpcReceiptReason, RpcRequest, RpcResponse, SessionApiProxyOptions,
    SessionApiProxyRuntime, SessionApiProxyServices, SessionProjectionReads,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
    },
};
use seekdeep_llm::AbortSignal;
use seekdeep_session_persistence::{
    SessionInspection, SessionLocation, SessionPersistence, SessionPersistenceRevision,
    SessionPersistenceSnapshot,
};
use seekdeep_session_projection::ProjectionSnapshot;
use seekdeep_session_query::{
    SessionCorpus, SessionQueryEngine, SessionQueryError, SessionQueryErrorCode,
    SessionQueryService, SessionSearchCursor,
    types::{
        SessionEventRecord, SessionEventSearchHit, SessionEventSearchPage,
        SessionEventSearchRequest, SessionEventSurface, SessionSearchExecContext, SessionSearchHit,
        SessionSearchPage, SessionSearchRequest,
    },
};
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

#[derive(Debug)]
struct NoProjections;

impl SessionProjectionReads for NoProjections {
    fn live_snapshot(
        &self,
        _session: &Arc<seekdeep_core::session::Session>,
    ) -> anyhow::Result<Option<ProjectionSnapshot>> {
        Ok(None)
    }

    fn cached_snapshot(&self, _meta: &SessionHeader) -> anyhow::Result<Option<ProjectionSnapshot>> {
        Ok(None)
    }

    fn snapshot_for_events(
        &self,
        _events: &[seekdeep_core::session::SessionEvent],
    ) -> anyhow::Result<Option<ProjectionSnapshot>> {
        Ok(None)
    }
}

enum SearchStep {
    Page(SessionSearchPage<SessionSearchHit>),
    Error(SessionQueryErrorCode),
    AbortThenError(SessionQueryErrorCode),
    WaitForAbort,
}

struct ScriptedQueryEngine {
    corpus: Arc<SessionCorpus>,
    steps: Mutex<VecDeque<SearchStep>>,
    requests: Mutex<Vec<SessionSearchRequest>>,
    calls: AtomicUsize,
}

impl ScriptedQueryEngine {
    fn new(context: &Context, steps: impl IntoIterator<Item = SearchStep>) -> Arc<Self> {
        Arc::new(Self {
            corpus: SessionCorpus::new(context, 4),
            steps: Mutex::new(steps.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl SessionQueryEngine for ScriptedQueryEngine {
    fn corpus(&self) -> &SessionCorpus {
        &self.corpus
    }

    fn read_window_max(&self) -> u64 {
        50
    }

    async fn search_sessions(
        &self,
        request: SessionSearchRequest,
        exec: Option<SessionSearchExecContext>,
    ) -> anyhow::Result<SessionSearchPage<SessionSearchHit>> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        self.requests.lock().push(request);
        let step = self.steps.lock().pop_front().expect("scripted search step");
        match step {
            SearchStep::Page(page) => Ok(page),
            SearchStep::Error(code) => {
                Err(SessionQueryError::new("scripted provider failure", code).into())
            }
            SearchStep::AbortThenError(code) => {
                exec.and_then(|exec| exec.signal)
                    .expect("carrier signal")
                    .abort();
                Err(SessionQueryError::new("coincident provider failure", code).into())
            }
            SearchStep::WaitForAbort => {
                let signal = exec.and_then(|exec| exec.signal).expect("carrier signal");
                signal.cancelled().await;
                Err(SessionQueryError::new(
                    "provider observed cancellation",
                    SessionQueryErrorCode::SessionQueryAborted,
                )
                .into())
            }
        }
    }

    async fn search_events(
        &self,
        _request: SessionEventSearchRequest,
        _exec: Option<SessionSearchExecContext>,
    ) -> anyhow::Result<SessionEventSearchPage> {
        anyhow::bail!("event search is not used")
    }
}

struct BlockingMetadata {
    started: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
    all_started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl BlockingMetadata {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            started: Arc::new(AtomicUsize::new(0)),
            completed: Arc::new(AtomicUsize::new(0)),
            all_started: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
        })
    }
}

impl ColdArtifactMetadata for BlockingMetadata {
    fn size(&self, _path: PathBuf) -> BoxFuture<'static, anyhow::Result<u64>> {
        let started = self.started.clone();
        let completed = self.completed.clone();
        let all_started = self.all_started.clone();
        let release = self.release.clone();
        async move {
            let count = started.fetch_add(1, Ordering::AcqRel) + 1;
            if count == 16 {
                all_started.notify_one();
            }
            release.notified().await;
            completed.fetch_add(1, Ordering::AcqRel);
            Ok(1)
        }
        .boxed()
    }
}

#[derive(Default)]
struct ColdHeadersPersistence {
    headers: Vec<SessionHeader>,
}

#[async_trait]
impl SessionPersistence for ColdHeadersPersistence {
    fn locate(&self, meta: &SessionHeader) -> Option<SessionLocation> {
        Some(SessionLocation {
            kind: "blocking-test".to_owned(),
            path: PathBuf::from(format!("/blocking/{}", meta.id)),
        })
    }

    fn supports_raw_artifacts(&self) -> bool {
        false
    }

    async fn create(&self, _meta: &SessionHeader) -> anyhow::Result<()> {
        Ok(())
    }

    async fn append(
        &self,
        _id: &SessionId,
        _events: &[seekdeep_core::session::SessionEvent],
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn load(&self, _id: &SessionId) -> anyhow::Result<SessionInspection> {
        anyhow::bail!("cold log load is not expected")
    }

    async fn inspect(
        &self,
        _id: &SessionId,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        anyhow::bail!("cold inspection is not expected")
    }

    async fn read_from(
        &self,
        _id: &SessionId,
        _from_seq: u64,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        anyhow::bail!("cancellation must win before the cold read")
    }

    async fn list(&self, _signal: Option<AbortSignal>) -> anyhow::Result<Vec<SessionHeader>> {
        Ok(self.headers.clone())
    }

    async fn list_snapshots(
        &self,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SessionPersistenceSnapshot>> {
        Ok(self
            .headers
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
}

impl Harness {
    fn new() -> Self {
        let context = Context::new();
        let sessions = SessionStore::install(&context).unwrap();
        let agents = Arc::new(AgentRegistry::new(context.clone()));
        agents.provide(&context).unwrap();
        assert!(context.get(AGENTS).is_some());
        Self {
            context,
            sessions,
            agents,
        }
    }

    fn visible(&self, id: &str) -> SessionHeader {
        self.sessions
            .create(
                &self.context,
                Some(SessionId::new(id)),
                CreateSessionOptions {
                    cwd: Some("/project".to_owned()),
                    ..CreateSessionOptions::default()
                },
            )
            .unwrap()
            .header()
            .clone()
    }

    fn runtime(&self, engine: Option<Arc<ScriptedQueryEngine>>) -> Arc<SessionApiProxyRuntime> {
        let query = engine.map(|engine| SessionQueryService::new(engine));
        SessionApiProxyRuntime::new(
            SessionApiProxyServices {
                context: self.context.clone(),
                sessions: self.sessions.clone(),
                agents: self.agents.clone(),
                persistence: None,
                query,
                projections: Arc::new(NoProjections),
                projection_registry: None,
                tools: None,
            },
            SessionApiProxyOptions::default(),
            Arc::new(TerminalDomains),
        )
    }
}

fn page(items: Vec<SessionSearchHit>, cursor: Option<&str>) -> SessionSearchPage<SessionSearchHit> {
    SessionSearchPage {
        items,
        next_cursor: cursor.map(SessionSearchCursor::new),
    }
}

fn hit(
    header: &SessionHeader,
    best_session_id: &str,
    event_type: &str,
    surface: SessionEventSurface,
    snippet: &str,
) -> SessionSearchHit {
    SessionSearchHit {
        record: seekdeep_session_query::SessionRecord {
            header: header.clone(),
            live: true,
            persisted: false,
        },
        best_match: SessionEventSearchHit {
            record: SessionEventRecord {
                session_id: SessionId::new(best_session_id),
                seq: 0,
                event_type: event_type.to_owned(),
                time: 1,
                surface,
            },
            snippet: snippet.to_owned(),
        },
    }
}

async fn search(runtime: &SessionApiProxyRuntime, signal: AbortSignal) -> RpcResult<Value> {
    runtime
        .unary(
            RpcMethod::SessionSearch,
            RpcRequest::new(RpcId::new("search-test"), json!({ "query": "needle" })),
            signal,
        )
        .await
        .unwrap()
        .result
}

fn success(result: RpcResult<Value>) -> Value {
    match result {
        RpcResult::Success { value: Some(value) } => value,
        other => panic!("expected search success, got {other:?}"),
    }
}

fn error_code(result: RpcResult<Value>) -> String {
    match result {
        RpcResult::Failure { error } => error.code,
        other @ RpcResult::Success { .. } => panic!("expected search failure, got {other:?}"),
    }
}

#[tokio::test]
async fn search_authorizes_visible_current_message_hits_and_bounds_unicode_snippets() {
    let harness = Harness::new();
    let visible = harness.visible("visible");
    let hidden = header("hidden");
    let snippet = format!("{}😀tail", "x".repeat(239));
    let engine = ScriptedQueryEngine::new(
        &harness.context,
        [SearchStep::Page(page(
            vec![
                hit(
                    &hidden,
                    "hidden",
                    "user/message",
                    SessionEventSurface::Current,
                    "hidden",
                ),
                hit(
                    &visible,
                    "other",
                    "user/message",
                    SessionEventSurface::Current,
                    "mismatch",
                ),
                hit(
                    &visible,
                    "visible",
                    "tool/call",
                    SessionEventSurface::Current,
                    "wrong type",
                ),
                hit(
                    &visible,
                    "visible",
                    "assistant/message",
                    SessionEventSurface::Shadowed,
                    "shadowed",
                ),
                hit(
                    &visible,
                    "visible",
                    "assistant/message",
                    SessionEventSurface::Current,
                    &snippet,
                ),
                hit(
                    &visible,
                    "visible",
                    "user/message",
                    SessionEventSurface::Current,
                    "duplicate",
                ),
            ],
            None,
        ))],
    );
    let value = success(search(&harness.runtime(Some(engine)), AbortSignal::default()).await);
    assert_eq!(value["items"].as_array().unwrap().len(), 1);
    assert_eq!(value["items"][0]["sessionId"], "visible");
    let bounded = value["items"][0]["snippet"].as_str().unwrap();
    assert_eq!(bounded.chars().count(), 240);
    assert!(bounded.ends_with('😀'));
    assert_eq!(value["hasMore"], false);
}

#[tokio::test]
async fn empty_visibility_and_missing_query_service_have_distinct_results() {
    let harness = Harness::new();
    let engine = ScriptedQueryEngine::new(&harness.context, []);
    let value = success(
        search(
            &harness.runtime(Some(engine.clone())),
            AbortSignal::default(),
        )
        .await,
    );
    assert_eq!(value, json!({ "items": [], "hasMore": false }));
    assert_eq!(engine.calls.load(Ordering::Acquire), 0);

    let visible = Harness::new();
    visible.visible("visible");
    assert_eq!(
        error_code(search(&visible.runtime(None), AbortSignal::default()).await),
        "internal"
    );
}

#[tokio::test]
async fn pagination_collects_twenty_plus_lookahead_without_provider_visibility_bindings() {
    let harness = Harness::new();
    let headers = (0..21)
        .map(|index| harness.visible(&format!("visible-{index:02}")))
        .collect::<Vec<_>>();
    let first = headers[..20]
        .iter()
        .map(|header| {
            hit(
                header,
                header.id.as_str(),
                "user/message",
                SessionEventSurface::Current,
                header.id.as_str(),
            )
        })
        .collect();
    let lookahead = hit(
        &headers[20],
        headers[20].id.as_str(),
        "assistant/message",
        SessionEventSurface::Current,
        "lookahead",
    );
    let engine = ScriptedQueryEngine::new(
        &harness.context,
        [
            SearchStep::Page(page(first, Some("next"))),
            SearchStep::Page(page(vec![lookahead], None)),
        ],
    );
    let value = success(
        search(
            &harness.runtime(Some(engine.clone())),
            AbortSignal::default(),
        )
        .await,
    );
    assert_eq!(value["items"].as_array().unwrap().len(), 20);
    assert_eq!(value["hasMore"], true);
    let requests = engine.requests.lock();
    assert!(
        requests
            .iter()
            .all(|request| request.session_filters.is_none())
    );
    assert_eq!(requests[0].limit, Some(20));
    assert_eq!(requests[1].cursor.as_ref().unwrap().as_str(), "next");
}

#[tokio::test]
async fn provider_limit_probe_is_learned_and_continuations_use_it() {
    let harness = Harness::new();
    let headers = (0..21)
        .map(|index| harness.visible(&format!("limited-{index:02}")))
        .collect::<Vec<_>>();
    let hits = headers
        .iter()
        .map(|header| {
            hit(
                header,
                header.id.as_str(),
                "user/message",
                SessionEventSurface::Current,
                "hit",
            )
        })
        .collect::<Vec<_>>();
    let engine = ScriptedQueryEngine::new(
        &harness.context,
        [
            SearchStep::Error(SessionQueryErrorCode::SessionQueryInvalidLimit),
            SearchStep::Page(page(hits[..10].to_vec(), Some("a"))),
            SearchStep::Page(page(hits[10..20].to_vec(), Some("b"))),
            SearchStep::Page(page(hits[20..].to_vec(), None)),
        ],
    );
    let value = success(
        search(
            &harness.runtime(Some(engine.clone())),
            AbortSignal::default(),
        )
        .await,
    );
    assert_eq!(value["hasMore"], true);
    assert_eq!(
        engine
            .requests
            .lock()
            .iter()
            .map(|request| request.limit)
            .collect::<Vec<_>>(),
        [Some(20), Some(10), Some(10), Some(10)]
    );
}

#[tokio::test]
async fn learned_limit_is_also_the_provider_overproduction_guard() {
    let harness = Harness::new();
    let header = harness.visible("limited-overproduction");
    let overproduced = (0..11)
        .map(|_| {
            hit(
                &header,
                header.id.as_str(),
                "user/message",
                SessionEventSurface::Current,
                "hit",
            )
        })
        .collect();
    let engine = ScriptedQueryEngine::new(
        &harness.context,
        [
            SearchStep::Error(SessionQueryErrorCode::SessionQueryInvalidLimit),
            SearchStep::Page(page(overproduced, None)),
        ],
    );
    assert_eq!(
        error_code(
            search(
                &harness.runtime(Some(engine.clone())),
                AbortSignal::default()
            )
            .await
        ),
        "internal"
    );
    assert_eq!(engine.requests.lock()[1].limit, Some(10));
}

#[tokio::test]
async fn stale_continuation_discards_partial_results_and_restarts_from_page_one() {
    let harness = Harness::new();
    let old = harness.visible("old");
    let fresh = harness.visible("fresh");
    let engine = ScriptedQueryEngine::new(
        &harness.context,
        [
            SearchStep::Page(page(
                vec![hit(
                    &old,
                    "old",
                    "user/message",
                    SessionEventSurface::Current,
                    "old",
                )],
                Some("stale"),
            )),
            SearchStep::Error(SessionQueryErrorCode::SessionQueryStaleCursor),
            SearchStep::Page(page(
                vec![hit(
                    &fresh,
                    "fresh",
                    "assistant/message",
                    SessionEventSurface::Current,
                    "fresh",
                )],
                None,
            )),
        ],
    );
    let value = success(
        search(
            &harness.runtime(Some(engine.clone())),
            AbortSignal::default(),
        )
        .await,
    );
    assert_eq!(
        value["items"],
        json!([{ "sessionId": "fresh", "snippet": "fresh" }])
    );
    let requests = engine.requests.lock();
    assert!(requests[0].cursor.is_none());
    assert_eq!(requests[1].cursor.as_ref().unwrap().as_str(), "stale");
    assert!(requests[2].cursor.is_none());
}

#[tokio::test]
async fn continuous_stale_restarts_stop_at_the_hundred_call_budget() {
    let harness = Harness::new();
    harness.visible("visible");
    let mut steps = Vec::new();
    for index in 0..50 {
        steps.push(SearchStep::Page(page(
            Vec::new(),
            Some(&format!("cursor-{index}")),
        )));
        steps.push(SearchStep::Error(
            SessionQueryErrorCode::SessionQueryStaleCursor,
        ));
    }
    let engine = ScriptedQueryEngine::new(&harness.context, steps);
    assert_eq!(
        error_code(
            search(
                &harness.runtime(Some(engine.clone())),
                AbortSignal::default()
            )
            .await
        ),
        "internal"
    );
    assert_eq!(engine.calls.load(Ordering::Acquire), 100);
}

#[tokio::test]
async fn repeated_cursor_and_provider_overproduction_fail_closed() {
    let harness = Harness::new();
    let header = harness.visible("visible");
    let repeated = ScriptedQueryEngine::new(
        &harness.context,
        [
            SearchStep::Page(page(Vec::new(), Some("same"))),
            SearchStep::Page(page(Vec::new(), Some("same"))),
        ],
    );
    assert_eq!(
        error_code(search(&harness.runtime(Some(repeated)), AbortSignal::default()).await),
        "internal"
    );
    let overproduced = (0..21)
        .map(|_| {
            hit(
                &header,
                "visible",
                "user/message",
                SessionEventSurface::Current,
                "hit",
            )
        })
        .collect();
    let oversized = ScriptedQueryEngine::new(
        &harness.context,
        [SearchStep::Page(page(overproduced, None))],
    );
    assert_eq!(
        error_code(search(&harness.runtime(Some(oversized)), AbortSignal::default()).await),
        "internal"
    );
}

#[tokio::test]
async fn invalid_limit_adaptation_stops_at_one_and_never_adapts_a_continuation() {
    let harness = Harness::new();
    harness.visible("visible");
    let floor = ScriptedQueryEngine::new(
        &harness.context,
        [
            SearchStep::Error(SessionQueryErrorCode::SessionQueryInvalidLimit),
            SearchStep::Error(SessionQueryErrorCode::SessionQueryInvalidLimit),
            SearchStep::Error(SessionQueryErrorCode::SessionQueryInvalidLimit),
            SearchStep::Error(SessionQueryErrorCode::SessionQueryInvalidLimit),
            SearchStep::Error(SessionQueryErrorCode::SessionQueryInvalidLimit),
        ],
    );
    assert_eq!(
        error_code(
            search(
                &harness.runtime(Some(floor.clone())),
                AbortSignal::default()
            )
            .await
        ),
        "internal"
    );
    assert_eq!(
        floor
            .requests
            .lock()
            .iter()
            .map(|request| request.limit)
            .collect::<Vec<_>>(),
        [Some(20), Some(10), Some(5), Some(2), Some(1)]
    );

    let continuation = ScriptedQueryEngine::new(
        &harness.context,
        [
            SearchStep::Page(page(Vec::new(), Some("next"))),
            SearchStep::Error(SessionQueryErrorCode::SessionQueryInvalidLimit),
        ],
    );
    assert_eq!(
        error_code(
            search(
                &harness.runtime(Some(continuation.clone())),
                AbortSignal::default()
            )
            .await
        ),
        "internal"
    );
    assert_eq!(continuation.calls.load(Ordering::Acquire), 2);
}

#[tokio::test]
async fn cancellation_wins_before_and_during_provider_failures() {
    let harness = Harness::new();
    harness.visible("visible");
    let preaborted = AbortSignal::default();
    preaborted.abort();
    let unused = ScriptedQueryEngine::new(&harness.context, []);
    assert_eq!(
        error_code(search(&harness.runtime(Some(unused.clone())), preaborted).await),
        "cancelled"
    );
    assert_eq!(unused.calls.load(Ordering::Acquire), 0);

    for code in [
        SessionQueryErrorCode::SessionQueryStaleCursor,
        SessionQueryErrorCode::SessionQueryInvalidLimit,
    ] {
        let engine = ScriptedQueryEngine::new(&harness.context, [SearchStep::AbortThenError(code)]);
        assert_eq!(
            error_code(search(&harness.runtime(Some(engine)), AbortSignal::default()).await),
            "cancelled"
        );
    }

    let waiting = ScriptedQueryEngine::new(&harness.context, [SearchStep::WaitForAbort]);
    let runtime = harness.runtime(Some(waiting));
    let signal = AbortSignal::default();
    let pending = search(&runtime, signal.clone());
    tokio::task::yield_now().await;
    signal.abort();
    assert_eq!(error_code(pending.await), "cancelled");
}

#[tokio::test]
async fn cancellation_awaits_every_started_cold_metadata_read_and_stops_next_batch() {
    let harness = Harness::new();
    let headers = (0..17)
        .map(|index| header(&format!("cold-{index:02}")))
        .collect::<Vec<_>>();
    let persistence: Arc<dyn SessionPersistence> = Arc::new(ColdHeadersPersistence { headers });
    let engine = ScriptedQueryEngine::new(&harness.context, []);
    let query = SessionQueryService::new(engine.clone());
    let metadata = BlockingMetadata::new();
    let runtime = SessionApiProxyRuntime::new(
        SessionApiProxyServices {
            context: harness.context.clone(),
            sessions: harness.sessions.clone(),
            agents: harness.agents.clone(),
            persistence: Some(persistence),
            query: Some(query),
            projections: Arc::new(NoProjections),
            projection_registry: None,
            tools: None,
        },
        SessionApiProxyOptions {
            artifact_metadata: Some(metadata.clone()),
            ..SessionApiProxyOptions::default()
        },
        Arc::new(TerminalDomains),
    );
    let signal = AbortSignal::default();
    let request_signal = signal.clone();
    let searching = tokio::spawn(async move { search(&runtime, request_signal).await });
    metadata.all_started.notified().await;
    assert_eq!(metadata.started.load(Ordering::Acquire), 16);
    signal.abort();
    tokio::task::yield_now().await;
    assert!(!searching.is_finished());
    metadata.release.notify_waiters();
    assert_eq!(error_code(searching.await.unwrap()), "cancelled");
    assert_eq!(metadata.completed.load(Ordering::Acquire), 16);
    assert_eq!(metadata.started.load(Ordering::Acquire), 16);
    assert_eq!(engine.calls.load(Ordering::Acquire), 0);
}

fn header(id: &str) -> SessionHeader {
    let mut header = SessionHeader::new(SessionId::new(id));
    header.cwd = Some("/project".to_owned());
    header
}
