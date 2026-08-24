//! Persistence churn, cancellation, generation, and close-fence parity.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_core::{
    session::{SESSION_FORMAT_VERSION, SessionEvent, SessionHeader, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::AbortSignal;
use seekdeep_session_persistence::{
    SessionInspection, SessionLocation, SessionPersistence, SessionPersistenceRevision,
    SessionPersistenceService, SessionPersistenceSnapshot,
};
use seekdeep_session_query::{
    SessionQueryEngine as _, SessionQueryError, SessionQueryErrorCode, SessionResultBound,
    types::{
        SessionEventMetadataFilter, SessionEventSearchRequest, SessionSearchExecContext,
        SessionSearchRequest,
    },
};
use seekdeep_session_query_sqlite::{OpenAt, SqliteSessionQueryConfig, SqliteSessionQueryEngine};
use serde_json::json;
use tokio::sync::Notify;

#[derive(Clone)]
struct StoredSession {
    header: SessionHeader,
    events: Vec<SessionEvent>,
    revision: u64,
}

#[derive(Clone)]
enum Failure {
    Plain(String),
    Query(SessionQueryError),
}

impl Failure {
    fn error(&self) -> anyhow::Error {
        match self {
            Self::Plain(message) => anyhow::Error::msg(message.clone()),
            Self::Query(error) => error.clone().into(),
        }
    }
}

#[derive(Default)]
struct OneShotGate {
    started: AtomicBool,
    released: AtomicBool,
    started_notify: Notify,
    release_notify: Notify,
}

impl OneShotGate {
    async fn wait_started(&self) {
        while !self.started.load(Ordering::Acquire) {
            self.started_notify.notified().await;
        }
    }

    async fn wait_release(&self) {
        self.started.store(true, Ordering::Release);
        self.started_notify.notify_waiters();
        while !self.released.load(Ordering::Acquire) {
            self.release_notify.notified().await;
        }
    }

    fn release(&self) {
        self.released.store(true, Ordering::Release);
        self.release_notify.notify_waiters();
    }
}

type SnapshotHook = Arc<dyn Fn(usize) + Send + Sync>;

#[derive(Default)]
struct ControlledState {
    entries: Mutex<HashMap<SessionId, StoredSession>>,
    next_revision: AtomicUsize,
    snapshots: AtomicUsize,
    inspections: Mutex<HashMap<SessionId, usize>>,
    failure: Mutex<Option<Failure>>,
    list_gate: Mutex<Option<Arc<OneShotGate>>>,
    snapshot_hook: Mutex<Option<SnapshotHook>>,
}

#[derive(Clone, Default)]
struct ControlledPersistence {
    state: Arc<ControlledState>,
}

impl ControlledPersistence {
    fn set(&self, header: SessionHeader, events: Vec<SessionEvent>) {
        let revision = u64::try_from(self.state.next_revision.fetch_add(1, Ordering::AcqRel) + 1)
            .expect("test revision");
        self.set_with_revision(header, events, revision);
    }

    fn set_with_revision(&self, header: SessionHeader, events: Vec<SessionEvent>, revision: u64) {
        self.state.entries.lock().insert(
            header.id.clone(),
            StoredSession {
                header,
                events,
                revision,
            },
        );
    }

    fn bump_revision(&self, id: &SessionId) {
        let revision = u64::try_from(self.state.next_revision.fetch_add(1, Ordering::AcqRel) + 1)
            .expect("test revision");
        self.state
            .entries
            .lock()
            .get_mut(id)
            .expect("entry")
            .revision = revision;
    }

    fn remove(&self, id: &SessionId) {
        self.state.entries.lock().remove(id);
    }

    fn set_failure(&self, failure: Option<Failure>) {
        *self.state.failure.lock() = failure;
    }

    fn gate_next_list(&self) -> Arc<OneShotGate> {
        let gate = Arc::new(OneShotGate::default());
        *self.state.list_gate.lock() = Some(gate.clone());
        gate
    }

    fn set_snapshot_hook(&self, hook: Option<SnapshotHook>) {
        *self.state.snapshot_hook.lock() = hook;
    }

    fn snapshot_count(&self) -> usize {
        self.state.snapshots.load(Ordering::Acquire)
    }

    fn inspection_count(&self, id: &SessionId) -> usize {
        self.state.inspections.lock().get(id).copied().unwrap_or(0)
    }

    fn entry(&self, id: &SessionId) -> anyhow::Result<StoredSession> {
        if let Some(failure) = self.state.failure.lock().clone() {
            return Err(failure.error());
        }
        self.state
            .entries
            .lock()
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("missing test session {id}"))
    }
}

