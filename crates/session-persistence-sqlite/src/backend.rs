//! Durable row storage and live-session orchestration.

use std::{
    collections::HashMap,
    fs::OpenOptions,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

use async_trait::async_trait;
use futures::{FutureExt as _, future::Shared};
use parking_lot::Mutex;
use rusqlite::{Connection, params};
use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply, Plugin, fiber::EffectHandle};
use seekdeep_core::{
    invariant::validate_session_events,
    preparation::SessionPreparation,
    repair::interrupted_turn_closers,
    session::{
        SESSION_FORMAT_VERSION, Session, SessionEvent, SessionHeader, SessionId, SessionOrigin,
    },
    session_store::{CreateSessionOptions, SESSIONS, SessionStore},
};
use seekdeep_llm::AbortSignal;
use seekdeep_session_persistence::{
    DEFAULT_PREPARED_SESSION_CACHE_SIZE, DEFAULT_WRITE_BATCH_MAX_DELAY_MS,
    MAX_WRITE_BATCH_DELAY_MS, SessionFormatUnsupportedError, SessionInspection, SessionPersistence,
    SessionPersistenceRevision, SessionPersistenceService, SessionPersistenceSnapshot,
    ensure_persistence_not_aborted,
    preparations::{
        DiscardReady, PreparedSource, SessionPreparationReservation, SessionPreparations,
    },
    session_format_version_refusal,
    stored_events::{assert_known_events, normalize_stored_events, validate_normalized_events},
    write_behind::SessionWriteBehind,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex as AsyncMutex, OnceCell, RwLock as AsyncRwLock};
use uuid::Uuid;

use crate::schema::{
    EventRow, JournalMode, SessionRow, open_database, row_to_meta, scan_rows, session_row,
    store_identity,
};

/// Cordis plugin name.
pub const NAME: &str = "session-persistence-sqlite";
/// Live sessions are required by the persistence coordinator.
pub const INJECT: &[&str] = &["sessions"];

/// `SQLite` persistence plugin configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SqliteConfig {
    /// Database path, or the special `:memory:` value.
    pub path: PathBuf,
    /// Durability-preserving journal mode.
    #[serde(default)]
    pub journal_mode: JournalMode,
    /// Maximum completed cold preparations retained for reuse.
    #[serde(default = "default_prepared_session_cache_size")]
    pub prepared_session_cache_size: usize,
    /// Fixed live-event write batching window.
    #[serde(default = "default_write_batch_max_delay_ms")]
    pub write_batch_max_delay_ms: u64,
}

const fn default_prepared_session_cache_size() -> usize {
    DEFAULT_PREPARED_SESSION_CACHE_SIZE
}

const fn default_write_batch_max_delay_ms() -> u64 {
    DEFAULT_WRITE_BATCH_MAX_DELAY_MS
}

impl SqliteConfig {
    /// Creates source-compatible defaults for one path.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            journal_mode: JournalMode::Wal,
            prepared_session_cache_size: DEFAULT_PREPARED_SESSION_CACHE_SIZE,
            write_batch_max_delay_ms: DEFAULT_WRITE_BATCH_MAX_DELAY_MS,
        }
    }
}

#[derive(Clone, Debug)]
struct BackendSession {
    header: SessionHeader,
    events: Vec<SessionEvent>,
    materialized: bool,
    owner: Option<usize>,
}

struct DatabaseState {
    connection: Mutex<Option<Connection>>,
    store_identity: String,
}

struct StoredPrefix {
    meta: SessionHeader,
    events: Vec<SessionEvent>,
    revision: SessionPersistenceRevision,
    torn_from: Option<u64>,
}

struct PreparedSqliteSource {
    inspection: SessionInspection,
    session: Arc<Session>,
    revision: SessionPersistenceRevision,
    session_length: usize,
    torn_from: Option<u64>,
    closers: Vec<SessionEvent>,
}

impl PreparedSource for PreparedSqliteSource {
    fn session(&self) -> &Arc<Session> {
        &self.session
    }
}

type InitFuture = Shared<futures::future::BoxFuture<'static, Result<(), Arc<String>>>>;

#[derive(Clone)]
struct LiveSessionState {
    init: InitFuture,
    writes: SessionWriteBehind,
}

#[derive(Clone)]
struct RetirementEntry {
    token: Uuid,
    owner: usize,
    session: Arc<Session>,
    settlement: InitFuture,
}

/// `SQLite` durable session persistence service.
pub struct SqliteSessionPersistence {
    path: PathBuf,
    journal_mode: JournalMode,
    sessions: Arc<SessionStore>,
    database: OnceCell<Result<DatabaseState, Arc<String>>>,
    state: Mutex<HashMap<SessionId, BackendSession>>,
    locks: Mutex<HashMap<SessionId, Arc<AsyncMutex<()>>>>,
    operation_gate: AsyncRwLock<()>,
    live: Mutex<HashMap<usize, LiveSessionState>>,
    retirements: Mutex<HashMap<SessionId, RetirementEntry>>,
    preparations: SessionPreparations<PreparedSqliteSource, BackendSession>,
    write_batch_max_delay: Duration,
    self_weak: std::sync::Weak<Self>,
}

impl std::fmt::Debug for SqliteSessionPersistence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SqliteSessionPersistence")
            .field("path", &self.path)
            .field("journal_mode", &self.journal_mode)
            .finish_non_exhaustive()
    }
}

impl SqliteSessionPersistence {
    /// Constructs and installs a standalone backend in the store context.
    ///
    /// # Errors
    ///
    /// Rejects invalid configuration or lifecycle installation.
    pub fn new(sessions: Arc<SessionStore>, config: SqliteConfig) -> anyhow::Result<Arc<Self>> {
        let context = sessions.context();
        let backend = Self::build(sessions, config)?;
        backend.install_write_path(&context)?;
        Ok(backend)
    }

