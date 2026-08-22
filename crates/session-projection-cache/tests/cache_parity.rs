//! Behavioral mirror of `packages/session/session-projection-cache/tests/cache.spec.ts`.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::{FutureExt as _, future::BoxFuture};
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionEvent, SessionHeader, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::AbortSignal;
use seekdeep_session_persistence::{
    SessionInspection, SessionLocation, SessionPersistence, SessionPersistenceRevision,
    SessionPersistenceService, SessionPersistenceSnapshot,
};
use seekdeep_session_projection::{
    ProjectionDefinition, ProjectionTransition, SessionProjectionRegistry,
};
use seekdeep_session_projection_cache::{
    CheckpointIdentity, CheckpointRecord, Config, SESSION_PROJECTION_CACHE, SessionProjectionCache,
    config_schema, plugin,
};
use seekdeep_storage::{
    BackendRegistration, FormMount, KvFacet, KvSnapshot, KvUnit, KvUnitDescriptor, Storage,
    StorageBackend, StorageError, StorageErrorCode,
};
use seekdeep_storage_domain::{DomainConfig, DomainFacility};
use serde_json::{Map, Value, json};

#[derive(Clone, Debug)]
struct Medium {
    version: u64,
    tables: IndexMap<String, Map<String, Value>>,
    global: Value,
}

#[derive(Debug, Default)]
struct PoolState {
    media: HashMap<String, Medium>,
    open: HashSet<String>,
}

#[derive(Debug, Default)]
struct MemoryPool {
    state: Mutex<PoolState>,
    fail_next_writes: AtomicUsize,
}

impl MemoryPool {
    fn fail_writes(&self, count: usize) {
        self.fail_next_writes.store(count, Ordering::Release);
    }

    fn maybe_fail(&self) -> Result<(), StorageError> {
        let remaining = self.fail_next_writes.load(Ordering::Acquire);
        if remaining == 0 {
            return Ok(());
        }
        self.fail_next_writes.fetch_sub(1, Ordering::AcqRel);
        Err(StorageError::new(
            StorageErrorCode::MalformedMedium,
            "injected write failure",
        ))
    }

    fn record(&self, id: &SessionId) -> Option<CheckpointRecord> {
        self.state
            .lock()
            .media
            .get("session_projcache")?
            .tables
            .get("sessions")?
            .get(id.as_str())
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
    }

    fn seed(&self, id: &str, record: CheckpointRecord) {
        let mut state = self.state.lock();
        let medium = state
            .media
            .entry("session_projcache".to_owned())
            .or_insert_with(|| Medium {
                version: 3,
                tables: IndexMap::from([("sessions".to_owned(), Map::new())]),
                global: Value::Null,
            });
        medium
            .tables
            .get_mut("sessions")
            .expect("sessions table")
            .insert(id.to_owned(), serde_json::to_value(record).expect("record"));
    }
}

#[derive(Debug)]
struct MemoryBackend {
    pool: Arc<MemoryPool>,
    closed: AtomicBool,
}

impl MemoryBackend {
    fn new(pool: Arc<MemoryPool>) -> Arc<Self> {
        Arc::new(Self {
            pool,
            closed: AtomicBool::new(false),
        })
    }
}

impl StorageBackend for MemoryBackend {
    fn kv(&self) -> Option<Arc<dyn KvFacet>> {
        Some(Arc::new(MemoryFacet {
            pool: self.pool.clone(),
        }))
    }

    fn close(&self) -> BoxFuture<'static, Result<(), StorageError>> {
        self.closed.store(true, Ordering::Release);
        async { Ok(()) }.boxed()
    }
}

#[derive(Debug)]
struct MemoryFacet {
    pool: Arc<MemoryPool>,
}