#[async_trait]
impl SessionPersistence for ControlledPersistence {
    fn locate(&self, _meta: &SessionHeader) -> Option<SessionLocation> {
        None
    }

    fn supports_raw_artifacts(&self) -> bool {
        false
    }

    async fn create(&self, meta: &SessionHeader) -> anyhow::Result<()> {
        self.set(meta.clone(), Vec::new());
        Ok(())
    }

    async fn append(&self, id: &SessionId, events: &[SessionEvent]) -> anyhow::Result<()> {
        let mut entries = self.state.entries.lock();
        let entry = entries
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("missing test session {id}"))?;
        entry.events.extend_from_slice(events);
        entry.revision = entry.revision.saturating_add(1);
        Ok(())
    }

    async fn load(&self, id: &SessionId) -> anyhow::Result<SessionInspection> {
        let entry = self.entry(id)?;
        Ok(SessionInspection {
            meta: entry.header,
            events: entry.events,
        })
    }

    async fn inspect(
        &self,
        id: &SessionId,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        *self.state.inspections.lock().entry(id.clone()).or_insert(0) += 1;
        self.load(id).await
    }

    async fn read_from(
        &self,
        id: &SessionId,
        from_seq: u64,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        let mut inspection = self.load(id).await?;
        inspection.events.retain(|event| event.seq >= from_seq);
        Ok(inspection)
    }

    async fn list(&self, _signal: Option<AbortSignal>) -> anyhow::Result<Vec<SessionHeader>> {
        if let Some(failure) = self.state.failure.lock().clone() {
            return Err(failure.error());
        }
        Ok(self
            .state
            .entries
            .lock()
            .values()
            .map(|entry| entry.header.clone())
            .collect())
    }

    async fn list_snapshots(
        &self,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SessionPersistenceSnapshot>> {
        let call = self.state.snapshots.fetch_add(1, Ordering::AcqRel) + 1;
        let gate = self.state.list_gate.lock().take();
        if let Some(gate) = gate {
            gate.wait_release().await;
        }
        if let Some(failure) = self.state.failure.lock().clone() {
            return Err(failure.error());
        }
        let snapshots = self
            .state
            .entries
            .lock()
            .values()
            .map(|entry| SessionPersistenceSnapshot {
                header: entry.header.clone(),
                revision: SessionPersistenceRevision::new(format!("test:{}", entry.revision)),
            })
            .collect();
        let hook = self.state.snapshot_hook.lock().clone();
        if let Some(hook) = hook {
            hook(call);
        }
        Ok(snapshots)
    }
}

fn header(id: &str, created_at: u64) -> SessionHeader {
    SessionHeader {
        version: SESSION_FORMAT_VERSION,
        id: SessionId::new(id),
        created_at,
        cwd: None,
        parent_session: None,
        seed_length: None,
        origin: None,
        delegation_depth: None,
        agent_preset: None,
    }
}

fn message_events(text: &str) -> Vec<SessionEvent> {
    vec![message_event(text, 0, 1)]
}

fn message_event(text: &str, seq: u64, time: i64) -> SessionEvent {
    SessionEvent {
        event_type: "user/message".to_owned(),
        seq,
        time,
        data: json!({
            "id": format!("message-{text}"),
            "role": "user",
            "source": {"kind": "user"},
            "content": [{"type": "text", "text": text}]
        }),
        source_event_seqs: None,
        surface_op: Some(SurfaceOp::append()),
        ignorable: None,
    }
}