    fn build(sessions: Arc<SessionStore>, config: SqliteConfig) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(!config.path.as_os_str().is_empty(), "path is required");
        anyhow::ensure!(
            config.prepared_session_cache_size > 0,
            "preparedSessionCacheSize must be a positive integer"
        );
        anyhow::ensure!(
            (1..=MAX_WRITE_BATCH_DELAY_MS).contains(&config.write_batch_max_delay_ms),
            "writeBatchMaxDelayMs must be an integer between 1 and 2147483647"
        );
        let path = if config.path == Path::new(":memory:") || config.path.is_absolute() {
            config.path
        } else {
            std::env::current_dir()?.join(config.path)
        };
        let preparations = SessionPreparations::new(config.prepared_session_cache_size)?;
        Ok(Arc::new_cyclic(|weak| Self {
            path,
            journal_mode: config.journal_mode,
            sessions,
            database: OnceCell::new(),
            state: Mutex::new(HashMap::new()),
            locks: Mutex::new(HashMap::new()),
            operation_gate: AsyncRwLock::new(()),
            live: Mutex::new(HashMap::new()),
            retirements: Mutex::new(HashMap::new()),
            preparations,
            write_batch_max_delay: Duration::from_millis(config.write_batch_max_delay_ms),
            self_weak: weak.clone(),
        }))
    }

    fn lock_for(&self, id: &SessionId) -> Arc<AsyncMutex<()>> {
        self.locks
            .lock()
            .entry(id.clone())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    async fn database(&self) -> anyhow::Result<&DatabaseState> {
        let path = self.path.clone();
        let journal_mode = self.journal_mode;
        self.database
            .get_or_init(|| async move {
                open_state(&path, journal_mode).map_err(|error| Arc::new(error.to_string()))
            })
            .await
            .as_ref()
            .map_err(|error| anyhow::Error::msg((**error).clone()))
    }

    fn install_write_path(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let weak = Arc::downgrade(self);
        context.own(EffectHandle::new(
            "SQLite persistence write path",
            move || {
                Box::pin(async move {
                    if let Some(backend) = weak.upgrade() {
                        let _closing = backend.operation_gate.write().await;
                        let drain = backend.drain_all().await;
                        backend.close_database();
                        drain?;
                    }
                    Ok(())
                })
            },
        ))?;

        let weak = Arc::downgrade(self);
        context.events().on_sync(
            context,
            "session/created",
            move |_, args| {
                let session = required_session(&args)?;
                if let Some(backend) = weak.upgrade() {
                    backend.init_for(&session)?;
                }
                Ok(EventReply::Undefined)
            },
            global_event(),
        )?;
        let weak = Arc::downgrade(self);
        context.events().on_sync(
            context,
            "session/event",
            move |_, args| {
                let session = required_session(&args)?;
                let event = args
                    .get::<SessionEvent>(1)
                    .ok_or_else(|| anyhow::anyhow!("session/event lacks an event"))?;
                if let Some(backend) = weak.upgrade() {
                    backend.init_for(&session)?.writes.enqueue(&event)?;
                }
                Ok(EventReply::Undefined)
            },
            global_event(),
        )?;
        let weak = Arc::downgrade(self);
        context.events().on(
            context,
            "session/flush",
            move |_, args| {
                let weak = weak.clone();
                Box::pin(async move {
                    let session = required_session(&args)?;
                    if let Some(backend) = weak.upgrade() {
                        backend.flush_live(&session).await?;
                    }
                    Ok(EventReply::Undefined)
                })
            },
            global_event(),
        )?;
        let weak = Arc::downgrade(self);
        context.events().on_sync(
            context,
            "session/disposed",
            move |_, args| {
                let session = required_session(&args)?;
                if let Some(backend) = weak.upgrade() {
                    backend.retire(session);
                }
                Ok(EventReply::Undefined)
            },
            global_event(),
        )?;
        for session in self.sessions.list() {
            self.init_for(&session)?;
        }
        Ok(())
    }

    fn close_database(&self) {
        if let Some(Ok(database)) = self.database.get()
            && let Some(connection) = database.connection.lock().take()
        {
            let _ = connection.close();
        }
    }

    async fn drain_all(self: &Arc<Self>) -> anyhow::Result<()> {
        let lives = self.live.lock().values().cloned().collect::<Vec<_>>();
        let mut errors = Vec::new();
        for live in lives {
            live.writes.cancel_automatic_wait();
            if live
                .init
                .await
                .map_err(|error| anyhow::Error::msg((*error).clone()))
                .is_err()
            {
                // Initialization failures already settled through the caller's
                // flush. They own no durable cursor to drain and must not be
                // replayed as a second teardown failure.
                continue;
            }
            if let Err(error) = live.writes.flush().await {
                errors.push(error.to_string());
            }
        }
        let retirements = self
            .retirements
            .lock()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for retirement in retirements {
            if retirement.settlement.clone().await.is_err()
                && let Err(error) = self.retire_core(&retirement.session).await
            {
                errors.push(error.to_string());
                continue;
            }
            if self
                .retirements
                .lock()
                .get(retirement.session.id())
                .is_some_and(|current| current.token == retirement.token)
            {
                self.retirements.lock().remove(retirement.session.id());
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("SQLite persistence dispose failed: {}", errors.join("; "))
        }
    }

    fn init_for(self: &Arc<Self>, session: &Arc<Session>) -> anyhow::Result<LiveSessionState> {
        let key = session_key(session);
        if let Some(live) = self.live.lock().get(&key).cloned() {
            return Ok(live);
        }
        if let Some(reservation) = self.preparations.reservation_for(session)? {
            return self.attach_prepared(session, &reservation);
        }
        let weak = Arc::downgrade(self);
        let owned_session = session.clone();
        let seed = session.events();
        let init: InitFuture = async move {
            let Some(backend) = weak.upgrade() else {
                return Err(Arc::new("SQLite persistence was disposed".to_owned()));
            };
            backend
                .initialize_live(&owned_session, &seed)
                .await
                .map_err(|error| Arc::new(error.to_string()))
        }
        .boxed()
        .shared();
        let writes = self.create_write_behind(session, init.clone());
        let live = LiveSessionState {
            init: init.clone(),
            writes,
        };
        let mut lives = self.live.lock();
        if let Some(existing) = lives.get(&key).cloned() {
            return Ok(existing);
        }
        lives.insert(key, live.clone());
        drop(lives);
        tokio::spawn(async move {
            let _ = init.await;
        });
        Ok(live)
    }

    fn create_write_behind(
        self: &Arc<Self>,
        session: &Arc<Session>,
        ready: InitFuture,
    ) -> SessionWriteBehind {
        let weak = Arc::downgrade(self);
        let id = session.id().clone();
        SessionWriteBehind::new(
            self.write_batch_max_delay,
            move |events| {
                let weak = weak.clone();
                let id = id.clone();
                let ready = ready.clone();
                async move {
                    ready
                        .await
                        .map_err(|error| anyhow::Error::msg((*error).clone()))?;
                    let backend = weak
                        .upgrade()
                        .ok_or_else(|| anyhow::anyhow!("SQLite persistence was disposed"))?;
                    backend.append_owned(&id, &events).await
                }
            },
            {
                let id = session.id().clone();
                move |error| {
                    tracing::warn!(session = %id, %error, "SQLite background write failed; events retained");
                }
            },
        )
    }

    fn attach_prepared(
        self: &Arc<Self>,
        session: &Arc<Session>,
        reservation: &SessionPreparationReservation<PreparedSqliteSource, BackendSession>,
    ) -> anyhow::Result<LiveSessionState> {
        let source = &reservation.source;
        let mut state = (*reservation.state).clone();
        let cursor = source.inspection.events.len();
        anyhow::ensure!(
            Arc::ptr_eq(&source.session, session)
                && state.owner.is_none()
                && state.events.len() == cursor
                && usize::try_from(session.first_live_seq())? == cursor,
            "session {} preparation no longer matches its persistence state",
            session.id()
        );
        let suffix = session.events()[cursor..].to_vec();
        self.preparations.attach(reservation)?;
        state.owner = Some(session_key(session));
        self.state.lock().insert(session.id().clone(), state);

        let weak = Arc::downgrade(self);
        let id = session.id().clone();
        let init: InitFuture = async move {
            let backend = weak
                .upgrade()
                .ok_or_else(|| Arc::new("SQLite persistence was disposed".to_owned()))?;
            backend
                .append_owned(&id, &suffix)
                .await
                .map_err(|error| Arc::new(error.to_string()))
        }
        .boxed()
        .shared();
        let writes = self.create_write_behind(session, init.clone());
        let live = LiveSessionState {
            init: init.clone(),
            writes,
        };
        self.live.lock().insert(session_key(session), live.clone());
        tokio::spawn(async move {
            let _ = init.await;
        });
        Ok(live)
    }

    async fn initialize_live(
        &self,
        session: &Arc<Session>,
        seed: &[SessionEvent],
    ) -> anyhow::Result<()> {
        let id = session.id();
        let owner = session_key(session);
        self.wait_for_other_retirement(id, owner).await?;
        let lock = self.lock_for(id);
        let _guard = lock.lock().await;
        let tracked = { self.state.lock().get(id).cloned() };
        if let Some(mut tracked) = tracked {
            anyhow::ensure!(
                tracked.header.cwd == session.header().cwd,
                "session {id} is already persisted at a different cwd (id collision)"
            );
            if let Some(existing_owner) = tracked.owner {
                if existing_owner == owner {
                    return Ok(());
                }
                let existing = self.live.lock().get(&existing_owner).cloned();
                if !tracked.materialized && existing.is_none_or(|live| !live.writes.has_work()) {
                    self.state.lock().remove(id);
                } else {
                    anyhow::bail!(
                        "session {id} is already bound to a different live session in this backend (id collision)"
                    );
                }
            } else {
                anyhow::ensure!(
                    seed_covers_prefix(seed, &tracked.events),
                    "session {id} is already persisted with events that do not match this live session (id collision)"
                );
                let cursor = tracked.events.len();
                tracked.owner = Some(owner);
                self.state.lock().insert(id.clone(), tracked);
                return self.append_core(id, &seed[cursor..]).await;
            }
        }
        if let Some(stored) = self.read_prefix(id).await? {
            anyhow::ensure!(
                stored.meta.cwd == session.header().cwd,
                "session {id} is already persisted at a different cwd (id collision)"
            );
            anyhow::ensure!(
                seed_covers_prefix(seed, &stored.events),
                "session {id} already has a persisted log on disk that does not match this live session (id collision)"
            );
            if stored.torn_from.is_some() {
                self.commit_repair(&stored.meta, stored.torn_from, &[])
                    .await?;
            }
            let cursor = stored.events.len();
            self.state.lock().insert(
                id.clone(),
                BackendSession {
                    header: stored.meta,
                    events: stored.events,
                    materialized: true,
                    owner: Some(owner),
                },
            );
            return self.append_core(id, &seed[cursor..]).await;
        }
        self.state.lock().insert(
            id.clone(),
            BackendSession {
                header: session.header().clone(),
                events: Vec::new(),
                materialized: false,
                owner: Some(owner),
            },
        );
        self.append_core(id, seed).await
    }

    async fn flush_live(self: &Arc<Self>, session: &Arc<Session>) -> anyhow::Result<()> {
        let live = self.init_for(session)?;
        live.writes.cancel_automatic_wait();
        live.init
            .await
            .map_err(|error| anyhow::Error::msg((*error).clone()))?;
        live.writes.flush().await
    }

    async fn flush_existing(&self, session: &Arc<Session>) -> anyhow::Result<()> {
        let live = self
            .live
            .lock()
            .get(&session_key(session))
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!("live session {} lacks persistence state", session.id())
            })?;
        live.writes.cancel_automatic_wait();
        live.init
            .await
            .map_err(|error| anyhow::Error::msg((*error).clone()))?;
        live.writes.flush().await
    }

    fn retire(self: &Arc<Self>, session: Arc<Session>) {
        if !self.live.lock().contains_key(&session_key(&session)) {
            return;
        }
        let weak = Arc::downgrade(self);
        let id = session.id().clone();
        let retiring_owner = session_key(&session);
        let token = Uuid::now_v7();
        let retiring_session = session.clone();
        let retirement: InitFuture = async move {
            let backend = weak
                .upgrade()
                .ok_or_else(|| Arc::new("SQLite persistence was disposed".to_owned()))?;
            backend
                .retire_core(&retiring_session)
                .await
                .map_err(|error| Arc::new(error.to_string()))
        }
        .boxed()
        .shared();
        self.retirements.lock().insert(
            id.clone(),
            RetirementEntry {
                token,
                owner: retiring_owner,
                session,
                settlement: retirement.clone(),
            },
        );
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let result = retirement.await;
            if let Some(backend) = weak.upgrade() {
                if result.is_ok()
                    && backend
                        .retirements
                        .lock()
                        .get(&id)
                        .is_some_and(|current| current.token == token)
                {
                    backend.retirements.lock().remove(&id);
                }
                if let Err(error) = result {
                    tracing::warn!(session = %id, %error, "SQLite session retirement failed");
                }
            }
        });
    }

    async fn wait_for_retirement(&self, id: &SessionId) -> anyhow::Result<()> {
        let retirement = self
            .retirements
            .lock()
            .get(id)
            .map(|retirement| retirement.settlement.clone());
        if let Some(retirement) = retirement {
            retirement
                .await
                .map_err(|error| anyhow::Error::msg((*error).clone()))?;
        }
        Ok(())
    }

    async fn wait_for_other_retirement(&self, id: &SessionId, owner: usize) -> anyhow::Result<()> {
        let retirement = self.retirements.lock().get(id).and_then(|retirement| {
            (retirement.owner != owner).then(|| retirement.settlement.clone())
        });
        if let Some(retirement) = retirement {
            retirement
                .await
                .map_err(|error| anyhow::Error::msg((*error).clone()))?;
        }
        Ok(())
    }

    /// Waits for any in-flight retirement with caller-local cancellation.
    ///
    /// Once the signal is aborted the observation rejects with the exact
    /// abort reason even when the retirement settles at the same time,
    /// matching the source's queued-abort precedence for an operation that
    /// never started. A failed retirement still propagates its failure.
    async fn wait_for_retirement_abortable(
        &self,
        id: &SessionId,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<()> {
        let retirement = self
            .retirements
            .lock()
            .get(id)
            .map(|retirement| retirement.settlement.clone());
        let Some(retirement) = retirement else {
            return Ok(());
        };
        match signal {
            Some(signal) => {
                tokio::select! {
                    biased;
                    () = signal.cancelled() => ensure_not_aborted(Some(signal)),
                    result = retirement => {
                        result.map_err(|error| anyhow::Error::msg((*error).clone()))
                    }
                }
            }
            None => retirement
                .await
                .map_err(|error| anyhow::Error::msg((*error).clone())),
        }
    }

    async fn retire_core(self: &Arc<Self>, session: &Arc<Session>) -> anyhow::Result<()> {
        let key = session_key(session);
        let id = session.id();
        let live = self.live.lock().get(&key).cloned();
        if let Some(live) = live {
            live.writes.cancel_automatic_wait();
            if live.init.await.is_ok() {
                live.writes.flush().await?;
            }
        }
        let lock = self.lock_for(id);
        let _guard = lock.lock().await;
        self.live.lock().remove(&key);
        if self
            .state
            .lock()
            .get(id)
            .is_some_and(|state| state.owner == Some(key))
        {
            self.state.lock().remove(id);
        }
        Ok(())
    }

    async fn append_core(&self, id: &SessionId, events: &[SessionEvent]) -> anyhow::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        self.preparations.assert_writable(id)?;
        let mut current = self.state.lock().get(id).cloned();
        if current.is_none() && self.read_revision(id).await?.is_some() {
            let loaded = self.cold_inspection(id, true).await?;
            current = Some(BackendSession {
                header: loaded.meta,
                events: loaded.events,
                materialized: true,
                owner: None,
            });
        }
        let mut current = current
            .ok_or_else(|| anyhow::anyhow!("session {id} has not been created in persistence"))?;
        let expected = u64::try_from(current.events.len())?;
        anyhow::ensure!(
            events.first().map(|event| event.seq) == Some(expected),
            "session {id} append must begin at seq {expected}"
        );
        for (offset, event) in events.iter().enumerate() {
            anyhow::ensure!(
                event.seq == expected + u64::try_from(offset)?,
                "session {id} append contains a sequence gap"
            );
        }
        let combined = current
            .events
            .iter()
            .cloned()
            .chain(events.iter().cloned())
            .collect::<Vec<_>>();
        normalize_stored_events(events, id)?;
        validate_normalized_events(&current.header, &combined)?;
        validate_session_events(&combined)?;
        self.append_batch(&current.header, events, current.materialized)
            .await?;
        current.materialized = true;
        current.events = combined;
        self.state.lock().insert(id.clone(), current);
        self.preparations.invalidate(id);
        Ok(())
    }

    async fn append_owned(&self, id: &SessionId, events: &[SessionEvent]) -> anyhow::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let lock = self.lock_for(id);
        let _guard = lock.lock().await;
        self.append_core(id, events).await
    }

    async fn prepare_core(&self, id: &SessionId) -> anyhow::Result<PreparedSqliteSource> {
        let stored = self
            .read_prefix(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session {id} was not found"))?;
        let mut balanced = stored.events.clone();
        let closers = interrupted_turn_closers(&balanced);
        balanced.extend(closers.clone());
        validate_session_events(&balanced)?;
        let meta = stored.meta;
        let session = self.sessions.prepare(
            Some(id.clone()),
            CreateSessionOptions {
                seed: Some(balanced.clone()),
                cwd: meta.cwd.clone(),
                parent_session: meta.parent_session.clone(),
                created_at: Some(meta.created_at),
                seed_length: meta.seed_length,
                origin: meta.origin,
                delegation_depth: meta.delegation_depth,
                agent_preset: meta.agent_preset.clone(),
            },
        )?;
        let session_length = session.events().len();
        Ok(PreparedSqliteSource {
            inspection: SessionInspection {
                meta: session.header().clone(),
                events: balanced,
            },
            session,
            revision: stored.revision,
            session_length,
            torn_from: stored.torn_from,
            closers,
        })
    }

    async fn commit_prepared(
        &self,
        source: Arc<PreparedSqliteSource>,
    ) -> anyhow::Result<Option<(Arc<PreparedSqliteSource>, BackendSession)>> {
        let id = &source.inspection.meta.id;
        let lock = self.lock_for(id);
        let _guard = lock.lock().await;
        anyhow::ensure!(
            self.state
                .lock()
                .get(id)
                .is_none_or(|state| state.owner.is_none()),
            "session {id} already has a live persistence owner"
        );
        if self.read_revision(id).await? != Some(source.revision.clone()) {
            return Ok(None);
        }
        if source.torn_from.is_some() || !source.closers.is_empty() {
            self.commit_repair(&source.inspection.meta, source.torn_from, &source.closers)
                .await?;
            return Ok(None);
        }
        let state = BackendSession {
            header: source.inspection.meta.clone(),
            events: source.inspection.events.clone(),
            materialized: true,
            owner: None,
        };
        self.state.lock().insert(id.clone(), state.clone());
        Ok(Some((source, state)))
    }

    async fn reserve_prepared(
        &self,
        id: &SessionId,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionPreparationReservation<PreparedSqliteSource, BackendSession>> {
        loop {
            ensure_not_aborted(signal.as_ref())?;
            self.wait_for_retirement(id).await?;
            anyhow::ensure!(
                self.sessions.get(id).is_none(),
                "cannot prepare session {id} while it is live"
            );
            let load_weak = self.self_weak.clone();
            let load_id = id.clone();
            let commit_weak = self.self_weak.clone();
            let reservation = self
                .preparations
                .reserve(
                    id,
                    move || async move {
                        let backend = load_weak
                            .upgrade()
                            .ok_or_else(|| anyhow::anyhow!("SQLite persistence was disposed"))?;
                        backend.prepare_core(&load_id).await
                    },
                    move |source| async move {
                        let backend = commit_weak
                            .upgrade()
                            .ok_or_else(|| anyhow::anyhow!("SQLite persistence was disposed"))?;
                        backend.commit_prepared(source).await
                    },
                    signal.clone(),
                )
                .await?;
            let Some(reservation) = reservation else {
                continue;
            };
            if self.sessions.get(id).is_some() {
                self.preparations.release(&reservation, false);
                anyhow::bail!("cannot prepare session {id} while it is live");
            }
            return Ok(reservation);
        }
    }

    async fn cold_inspection(
        &self,
        id: &SessionId,
        repair: bool,
    ) -> anyhow::Result<SessionInspection> {
        let stored = self
            .read_prefix(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session {id} was not found"))?;
        let mut events = stored.events;
        let closers = interrupted_turn_closers(&events);
        if repair && (stored.torn_from.is_some() || !closers.is_empty()) {
            self.commit_repair(&stored.meta, stored.torn_from, &closers)
                .await?;
        }
        events.extend(closers);
        validate_session_events(&events)?;
        if repair {
            self.state.lock().insert(
                id.clone(),
                BackendSession {
                    header: stored.meta.clone(),
                    events: events.clone(),
                    materialized: true,
                    owner: None,
                },
            );
        }
        Ok(SessionInspection {
            meta: stored.meta,
            events,
        })
    }

    async fn read_prefix(&self, id: &SessionId) -> anyhow::Result<Option<StoredPrefix>> {
        let database = self.database().await?;
        let mut connection = database.connection.lock();
        let connection = connection
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("SQLite session persistence is closed"))?;
        let transaction = connection.transaction()?;
        let Some(row) = session_row(&transaction, id)? else {
            transaction.commit()?;
            return Ok(None);
        };
        let event_rows = query_event_rows(&transaction, id, None)?;
        transaction.commit()?;
        let meta = row_to_meta(&row)?;
        assert_supported_version(&meta)?;
        let (events, torn_from) = scan_rows(&event_rows, 0)?;
        let events = normalize_stored_events(&events, id)?;
        assert_known_events(&events, id)
            .map_err(|error| SessionFormatUnsupportedError::new(error.to_string(), None))?;
        validate_normalized_events(&meta, &events)?;
        validate_session_events(&events)?;
        Ok(Some(StoredPrefix {
            meta,
            events,
            revision: sqlite_revision(&database.store_identity, &row),
            torn_from,
        }))
    }

    async fn read_revision(
        &self,
        id: &SessionId,
    ) -> anyhow::Result<Option<SessionPersistenceRevision>> {
        let database = self.database().await?;
        let connection = database.connection.lock();
        let connection = connection
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("SQLite session persistence is closed"))?;
        Ok(session_row(connection, id)?
            .as_ref()
            .map(|row| sqlite_revision(&database.store_identity, row)))
    }

    async fn read_suffix(
        &self,
        id: &SessionId,
        from_seq: u64,
    ) -> anyhow::Result<Option<SessionInspection>> {
        let database = self.database().await?;
        let (meta, rows) = {
            let connection = database.connection.lock();
            let connection = connection
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("SQLite session persistence is closed"))?;
            let Some(row) = session_row(connection, id)? else {
                return Ok(None);
            };
            let meta = row_to_meta(&row)?;
            assert_supported_version(&meta)?;
            let rows = query_event_rows(connection, id, Some(from_seq))?;
            (meta, rows)
        };
        let (events, _) = scan_rows(&rows, from_seq)?;
        if events.iter().any(needs_legacy_prefix) {
            let whole = self
                .read_prefix(id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("session {id} was not found"))?;
            return Ok(Some(SessionInspection {
                meta: whole.meta,
                events: whole
                    .events
                    .into_iter()
                    .filter(|event| event.seq >= from_seq)
                    .collect(),
            }));
        }
        let events = normalize_stored_events(&events, id)?;
        assert_known_events(&events, id)
            .map_err(|error| SessionFormatUnsupportedError::new(error.to_string(), None))?;
        Ok(Some(SessionInspection { meta, events }))
    }

    /// Atomically materializes if needed and appends one complete batch.
    ///
    /// # Errors
    ///
    /// Propagates readiness, serialization, conversion, and transaction errors.
    pub async fn append_batch(
        &self,
        meta: &SessionHeader,
        events: &[SessionEvent],
        materialized: bool,
    ) -> anyhow::Result<()> {
        let database = self.database().await?;
        let mut connection = database.connection.lock();
        let connection = connection
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("SQLite session persistence is closed"))?;
        let transaction = connection.transaction()?;
        if !materialized {
            write_session_row(&transaction, meta)?;
        }
        {
            let mut insert = transaction.prepare(
                "INSERT INTO events (session_id, seq, type, time, data, source_event_seqs, surface_op, ignorable) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for event in events {
                insert.execute(params![
                    meta.id.as_str(),
                    i64::try_from(event.seq)?,
                    event.event_type,
                    event.time,
                    serde_json::to_string(&event.data)?,
                    event
                        .source_event_seqs
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?,
                    event
                        .surface_op
                        .as_ref()
                        .map(serde_json::to_string)
                        .transpose()?,
                    event.ignorable.is_some_and(|value| value).then_some(1_i64),
                ])?;
            }
        }
        transaction.execute(
            "UPDATE sessions SET revision = revision + 1 WHERE id = ?1",
            [meta.id.as_str()],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Atomically deletes a torn tail and appends synthetic closing events.
    ///
    /// # Errors
    ///
    /// Propagates readiness, serialization, conversion, and transaction errors.
    pub async fn commit_repair(
        &self,
        meta: &SessionHeader,
        torn_from: Option<u64>,
        closers: &[SessionEvent],
    ) -> anyhow::Result<()> {
        let database = self.database().await?;
        let mut connection = database.connection.lock();
        let connection = connection
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("SQLite session persistence is closed"))?;
        let transaction = connection.transaction()?;
        if let Some(torn_from) = torn_from {
            transaction.execute(
                "DELETE FROM events WHERE session_id = ?1 AND seq >= ?2",
                params![meta.id.as_str(), i64::try_from(torn_from)?],
            )?;
        }
        if !closers.is_empty() {
            let mut insert = transaction.prepare(
                "INSERT INTO events (session_id, seq, type, time, data, source_event_seqs, surface_op, ignorable) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for event in closers {
                insert.execute(event_bindings(meta, event)?)?;
            }
        }
        if torn_from.is_some() || !closers.is_empty() {
            transaction.execute(
                "UPDATE sessions SET revision = revision + 1 WHERE id = ?1",
                [meta.id.as_str()],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    async fn stored_headers(&self) -> anyhow::Result<Vec<(SessionHeader, SessionRow)>> {
        let database = self.database().await?;
        let connection = database.connection.lock();
        let connection = connection
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("SQLite session persistence is closed"))?;
        let mut statement = connection.prepare(
            "SELECT id, version, created_at, cwd, parent_session, seed_length, origin, incarnation, revision, delegation_depth, agent_preset FROM sessions",
        )?;
        let rows = statement
            .query_map([], decode_session_row)?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|row| Ok((row_to_meta(&row)?, row)))
            .collect()
    }
}