impl KvFacet for MemoryFacet {
    fn open(
        &self,
        descriptor: KvUnitDescriptor,
    ) -> BoxFuture<'static, anyhow::Result<Arc<dyn KvUnit>>> {
        let pool = self.pool.clone();
        async move {
            descriptor.validate()?;
            let mut state = pool.state.lock();
            if !state.open.insert(descriptor.name.clone()) {
                return Err(StorageError::new(
                    StorageErrorCode::MalformedMedium,
                    format!("unit '{}' is already open", descriptor.name),
                )
                .into());
            }
            if let Some(medium) = state.media.get(&descriptor.name) {
                if medium.version != descriptor.version {
                    state.open.remove(&descriptor.name);
                    return Err(StorageError::new(
                        StorageErrorCode::VersionMismatch,
                        format!("unit '{}' has another version", descriptor.name),
                    )
                    .into());
                }
            } else {
                state.media.insert(
                    descriptor.name.clone(),
                    Medium {
                        version: descriptor.version,
                        tables: descriptor
                            .tables
                            .iter()
                            .map(|table| (table.clone(), Map::new()))
                            .collect(),
                        global: Value::Null,
                    },
                );
            }
            drop(state);
            Ok(Arc::new(MemoryUnit {
                pool,
                descriptor,
                closed: AtomicBool::new(false),
            }) as Arc<dyn KvUnit>)
        }
        .boxed()
    }
}

#[derive(Debug)]
struct MemoryUnit {
    pool: Arc<MemoryPool>,
    descriptor: KvUnitDescriptor,
    closed: AtomicBool,
}

impl MemoryUnit {
    fn check(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.closed.load(Ordering::Acquire),
            "unit '{}' is closed",
            self.descriptor.name
        );
        Ok(())
    }
}

impl KvUnit for MemoryUnit {
    fn load_all(&self) -> BoxFuture<'static, anyhow::Result<KvSnapshot>> {
        let checked = self.check();
        let pool = self.pool.clone();
        let name = self.descriptor.name.clone();
        async move {
            checked?;
            let state = pool.state.lock();
            let medium = state.media.get(&name).expect("medium");
            Ok(KvSnapshot {
                tables: medium.tables.clone(),
                global: medium.global.clone(),
            })
        }
        .boxed()
    }

    fn put_record(
        &self,
        table: String,
        key: String,
        value: Value,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        let checked = self.check();
        let pool = self.pool.clone();
        let name = self.descriptor.name.clone();
        async move {
            checked?;
            pool.maybe_fail()?;
            pool.state
                .lock()
                .media
                .get_mut(&name)
                .expect("medium")
                .tables
                .get_mut(&table)
                .expect("table")
                .insert(key, value);
            Ok(())
        }
        .boxed()
    }

    fn delete_record(&self, table: String, key: String) -> BoxFuture<'static, anyhow::Result<()>> {
        let checked = self.check();
        let pool = self.pool.clone();
        let name = self.descriptor.name.clone();
        async move {
            checked?;
            pool.maybe_fail()?;
            pool.state
                .lock()
                .media
                .get_mut(&name)
                .expect("medium")
                .tables
                .get_mut(&table)
                .expect("table")
                .remove(&key);
            Ok(())
        }
        .boxed()
    }

    fn set_global(&self, value: Value) -> BoxFuture<'static, anyhow::Result<()>> {
        let checked = self.check();
        let pool = self.pool.clone();
        let name = self.descriptor.name.clone();
        async move {
            checked?;
            pool.maybe_fail()?;
            pool.state
                .lock()
                .media
                .get_mut(&name)
                .expect("medium")
                .global = value;
            Ok(())
        }
        .boxed()
    }

    fn close(&self) -> BoxFuture<'static, Result<(), StorageError>> {
        let first = !self.closed.swap(true, Ordering::AcqRel);
        let pool = self.pool.clone();
        let name = self.descriptor.name.clone();
        async move {
            if first {
                pool.state.lock().open.remove(&name);
            }
            Ok(())
        }
        .boxed()
    }
}

#[derive(Debug, Default)]
struct FakePersistence {
    logs: Mutex<HashMap<String, SessionInspection>>,
    reads: Mutex<Vec<(String, u64)>>,
}

impl FakePersistence {
    fn set_log(&self, id: &str, events: Vec<SessionEvent>) {
        let mut header = SessionHeader::new(SessionId::new(id));
        header.created_at = 0;
        self.logs.lock().insert(
            id.to_owned(),
            SessionInspection {
                meta: header,
                events,
            },
        );
    }

    fn reads(&self) -> Vec<(String, u64)> {
        self.reads.lock().clone()
    }

    fn inspection(&self, id: &SessionId) -> anyhow::Result<SessionInspection> {
        self.logs
            .lock()
            .get(id.as_str())
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("session {id:?} not found"))
    }
}

#[async_trait]
impl SessionPersistence for FakePersistence {
    fn locate(&self, _meta: &SessionHeader) -> Option<SessionLocation> {
        None
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
        self.inspection(id)
    }