fn config(path: impl Into<String>) -> SqliteSessionQueryConfig {
    SqliteSessionQueryConfig {
        path: path.into(),
        open_at: OpenAt::FirstSearch,
        default_limit: 1,
        max_limit: 5,
        ..SqliteSessionQueryConfig::default()
    }
}

fn search(
    query: &str,
    cursor: Option<seekdeep_session_query::SessionSearchCursor>,
) -> SessionSearchRequest {
    SessionSearchRequest {
        query: query.to_owned(),
        session_filters: None,
        event_filters: None,
        limit: Some(1),
        cursor,
    }
}

fn mount(
    context: &seekdeep_cordis::Context,
    persistence: &ControlledPersistence,
) -> seekdeep_cordis::fiber::EffectHandle {
    let erased: Arc<dyn SessionPersistence> = Arc::new(persistence.clone());
    SessionPersistenceService::new(erased)
        .provide(context)
        .expect("provide persistence")
}

fn query_code(error: &anyhow::Error) -> SessionQueryErrorCode {
    error
        .downcast_ref::<SessionQueryError>()
        .expect("typed query error")
        .code
}

#[tokio::test]
async fn dynamic_persistence_shadows_reveals_hides_and_skips_shadowed_inspection() {
    let context = seekdeep_cordis::Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let persistence = ControlledPersistence::default();
    let durable_header = header("shared", 10);
    persistence.set(durable_header.clone(), message_events("persisted needle"));
    let mounted = mount(&context, &persistence);
    let live = sessions
        .prepare(
            Some(durable_header.id.clone()),
            CreateSessionOptions {
                seed: Some(message_events("live needle")),
                created_at: Some(10),
                ..CreateSessionOptions::default()
            },
        )
        .expect("prepare live");
    let detach = sessions.enter(&live).expect("enter");
    sessions.announce(&live).expect("announce");
    let engine = SqliteSessionQueryEngine::new(&context, config(":memory:")).expect("engine");

    let live_page = engine
        .search_sessions(search("live", None), None)
        .await
        .expect("live search");
    assert_eq!(live_page.items.len(), 1);
    assert!(live_page.items[0].record.live);
    assert!(live_page.items[0].record.persisted);
    assert_eq!(persistence.inspection_count(&durable_header.id), 0);
    assert!(
        engine
            .search_sessions(search("persisted", None), None)
            .await
            .expect("shadowed search")
            .items
            .is_empty()
    );

    detach.dispose().await.expect("detach");
    let revealed = engine
        .search_sessions(search("persisted", None), None)
        .await
        .expect("revealed search");
    assert_eq!(revealed.items.len(), 1);
    assert!(!revealed.items[0].record.live);
    assert_eq!(persistence.inspection_count(&durable_header.id), 1);

    mounted.dispose().await.expect("unmount");
    assert!(
        engine
            .search_sessions(search("persisted", None), None)
            .await
            .expect("hidden search")
            .items
            .is_empty()
    );
}