#[async_trait]
impl SessionPersistence for SqliteSessionPersistence {
    fn locate(
        &self,
        _meta: &SessionHeader,
    ) -> Option<seekdeep_session_persistence::SessionLocation> {
        None
    }

    fn supports_raw_artifacts(&self) -> bool {
        false
    }

    async fn create(&self, meta: &SessionHeader) -> anyhow::Result<()> {
        anyhow::ensure!(!meta.id.as_str().is_empty(), "session id cannot be empty");
        self.wait_for_retirement(&meta.id).await?;
        let lock = self.lock_for(&meta.id);
        let _guard = lock.lock().await;
        anyhow::ensure!(
            !self.preparations.has(&meta.id),
            "session {} already exists in this backend",
            meta.id
        );
        anyhow::ensure!(
            !self.state.lock().contains_key(&meta.id),
            "session {} already exists in this backend",
            meta.id
        );
        anyhow::ensure!(
            self.read_prefix(&meta.id).await?.is_none(),
            "session {} already has a persisted log on disk; load/resume it instead of creating",
            meta.id
        );
        self.state.lock().insert(
            meta.id.clone(),
            BackendSession {
                header: meta.clone(),
                events: Vec::new(),
                materialized: false,
                owner: None,
            },
        );
        Ok(())
    }