    async fn inspect(
        &self,
        id: &SessionId,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        self.inspection(id)
    }

    async fn read_from(
        &self,
        id: &SessionId,
        from_seq: u64,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        self.reads.lock().push((id.as_str().to_owned(), from_seq));
        let mut inspection = self.inspection(id)?;
        inspection.events.retain(|event| event.seq >= from_seq);
        Ok(inspection)
    }

    async fn list(&self, _signal: Option<AbortSignal>) -> anyhow::Result<Vec<SessionHeader>> {
        Ok(self
            .logs
            .lock()
            .values()
            .map(|inspection| inspection.meta.clone())
            .collect())
    }

    async fn list_snapshots(
        &self,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SessionPersistenceSnapshot>> {
        Ok(self
            .logs
            .lock()
            .values()
            .map(|inspection| SessionPersistenceSnapshot {
                header: inspection.meta.clone(),
                revision: SessionPersistenceRevision::new(format!(
                    "test:{}",
                    inspection.events.len()
                )),
            })
            .collect())
    }
}

struct Harness {
    context: Context,
    pool: Arc<MemoryPool>,
    sessions: Arc<SessionStore>,
    cache: Arc<SessionProjectionCache>,
    cache_fiber: Arc<seekdeep_cordis::PluginFiber>,
    _storage: Arc<Storage>,
    _backend: BackendRegistration,
    _facility: Arc<DomainFacility>,
    _mount: FormMount,
}

impl Harness {
    async fn new(
        pool: Arc<MemoryPool>,
        persistence: Arc<FakePersistence>,
        config: Config,
        state_version: Option<u64>,
    ) -> anyhow::Result<Self> {
        let context = Context::new();
        let storage = Storage::new();
        storage.provide(&context)?;
        let backend = storage
            .backend
            .register("memory", MemoryBackend::new(pool.clone()))?;
        let facility = DomainFacility::new(
            context.clone(),
            storage.clone(),
            DomainConfig {
                backend: "memory".to_owned(),
                routes: HashMap::new(),
            },
        );
        let (_, mount) = facility.mount(&context)?;
        let sessions = SessionStore::install(&context)?;
        let projections = SessionProjectionRegistry::install(&context)?;
        if let Some(version) = state_version {
            projections.register(&context, marks_projection(version))?;
        }
        SessionPersistenceService::new(persistence.clone()).provide(&context)?;
        let cache_fiber = context.plugin(plugin(), serde_json::to_value(config)?)?;
        cache_fiber.await_settled().await?;
        let cache = context
            .get(SESSION_PROJECTION_CACHE)
            .ok_or_else(|| anyhow::anyhow!("cache plugin did not publish service"))?;
        Ok(Self {
            context,
            pool,
            sessions,
            cache,
            cache_fiber,
            _storage: storage,
            _backend: backend,
            _facility: facility,
            _mount: mount,
        })
    }