#[tokio::test]
async fn stale_binding_failures_retry_and_colliding_revisions_reload_replacements() {
    let context = seekdeep_cordis::Context::new();
    SessionStore::install(&context).expect("sessions");
    let persistence = ControlledPersistence::default();
    let durable = header("replacement", 10);
    persistence.set_with_revision(durable.clone(), message_events("old content"), 7);
    let mounted = mount(&context, &persistence);
    let engine = SqliteSessionQueryEngine::new(&context, config(":memory:")).expect("engine");
    assert_eq!(
        engine
            .search_sessions(search("old", None), None)
            .await
            .expect("old search")
            .items
            .len(),
        1
    );
    mounted.dispose().await.expect("unmount old");
    persistence.set_with_revision(durable.clone(), message_events("new needle"), 7);
    let replacement = mount(&context, &persistence);
    assert_eq!(
        engine
            .search_sessions(search("new", None), None)
            .await
            .expect("replacement search")
            .items
            .len(),
        1
    );
    assert!(
        engine
            .search_sessions(search("old", None), None)
            .await
            .expect("old removed")
            .items
            .is_empty()
    );
    assert_eq!(persistence.inspection_count(&durable.id), 2);

    let gate = persistence.gate_next_list();
    let pending_engine = engine.clone();
    let pending = tokio::spawn(async move {
        pending_engine
            .search_sessions(search("new", None), None)
            .await
    });
    gate.wait_started().await;
    replacement.dispose().await.expect("unmount racing source");
    persistence.set_failure(Some(Failure::Plain("stale source failure".to_owned())));
    gate.release();
    assert!(
        pending
            .await
            .expect("join")
            .expect("stale failure discarded")
            .items
            .is_empty()
    );
}

#[tokio::test]
async fn snapshot_churn_fails_after_one_retry_and_typed_failures_are_preserved() {
    let context = seekdeep_cordis::Context::new();
    SessionStore::install(&context).expect("sessions");
    let persistence = ControlledPersistence::default();
    let durable = header("churn", 1);
    persistence.set(durable.clone(), message_events("durable needle"));
    let _mounted = mount(&context, &persistence);
    let engine = SqliteSessionQueryEngine::new(&context, config(":memory:")).expect("engine");
    let mutating = persistence.clone();
    let id = durable.id.clone();
    persistence.set_snapshot_hook(Some(Arc::new(move |_| mutating.bump_revision(&id))));
    let error = engine
        .search_sessions(search("needle", None), None)
        .await
        .expect_err("continuous churn");
    assert_eq!(
        query_code(&error),
        SessionQueryErrorCode::SessionQueryPersistenceFailed
    );
    assert_eq!(persistence.snapshot_count(), 4);

    persistence.set_snapshot_hook(None);
    let typed = SessionQueryError::new(
        "typed persistence failure",
        SessionQueryErrorCode::SessionQueryPersistenceFailed,
    );
    persistence.set_failure(Some(Failure::Query(typed.clone())));
    let error = engine
        .search_sessions(search("needle", None), None)
        .await
        .expect_err("typed failure");
    assert_eq!(error.downcast_ref::<SessionQueryError>(), Some(&typed));
}

#[tokio::test]
async fn immutable_live_and_persisted_header_conflicts_remain_typed() {
    let context = seekdeep_cordis::Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let persistence = ControlledPersistence::default();
    let durable = header("conflict", 10);
    persistence.set(durable.clone(), message_events("persisted needle"));
    let _mounted = mount(&context, &persistence);
    sessions
        .create(
            &context,
            Some(durable.id),
            CreateSessionOptions {
                seed: Some(message_events("live needle")),
                created_at: Some(11),
                ..CreateSessionOptions::default()
            },
        )
        .expect("live");
    let engine = SqliteSessionQueryEngine::new(&context, config(":memory:")).expect("engine");
    let error = engine
        .search_sessions(search("needle", None), None)
        .await
        .expect_err("header conflict");
    assert_eq!(
        query_code(&error),
        SessionQueryErrorCode::SessionQuerySourceConflict
    );
}