    async fn append(&self, id: &SessionId, events: &[SessionEvent]) -> anyhow::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let _operation = self.operation_gate.read().await;
        self.wait_for_retirement(id).await?;
        self.append_owned(id, events).await
    }

    async fn prepare(
        &self,
        sessions: &Arc<SessionStore>,
        id: &SessionId,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionPreparation> {
        anyhow::ensure!(
            Arc::ptr_eq(sessions, &self.sessions),
            "cannot prepare a session with a different SessionStore"
        );
        let reservation = self.reserve_prepared(id, signal).await?;
        let session = reservation.source.session.clone();
        let source = reservation.source.clone();
        let pool = self.preparations.clone();
        Ok(SessionPreparation::new(session, move || {
            let reusable = reservation.state.owner.is_none()
                && source.session.events().len() == source.session_length;
            pool.release(&reservation, reusable);
        }))
    }

    async fn load(&self, id: &SessionId) -> anyhow::Result<SessionInspection> {
        self.wait_for_retirement(id).await?;
        if let Some(live) = self.sessions.get(id) {
            let events = live.events();
            self.flush_existing(&live).await?;
            anyhow::ensure!(
                interrupted_turn_closers(&events).is_empty(),
                "cannot load session {id} while its live turn is open; use the live Session or wait for the turn to close"
            );
            anyhow::ensure!(!events.is_empty(), "session {id} was not found");
            return Ok(SessionInspection {
                meta: live.header().clone(),
                events,
            });
        }
        let reservation = self.reserve_prepared(id, None).await?;
        if let Some(live) = self.sessions.get(id) {
            self.preparations.discard(&reservation);
            let events = live.events();
            self.flush_existing(&live).await?;
            anyhow::ensure!(
                interrupted_turn_closers(&events).is_empty(),
                "cannot load session {id} while its live turn is open; use the live Session or wait for the turn to close"
            );
            return Ok(SessionInspection {
                meta: live.header().clone(),
                events,
            });
        }
        let inspection = reservation.source.inspection.clone();
        self.preparations.discard(&reservation);
        Ok(inspection)
    }

    async fn inspect(
        &self,
        id: &SessionId,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        loop {
            ensure_not_aborted(signal.as_ref())?;
            self.wait_for_retirement_abortable(id, signal.as_ref())
                .await?;
            if let Some(live) = self.sessions.get(id) {
                return Ok(SessionInspection {
                    meta: live.header().clone(),
                    events: live.events(),
                });
            }
            let weak = self.self_weak.clone();
            let load_id = id.clone();
            let source = self
                .preparations
                .inspect(
                    id,
                    move || async move {
                        let backend = weak
                            .upgrade()
                            .ok_or_else(|| anyhow::anyhow!("SQLite persistence was disposed"))?;
                        backend.prepare_core(&load_id).await
                    },
                    signal.clone(),
                )
                .await?;
            if let Some(live) = self.sessions.get(id) {
                return Ok(SessionInspection {
                    meta: live.header().clone(),
                    events: live.events(),
                });
            }
            ensure_not_aborted(signal.as_ref())?;
            if self.read_revision(id).await? == Some(source.revision.clone()) {
                return Ok(source.inspection.clone());
            }
            match self.preparations.discard_ready(id, &source) {
                DiscardReady::Retained => return Ok(source.inspection.clone()),
                DiscardReady::Discarded | DiscardReady::Missing => {}
            }
        }
    }

    async fn read_from(
        &self,
        id: &SessionId,
        from_seq: u64,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection> {
        anyhow::ensure!(
            from_seq <= 9_007_199_254_740_991,
            "readFrom fromSeq must be a non-negative safe integer, got {from_seq}"
        );
        ensure_not_aborted(signal.as_ref())?;
        self.wait_for_retirement(id).await?;
        let inspection = self
            .read_suffix(id, from_seq)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session {id} was not found"))?;
        ensure_not_aborted(signal.as_ref())?;
        Ok(inspection)
    }

    async fn list(&self, signal: Option<AbortSignal>) -> anyhow::Result<Vec<SessionHeader>> {
        ensure_not_aborted(signal.as_ref())?;
        let headers = self
            .stored_headers()
            .await?
            .into_iter()
            .map(|(header, _)| header)
            .collect();
        ensure_not_aborted(signal.as_ref())?;
        Ok(headers)
    }

    async fn list_snapshots(
        &self,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SessionPersistenceSnapshot>> {
        ensure_not_aborted(signal.as_ref())?;
        let database = self.database().await?;
        ensure_not_aborted(signal.as_ref())?;
        let identity = database.store_identity.clone();
        let rows = self.stored_headers().await?;
        ensure_not_aborted(signal.as_ref())?;
        rows.into_iter()
            .map(|(header, row)| {
                Ok(SessionPersistenceSnapshot {
                    header,
                    revision: sqlite_revision(&identity, &row),
                })
            })
            .collect()
    }
}

/// Builds the source-compatible `SQLite` persistence plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: SqliteConfig = serde_json::from_value(config)?;
            let sessions = context.get(SESSIONS).ok_or_else(|| {
                anyhow::anyhow!("session-persistence-sqlite lost required sessions service")
            })?;
            let backend = SqliteSessionPersistence::build(sessions, config)?;
            let erased: Arc<dyn SessionPersistence> = backend.clone();
            SessionPersistenceService::new(erased).provide(&context)?;
            backend.install_write_path(&context)?;
            Ok(())
        })
    })
    .with_config_validator(|value: &Value| {
        let config: SqliteConfig = serde_json::from_value(value.clone())?;
        anyhow::ensure!(!config.path.as_os_str().is_empty(), "path is required");
        anyhow::ensure!(
            config.prepared_session_cache_size > 0,
            "preparedSessionCacheSize must be a positive integer"
        );
        anyhow::ensure!(
            (1..=MAX_WRITE_BATCH_DELAY_MS).contains(&config.write_batch_max_delay_ms),
            "writeBatchMaxDelayMs must be an integer between 1 and 2147483647"
        );
        Ok(serde_json::to_value(config)?)
    })
}