    async fn dispose(self) -> anyhow::Result<()> {
        self.context.fiber().dispose().await
    }
}

fn marks_projection(state_version: u64) -> ProjectionDefinition {
    ProjectionDefinition::new(
        "cache-test/marks",
        state_version,
        || Ok(Value::Null),
        |state, event| {
            if event.event_type == "cache-test/mark" {
                Ok(ProjectionTransition::Changed(event.data.clone()))
            } else {
                let _ = state;
                Ok(ProjectionTransition::Unchanged)
            }
        },
        |state| {
            Ok(if state.is_null() {
                json!({"marks": []})
            } else {
                state.clone()
            })
        },
    )
}

fn session(harness: &Harness, id: &str) -> Arc<Session> {
    harness
        .sessions
        .create(
            &harness.context,
            Some(SessionId::new(id)),
            CreateSessionOptions::default(),
        )
        .expect("session")
}

fn mark(session: &Arc<Session>, marks: &[&str]) -> SessionEvent {
    session
        .append(
            "cache-test/mark",
            json!({"marks": marks}),
            AppendOptions::default(),
        )
        .expect("mark")
}

fn end_turn(session: &Arc<Session>) -> SessionEvent {
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("turn end")
}

async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

fn row(
    pool: &MemoryPool,
    id: &SessionId,
) -> Option<seekdeep_session_projection::ProjectionCheckpointRow> {
    pool.record(id)?.rows.get("cache-test/marks").cloned()
}

fn stored_log(marks: &[&[&str]]) -> Vec<SessionEvent> {
    let mut events = vec![SessionEvent {
        event_type: "turn/start".to_owned(),
        seq: 0,
        time: 0,
        data: json!({"turn": 1}),
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    }];
    for values in marks {
        let seq = u64::try_from(events.len()).expect("event count");
        events.push(SessionEvent {
            event_type: "cache-test/mark".to_owned(),
            seq,
            time: i64::try_from(seq).expect("time"),
            data: json!({"marks": values}),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        });
    }
    let seq = u64::try_from(events.len()).expect("event count");
    events.push(SessionEvent {
        event_type: "turn/end".to_owned(),
        seq,
        time: i64::try_from(seq).expect("time"),
        data: json!({"turn": 1, "reason": {"kind": "completed"}}),
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    });
    events
}

fn seed_record(
    pool: &MemoryPool,
    id: &str,
    version: u64,
    seq: i64,
    value: Value,
    identity: CheckpointIdentity,
) {
    pool.seed(
        id,
        CheckpointRecord {
            identity,
            rows: IndexMap::from([(
                "cache-test/marks".to_owned(),
                seekdeep_session_projection::ProjectionCheckpointRow {
                    ver: version,
                    seq,
                    val: value,
                },
            )]),
        },
    );
}

fn default_config() -> Config {
    Config {
        write_every_events: 100,
        write_interval_ms: 60_000,
    }
}

#[tokio::test]
async fn loader_schema_service_seat_and_direct_validation_are_exact() -> anyhow::Result<()> {
    assert_eq!(plugin().name(), "session-projection-cache");
    assert_eq!(
        plugin().inject(),
        [
            "storageDomain",
            "sessionProjections",
            "sessionPersistence",
            "sessions"
        ]
    );
    for invalid in [
        json!({"writeEveryEvents": 0, "writeIntervalMs": 1}),
        json!({"writeEveryEvents": 1, "writeIntervalMs": 0}),
        json!({"writeEveryEvents": 1.5, "writeIntervalMs": 1}),
    ] {
        assert!(config_schema().resolve(&invalid).is_err(), "{invalid}");
    }
    let context = Context::new();
    let error = SessionProjectionCache::install(
        &context,
        Config {
            write_every_events: 0,
            write_interval_ms: 1,
        },
    )
    .await
    .expect_err("invalid config must fail before dependency lookup");
    assert!(error.to_string().contains("writeEveryEvents"));
    Ok(())
}

#[tokio::test]
async fn mandatory_count_and_direct_writes_land_exact_checkpoint_cuts() -> anyhow::Result<()> {
    let harness = Harness::new(
        Arc::new(MemoryPool::default()),
        Arc::new(FakePersistence::default()),
        Config {
            write_every_events: 3,
            write_interval_ms: 60_000,
        },
        Some(1),
    )
    .await?;
    assert_eq!(harness.cache.config().write_every_events, 3);

    let turn = session(&harness, "turn-end");
    mark(&turn, &["a"]);
    assert!(row(&harness.pool, turn.id()).is_none());
    let end = end_turn(&turn);
    settle().await;
    assert_eq!(
        row(&harness.pool, turn.id()),
        Some(seekdeep_session_projection::ProjectionCheckpointRow {
            ver: 1,
            seq: i64::try_from(end.seq).expect("seq"),
            val: json!({"marks": ["a"]}),
        })
    );

    let count = session(&harness, "count");
    mark(&count, &["1"]);
    mark(&count, &["2"]);
    settle().await;
    assert!(row(&harness.pool, count.id()).is_none());
    let third = mark(&count, &["3"]);
    settle().await;
    assert_eq!(
        row(&harness.pool, count.id()).expect("count row").seq,
        i64::try_from(third.seq).expect("seq")
    );

    let clean = session(&harness, "clean");
    harness.cache.write(&clean).await?;
    assert_eq!(
        row(&harness.pool, clean.id()),
        Some(seekdeep_session_projection::ProjectionCheckpointRow {
            ver: 1,
            seq: -1,
            val: Value::Null,
        })
    );
    harness.dispose().await
}

#[tokio::test(start_paused = true)]
async fn interval_writes_once_without_self_aborting_and_plugin_disposal_clears_timers()
-> anyhow::Result<()> {
    let harness = Harness::new(
        Arc::new(MemoryPool::default()),
        Arc::new(FakePersistence::default()),
        Config {
            write_every_events: 100,
            write_interval_ms: 250,
        },
        Some(1),
    )
    .await?;
    let interval = session(&harness, "interval");
    mark(&interval, &["slow"]);
    tokio::time::advance(Duration::from_millis(249)).await;
    settle().await;
    assert!(row(&harness.pool, interval.id()).is_none());
    tokio::time::advance(Duration::from_millis(1)).await;
    settle().await;
    assert_eq!(
        row(&harness.pool, interval.id()).expect("interval row").val,
        json!({"marks": ["slow"]})
    );

    let armed = session(&harness, "armed");
    mark(&armed, &["pending"]);
    harness.cache_fiber.dispose().await?;
    assert!(harness.context.get(SESSION_PROJECTION_CACHE).is_none());
    tokio::time::advance(Duration::from_secs(10)).await;
    settle().await;
    assert!(row(&harness.pool, armed.id()).is_none());
    harness.context.fiber().dispose().await
}

#[tokio::test]
async fn detach_writes_and_a_failed_mandatory_write_self_heals() -> anyhow::Result<()> {
    let harness = Harness::new(
        Arc::new(MemoryPool::default()),
        Arc::new(FakePersistence::default()),
        default_config(),
        Some(1),
    )
    .await?;
    let owner = seekdeep_cordis::Fiber::active_child("session owner");
    let owner_context = harness.context.with_fiber(owner.clone());
    let detached = harness.sessions.create(
        &owner_context,
        Some(SessionId::new("detach")),
        CreateSessionOptions::default(),
    )?;
    mark(&detached, &["live"]);
    owner.dispose().await?;
    settle().await;
    assert_eq!(
        row(&harness.pool, detached.id()).expect("detach row").val,
        json!({"marks": ["live"]})
    );

    let soft = session(&harness, "fail-soft");
    mark(&soft, &["x"]);
    harness.pool.fail_writes(1);
    end_turn(&soft);
    settle().await;
    assert!(row(&harness.pool, soft.id()).is_none());
    mark(&soft, &["y"]);
    end_turn(&soft);
    settle().await;
    assert_eq!(
        row(&harness.pool, soft.id()).expect("healed row").val,
        json!({"marks": ["y"]})
    );
    harness.dispose().await
}

#[tokio::test]
async fn cold_read_uses_tail_then_writes_the_served_cut_back() -> anyhow::Result<()> {
    let pool = Arc::new(MemoryPool::default());
    seed_record(
        &pool,
        "cold",
        1,
        1,
        json!({"marks": ["a"]}),
        CheckpointIdentity {
            created_at: 0,
            cwd: None,
        },
    );
    let persistence = Arc::new(FakePersistence::default());
    persistence.set_log("cold", stored_log(&[&["a"], &["a", "b"]]));
    let harness = Harness::new(pool, persistence.clone(), default_config(), Some(1)).await?;
    let snapshot = harness
        .cache
        .cold_snapshot(&SessionId::new("cold"), None)
        .await?;
    assert_eq!(
        snapshot.values["cache-test/marks"],
        json!({"marks": ["a", "b"]})
    );
    assert_eq!(snapshot.as_of_seq, 3);
    assert_eq!(persistence.reads(), [("cold".to_owned(), 1)]);
    assert_eq!(
        row(&harness.pool, &SessionId::new("cold"))
            .expect("writeback")
            .seq,
        3
    );
    harness.dispose().await
}

#[tokio::test]
async fn cold_read_refolds_version_mismatch_and_a_shrunk_log() -> anyhow::Result<()> {
    let bumped_pool = Arc::new(MemoryPool::default());
    seed_record(
        &bumped_pool,
        "bumped",
        1,
        2,
        json!({"marks": ["stale"]}),
        CheckpointIdentity {
            created_at: 0,
            cwd: None,
        },
    );
    let bumped_persistence = Arc::new(FakePersistence::default());
    bumped_persistence.set_log("bumped", stored_log(&[&["a"]]));
    let bumped = Harness::new(
        bumped_pool,
        bumped_persistence.clone(),
        default_config(),
        Some(2),
    )
    .await?;
    assert_eq!(
        bumped
            .cache
            .cold_snapshot(&SessionId::new("bumped"), None)
            .await?
            .values["cache-test/marks"],
        json!({"marks": ["a"]})
    );
    assert_eq!(bumped_persistence.reads(), [("bumped".to_owned(), 0)]);
    bumped.dispose().await?;

    let shrunk_pool = Arc::new(MemoryPool::default());
    seed_record(
        &shrunk_pool,
        "shrunk",
        1,
        9,
        json!({"marks": ["ghost"]}),
        CheckpointIdentity {
            created_at: 0,
            cwd: None,
        },
    );
    let shrunk_persistence = Arc::new(FakePersistence::default());
    shrunk_persistence.set_log("shrunk", stored_log(&[&["a"]]));
    let shrunk = Harness::new(
        shrunk_pool,
        shrunk_persistence.clone(),
        default_config(),
        Some(1),
    )
    .await?;
    assert_eq!(
        shrunk
            .cache
            .cold_snapshot(&SessionId::new("shrunk"), None)
            .await?
            .as_of_seq,
        2
    );
    assert_eq!(
        shrunk_persistence.reads(),
        [("shrunk".to_owned(), 9), ("shrunk".to_owned(), 0)]
    );
    shrunk.dispose().await
}

#[tokio::test]
async fn cold_read_discards_a_record_from_another_session_lifecycle() -> anyhow::Result<()> {
    let reborn_pool = Arc::new(MemoryPool::default());
    seed_record(
        &reborn_pool,
        "reborn",
        1,
        2,
        json!({"marks": ["phantom"]}),
        CheckpointIdentity {
            created_at: 999,
            cwd: None,
        },
    );
    let reborn_persistence = Arc::new(FakePersistence::default());
    reborn_persistence.set_log("reborn", stored_log(&[&["real"]]));
    let reborn = Harness::new(reborn_pool, reborn_persistence, default_config(), Some(1)).await?;
    assert_eq!(
        reborn
            .cache
            .cold_snapshot(&SessionId::new("reborn"), None)
            .await?
            .values["cache-test/marks"],
        json!({"marks": ["real"]})
    );
    assert_eq!(
        reborn
            .pool
            .record(&SessionId::new("reborn"))
            .expect("rebound")
            .identity
            .created_at,
        0
    );
    reborn.dispose().await
}

#[tokio::test]
async fn cached_snapshot_filters_versions_and_identity_and_zero_units_preserve_not_found()
-> anyhow::Result<()> {
    let pool = Arc::new(MemoryPool::default());
    seed_record(
        &pool,
        "listed",
        1,
        4,
        json!({"marks": ["t"]}),
        CheckpointIdentity {
            created_at: 0,
            cwd: Some("/work".to_owned()),
        },
    );
    let harness = Harness::new(
        pool,
        Arc::new(FakePersistence::default()),
        default_config(),
        Some(1),
    )
    .await?;
    let mut matching = SessionHeader::new(SessionId::new("listed"));
    matching.created_at = 0;
    matching.cwd = Some("/work".to_owned());
    assert_eq!(
        harness.cache.cached_snapshot(&matching),
        Some(seekdeep_session_projection::ProjectionSnapshot {
            as_of_seq: 4,
            values: IndexMap::from([("cache-test/marks".to_owned(), json!({"marks": ["t"]}),)]),
        })
    );
    matching.cwd = Some("/elsewhere".to_owned());
    assert!(harness.cache.cached_snapshot(&matching).is_none());
    harness.dispose().await?;

    let zero_persistence = Arc::new(FakePersistence::default());
    zero_persistence.set_log("bare", stored_log(&[&["a"]]));
    zero_persistence.set_log("empty", Vec::new());
    let zero = Harness::new(
        Arc::new(MemoryPool::default()),
        zero_persistence,
        default_config(),
        None,
    )
    .await?;
    assert!(
        zero.cache
            .cold_snapshot(&SessionId::new("absent"), None)
            .await
            .is_err()
    );
    assert_eq!(
        zero.cache
            .cold_snapshot(&SessionId::new("bare"), None)
            .await?,
        seekdeep_session_projection::ProjectionSnapshot {
            as_of_seq: 2,
            values: IndexMap::new(),
        }
    );
    assert_eq!(
        zero.cache
            .cold_snapshot(&SessionId::new("empty"), None)
            .await?
            .as_of_seq,
        -1
    );
    zero.dispose().await
}