#[tokio::test]
async fn queued_and_inflight_cancellation_and_close_obey_serialized_fences() {
    let context = seekdeep_cordis::Context::new();
    SessionStore::install(&context).expect("sessions");
    let persistence = ControlledPersistence::default();
    let _mounted = mount(&context, &persistence);
    let engine = SqliteSessionQueryEngine::new(&context, config(":memory:")).expect("engine");

    let active_gate = persistence.gate_next_list();
    let active_signal = AbortSignal::default();
    let active_engine = engine.clone();
    let active_exec = active_signal.clone();
    let active = tokio::spawn(async move {
        active_engine
            .search_sessions(
                search("needle", None),
                Some(SessionSearchExecContext {
                    signal: Some(active_exec),
                }),
            )
            .await
    });
    active_gate.wait_started().await;
    active_signal.abort();
    tokio::task::yield_now().await;
    assert!(!active.is_finished());

    let queued_signal = AbortSignal::default();
    let queued_engine = engine.clone();
    let queued_exec = queued_signal.clone();
    let queued = tokio::spawn(async move {
        queued_engine
            .search_sessions(
                search("needle", None),
                Some(SessionSearchExecContext {
                    signal: Some(queued_exec),
                }),
            )
            .await
    });
    queued_signal.abort();
    let queued_error = queued
        .await
        .expect("queued join")
        .expect_err("queued abort");
    assert_eq!(
        query_code(&queued_error),
        SessionQueryErrorCode::SessionQueryAborted
    );
    active_gate.release();
    let active_error = active
        .await
        .expect("active join")
        .expect_err("active abort");
    assert_eq!(
        query_code(&active_error),
        SessionQueryErrorCode::SessionQueryAborted
    );

    let close_gate = persistence.gate_next_list();
    let accepted_engine = engine.clone();
    let accepted = tokio::spawn(async move {
        accepted_engine
            .search_sessions(search("needle", None), None)
            .await
    });
    close_gate.wait_started().await;
    let closing_engine = engine.clone();
    let closing = tokio::spawn(async move { closing_engine.close().await });
    tokio::task::yield_now().await;
    assert!(!closing.is_finished());
    let future_error = engine
        .search_sessions(search("needle", None), None)
        .await
        .expect_err("future work rejected");
    assert_eq!(
        query_code(&future_error),
        SessionQueryErrorCode::SessionQueryIndexFailed
    );
    close_gate.release();
    accepted
        .await
        .expect("accepted join")
        .expect("accepted search");
    closing.await.expect("closing join");
}

#[tokio::test]
async fn first_search_open_failures_are_cached_after_the_filesystem_is_repaired() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("foreign.db");
    {
        let connection = rusqlite::Connection::open(&path).expect("foreign open");
        connection
            .execute("CREATE TABLE canonical(value TEXT)", [])
            .expect("foreign table");
    }
    let context = seekdeep_cordis::Context::new();
    SessionStore::install(&context).expect("sessions");
    let engine =
        SqliteSessionQueryEngine::new(&context, config(path.to_string_lossy().into_owned()))
            .expect("lazy engine");
    let first = engine
        .search_sessions(search("needle", None), None)
        .await
        .expect_err("foreign database");
    assert_eq!(
        query_code(&first),
        SessionQueryErrorCode::SessionQueryIndexFailed
    );
    std::fs::remove_file(&path).expect("remove temporary foreign database");
    let second = engine
        .search_sessions(search("needle", None), None)
        .await
        .expect_err("cached open failure");
    assert_eq!(second.to_string(), first.to_string());
    assert!(!path.exists());
}