/// Installs the `SQLite` plugin and returns its lifecycle fiber.
///
/// # Errors
///
/// Returns inactive-context and configuration failures.
pub fn install(
    context: &Context,
    config: SqliteConfig,
) -> anyhow::Result<Arc<seekdeep_cordis::PluginFiber>> {
    Ok(context.plugin(plugin(), serde_json::to_value(config)?)?)
}

fn query_event_rows(
    connection: &Connection,
    id: &SessionId,
    from_seq: Option<u64>,
) -> anyhow::Result<Vec<EventRow>> {
    let sql = if from_seq.is_some() {
        "SELECT seq, type, time, data, source_event_seqs, surface_op, ignorable FROM events WHERE session_id = ?1 AND seq >= ?2 ORDER BY seq"
    } else {
        "SELECT seq, type, time, data, source_event_seqs, surface_op, ignorable FROM events WHERE session_id = ?1 ORDER BY seq"
    };
    let mut statement = connection.prepare(sql)?;
    let mut output = Vec::new();
    if let Some(from_seq) = from_seq {
        let rows = statement.query_map(
            params![id.as_str(), i64::try_from(from_seq)?],
            decode_event_row,
        )?;
        for row in rows {
            output.push(row?);
        }
    } else {
        let rows = statement.query_map([id.as_str()], decode_event_row)?;
        for row in rows {
            output.push(row?);
        }
    }
    Ok(output)
}