#[tokio::test]
async fn sqlite_comparisons_preserve_fractional_and_pre_epoch_time_bounds() {
    let context = seekdeep_cordis::Context::new();
    SessionStore::install(&context).expect("sessions");
    let persistence = ControlledPersistence::default();
    let durable = header("fractional", 1);
    persistence.set(
        durable.clone(),
        vec![
            message_event("fractional needle", 0, 123),
            message_event("fractional needle", 1, 124),
            message_event("pre epoch needle", 2, -124),
            message_event("pre epoch needle", 3, -123),
        ],
    );
    let _mounted = mount(&context, &persistence);
    let engine = SqliteSessionQueryEngine::new(&context, config(":memory:")).expect("engine");
    let run = |query: &str, from: Option<f64>, to: Option<f64>| SessionEventSearchRequest {
        session_id: durable.id.clone(),
        query: query.to_owned(),
        filters: Some(vec![SessionEventMetadataFilter::Time {
            from: from.map(|value| SessionResultBound::new(value).expect("finite")),
            to: to.map(|value| SessionResultBound::new(value).expect("finite")),
        }]),
        limit: Some(5),
        cursor: None,
    };

    let lower = engine
        .search_events(run("fractional needle", Some(123.000_01), None), None)
        .await
        .expect("fractional lower");
    assert_eq!(
        lower
            .page
            .items
            .iter()
            .map(|hit| hit.record.seq)
            .collect::<Vec<_>>(),
        [1]
    );
    let upper = engine
        .search_events(run("fractional needle", None, Some(123.999_9)), None)
        .await
        .expect("fractional upper");
    assert_eq!(
        upper
            .page
            .items
            .iter()
            .map(|hit| hit.record.seq)
            .collect::<Vec<_>>(),
        [0]
    );
    let pre_lower = engine
        .search_events(run("pre epoch needle", Some(-123.999_99), None), None)
        .await
        .expect("pre-epoch lower");
    assert_eq!(
        pre_lower
            .page
            .items
            .iter()
            .map(|hit| hit.record.seq)
            .collect::<Vec<_>>(),
        [3]
    );
    let pre_upper = engine
        .search_events(run("pre epoch needle", None, Some(-123.000_1)), None)
        .await
        .expect("pre-epoch upper");
    assert_eq!(
        pre_upper
            .page
            .items
            .iter()
            .map(|hit| hit.record.seq)
            .collect::<Vec<_>>(),
        [2]
    );
}

#[tokio::test]
async fn live_attachment_and_snapshot_population_changes_retry_one_stable_observation() {
    let context = seekdeep_cordis::Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let persistence = ControlledPersistence::default();
    let durable = header("attach-race", 10);
    persistence.set(durable.clone(), message_events("persisted needle"));
    let _mounted = mount(&context, &persistence);
    let attached = Arc::new(AtomicBool::new(false));
    let hook_attached = attached.clone();
    let hook_sessions = sessions.clone();
    let hook_context = context.clone();
    let hook_header = durable.clone();
    persistence.set_snapshot_hook(Some(Arc::new(move |_| {
        if !hook_attached.swap(true, Ordering::AcqRel) {
            hook_sessions
                .create(
                    &hook_context,
                    Some(hook_header.id.clone()),
                    CreateSessionOptions {
                        seed: Some(message_events("live needle")),
                        created_at: Some(hook_header.created_at),
                        ..CreateSessionOptions::default()
                    },
                )
                .expect("attach live during observation");
        }
    })));
    let engine = SqliteSessionQueryEngine::new(&context, config(":memory:")).expect("engine");
    let page = engine
        .search_sessions(search("live", None), None)
        .await
        .expect("retried live observation");
    assert_eq!(page.items.len(), 1);
    assert!(page.items[0].record.live);
    assert_eq!(persistence.inspection_count(&durable.id), 0);

    let population_context = seekdeep_cordis::Context::new();
    SessionStore::install(&population_context).expect("sessions");
    let population = ControlledPersistence::default();
    let first = header("first", 1);
    let added = header("added", 2);
    population.set(first.clone(), message_events("first needle"));
    let _population_mount = mount(&population_context, &population);
    let add_once = Arc::new(AtomicBool::new(false));
    let adding = add_once.clone();
    let adding_store = population.clone();
    let adding_header = added.clone();
    population.set_snapshot_hook(Some(Arc::new(move |_| {
        if !adding.swap(true, Ordering::AcqRel) {
            adding_store.set(adding_header.clone(), message_events("added needle"));
        }
    })));
    let population_engine = SqliteSessionQueryEngine::new(&population_context, config(":memory:"))
        .expect("population engine");
    let mut population_search = search("needle", None);
    population_search.limit = Some(5);
    let page = population_engine
        .search_sessions(population_search, None)
        .await
        .expect("population retry");
    assert_eq!(page.items.len(), 2);
    assert_eq!(population.inspection_count(&first.id), 2);
    assert_eq!(population.inspection_count(&added.id), 1);
}