fn decode_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        seq: row.get(0)?,
        event_type: row.get(1)?,
        time: row.get(2)?,
        data: row.get(3)?,
        source_event_seqs: row.get(4)?,
        surface_op: row.get(5)?,
        ignorable: row.get(6)?,
    })
}

fn decode_session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        id: row.get(0)?,
        version: row.get(1)?,
        created_at: row.get(2)?,
        cwd: row.get(3)?,
        parent_session: row.get(4)?,
        seed_length: row.get(5)?,
        origin: row.get(6)?,
        incarnation: row.get(7)?,
        revision: row.get(8)?,
        delegation_depth: row.get(9)?,
        agent_preset: row.get(10)?,
    })
}

fn write_session_row(connection: &Connection, meta: &SessionHeader) -> anyhow::Result<()> {
    connection.execute(
        "INSERT INTO sessions
          (id, version, created_at, cwd, parent_session, seed_length, origin, delegation_depth, agent_preset, incarnation, revision)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0)
         ON CONFLICT(id) DO UPDATE SET
          version = excluded.version,
          created_at = excluded.created_at,
          cwd = excluded.cwd,
          parent_session = excluded.parent_session,
          seed_length = excluded.seed_length,
          origin = excluded.origin,
          delegation_depth = excluded.delegation_depth,
          agent_preset = excluded.agent_preset",
        params![
            meta.id.as_str(),
            i64::from(meta.version),
            i64::try_from(meta.created_at)?,
            meta.cwd,
            meta.parent_session.as_ref().map(SessionId::as_str),
            meta.seed_length.map(i64::try_from).transpose()?,
            meta.origin.map(|origin| match origin {
                SessionOrigin::Subagent => "subagent",
            }),
            meta.delegation_depth.map(i64::try_from).transpose()?,
            meta.agent_preset,
            Uuid::new_v4().to_string(),
        ],
    )?;
    Ok(())
}

fn event_bindings<'a>(
    meta: &'a SessionHeader,
    event: &'a SessionEvent,
) -> anyhow::Result<rusqlite::ParamsFromIter<Vec<rusqlite::types::Value>>> {
    let values = vec![
        rusqlite::types::Value::Text(meta.id.to_string()),
        rusqlite::types::Value::Integer(i64::try_from(event.seq)?),
        rusqlite::types::Value::Text(event.event_type.clone()),
        rusqlite::types::Value::Integer(event.time),
        rusqlite::types::Value::Text(serde_json::to_string(&event.data)?),
        event
            .source_event_seqs
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?
            .map_or(rusqlite::types::Value::Null, rusqlite::types::Value::Text),
        event
            .surface_op
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?
            .map_or(rusqlite::types::Value::Null, rusqlite::types::Value::Text),
        if event.ignorable.is_some_and(|value| value) {
            rusqlite::types::Value::Integer(1)
        } else {
            rusqlite::types::Value::Null
        },
    ];
    Ok(rusqlite::params_from_iter(values))
}

fn global_event() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}

fn required_session(args: &EventArgs) -> anyhow::Result<Arc<Session>> {
    args.get::<Session>(0)
        .ok_or_else(|| anyhow::anyhow!("session event lacks a session"))
}