#[tokio::test]
async fn failed_transactions_roll_back_and_the_next_search_recovers() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("derived.db");
    let context = seekdeep_cordis::Context::new();
    SessionStore::install(&context).expect("sessions");
    let persistence = ControlledPersistence::default();
    let durable = header("transaction", 1);
    persistence.set(durable.clone(), message_events("base content"));
    let _mounted = mount(&context, &persistence);
    let engine =
        SqliteSessionQueryEngine::new(&context, config(path.to_string_lossy().into_owned()))
            .expect("engine");
    assert_eq!(
        engine
            .search_sessions(search("base", None), None)
            .await
            .expect("baseline")
            .items
            .len(),
        1
    );
    let external = rusqlite::Connection::open(&path).expect("external connection");
    external
        .execute_batch(
            "CREATE TRIGGER fail_persisted_insert BEFORE INSERT ON persisted_sessions BEGIN SELECT RAISE(FAIL, 'forced reconciliation failure'); END;",
        )
        .expect("failure trigger");
    persistence.set(durable, message_events("retry needle"));
    let error = engine
        .search_sessions(search("retry", None), None)
        .await
        .expect_err("forced transaction failure");
    assert_eq!(
        query_code(&error),
        SessionQueryErrorCode::SessionQueryIndexFailed
    );
    external
        .execute("DROP TRIGGER fail_persisted_insert", [])
        .expect("drop trigger");
    assert_eq!(
        engine
            .search_sessions(search("retry", None), None)
            .await
            .expect("recovered search")
            .items
            .len(),
        1
    );
}

#[tokio::test]
async fn unchanged_persisted_rows_keep_generations_across_reopen() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("derived.db");
    let context = seekdeep_cordis::Context::new();
    SessionStore::install(&context).expect("sessions");
    let persistence = ControlledPersistence::default();
    let unchanged = header("unchanged", 1);
    let changed = header("changed", 1);
    let deleted = header("deleted", 1);
    persistence.set(unchanged.clone(), message_events("unchanged needle"));
    persistence.set(changed.clone(), message_events("old needle"));
    persistence.set(deleted.clone(), message_events("deleted needle"));
    let _mounted = mount(&context, &persistence);
    let first =
        SqliteSessionQueryEngine::new(&context, config(path.to_string_lossy().into_owned()))
            .expect("first engine");
    first
        .search_sessions(search("needle", None), None)
        .await
        .expect("first reconciliation");
    first.close().await;
    let before = persisted_generations(&path);

    persistence.set(changed.clone(), message_events("changed needle"));
    persistence.remove(&deleted.id);
    let added = header("added", 1);
    persistence.set(added.clone(), message_events("added needle"));
    let second =
        SqliteSessionQueryEngine::new(&context, config(path.to_string_lossy().into_owned()))
            .expect("second engine");
    let mut all_sessions = search("needle", None);
    all_sessions.limit = Some(5);
    let page = second
        .search_sessions(all_sessions, None)
        .await
        .expect("second reconciliation");
    assert_eq!(page.items.len(), 3);
    let after = persisted_generations(&path);
    assert_eq!(after.get(&unchanged.id), before.get(&unchanged.id));
    assert!(after[&changed.id] > before[&changed.id]);
    assert!(!after.contains_key(&deleted.id));
    assert!(after.contains_key(&added.id));
    assert_eq!(persistence.inspection_count(&unchanged.id), 1);
    assert_eq!(persistence.inspection_count(&changed.id), 2);
}

fn persisted_generations(path: &std::path::Path) -> HashMap<SessionId, i64> {
    let connection = rusqlite::Connection::open(path).expect("generation connection");
    let mut statement = connection
        .prepare("SELECT id, generation FROM persisted_sessions")
        .expect("generation query");
    statement
        .query_map([], |row| {
            Ok((SessionId::new(row.get::<_, String>(0)?), row.get(1)?))
        })
        .expect("generation rows")
        .collect::<Result<HashMap<_, _>, _>>()
        .expect("generation values")
}