fn session_key(session: &Arc<Session>) -> usize {
    Arc::as_ptr(session) as usize
}

fn seed_covers_prefix(seed: &[SessionEvent], prefix: &[SessionEvent]) -> bool {
    prefix.len() <= seed.len() && prefix.iter().zip(seed).all(|(left, right)| left == right)
}

fn needs_legacy_prefix(event: &SessionEvent) -> bool {
    if event.event_type == "steering/message" {
        return true;
    }
    let Some(data) = event.data.as_object() else {
        return false;
    };
    match event.event_type.as_str() {
        "user/message" => !data.contains_key("id") && data.contains_key("content"),
        "assistant/message" => !data.contains_key("message") && data.contains_key("content"),
        "tool/result" => !data.contains_key("message") && data.contains_key("callId"),
        _ => false,
    }
}

fn open_state(path: &Path, journal_mode: JournalMode) -> anyhow::Result<DatabaseState> {
    if path != Path::new(":memory:") {
        let parent = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("database path has no parent"))?;
        create_owner_directories(parent)?;
        create_owner_file(path)?;
    }
    let connection = open_database(path, journal_mode)?;
    let store_id = store_identity(&connection, path)?;
    let identity = if path == Path::new(":memory:") {
        format!("memory:store:{store_id}")
    } else {
        physical_identity(path, &store_id)?
    };
    Ok(DatabaseState {
        connection: Mutex::new(Some(connection)),
        store_identity: identity,
    })
}

#[cfg(unix)]
fn create_owner_directories(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700).create(path)?;
    Ok(())
}

#[cfg(not(unix))]
fn create_owner_directories(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)?;
    Ok(())
}

#[cfg(unix)]
fn create_owner_file(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::OpenOptionsExt as _;
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(file) => drop(file),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_owner_file(path: &Path) -> anyhow::Result<()> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => drop(file),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn physical_identity(path: &Path, store_id: &str) -> anyhow::Result<String> {
    let metadata = std::fs::metadata(path)?;
    let created = metadata
        .created()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Ok(format!(
            "file:{}:{}:{created}:store:{store_id}",
            metadata.dev(),
            metadata.ino()
        ))
    }
    #[cfg(not(unix))]
    {
        Ok(format!(
            "file:{}:{created}:store:{store_id}",
            path.display()
        ))
    }
}

fn sqlite_revision(identity: &str, row: &SessionRow) -> SessionPersistenceRevision {
    SessionPersistenceRevision::new(format!(
        "{identity}:incarnation:{}:revision:{}",
        row.incarnation, row.revision
    ))
}

fn ensure_not_aborted(signal: Option<&AbortSignal>) -> anyhow::Result<()> {
    ensure_persistence_not_aborted(signal)
}

fn assert_supported_version(meta: &SessionHeader) -> anyhow::Result<()> {
    if meta.version == SESSION_FORMAT_VERSION {
        return Ok(());
    }
    Err(SessionFormatUnsupportedError::new(
        session_format_version_refusal(meta.id.as_str(), &serde_json::json!(meta.version)),
        None,
    )
    .into())
}
