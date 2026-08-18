//! Durable filesystem orchestration for the JSONL artifact format.

#[cfg(not(unix))]
use std::time::UNIX_EPOCH;
use std::{
    collections::{HashMap, HashSet},
    fs::Metadata,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use futures::{FutureExt as _, future::Shared};
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply, fiber::EffectHandle};
use seekdeep_core::{
    invariant::validate_session_events,
    preparation::SessionPreparation,
    repair::interrupted_turn_closers,
    session::{Session, SessionEvent, SessionHeader, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::AbortSignal;
use seekdeep_session_persistence::{
    DEFAULT_PREPARED_SESSION_CACHE_SIZE, DEFAULT_WRITE_BATCH_MAX_DELAY_MS,
    MAX_WRITE_BATCH_DELAY_MS, SessionFormatUnsupportedError, SessionInspection, SessionLocation,
    SessionPersistence, SessionPersistenceRevision, SessionPersistenceSnapshot, SessionRawArtifact,
    ensure_persistence_not_aborted,
    preparations::{
        DiscardReady, PreparedSource, SessionPreparationReservation, SessionPreparations,
    },
    stored_events::{assert_known_events, normalize_stored_events, validate_normalized_events},
    write_behind::SessionWriteBehind,
};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex as AsyncMutex, OnceCell, RwLock as AsyncRwLock},
};
use uuid::Uuid;

use crate::format::{
    JsonlCompression, SessionLogScan, SessionLogScanner, event_lines, header_line, log_path,
    parse_header_meta, scan_log, session_dir,
};
use crate::zstd::{
    compress_zstd_frame, decompress_zstd_frame, decompress_zstd_prefix, scan_zstd_frames,
};

/// JSONL backend configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JsonlConfig {
    /// Root directory for every project/session artifact.
    pub root: PathBuf,
    /// Whether newly written delta runs use packed storage rows.
    #[serde(default = "default_pack_chunks")]
    pub pack_chunks: bool,
    /// Physical artifact encoding.
    #[serde(default)]
    pub compression: JsonlCompression,
    /// Fixed live-event coalescing window.
    #[serde(default = "default_write_batch_max_delay_ms")]
    pub write_batch_max_delay_ms: u64,
    /// Maximum completed cold preparations retained for resume reuse.
    #[serde(default = "default_prepared_session_cache_size")]
    pub prepared_session_cache_size: usize,
}

const fn default_pack_chunks() -> bool {
    true
}

const fn default_write_batch_max_delay_ms() -> u64 {
    DEFAULT_WRITE_BATCH_MAX_DELAY_MS
}

const fn default_prepared_session_cache_size() -> usize {
    DEFAULT_PREPARED_SESSION_CACHE_SIZE
}

impl JsonlConfig {
    /// Creates source-compatible defaults rooted at an explicit directory.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            pack_chunks: true,
            compression: JsonlCompression::Zstd,
            write_batch_max_delay_ms: DEFAULT_WRITE_BATCH_MAX_DELAY_MS,
            prepared_session_cache_size: DEFAULT_PREPARED_SESSION_CACHE_SIZE,
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

#[derive(Debug)]
struct StoredScan {
    scan: SessionLogScan,
    truncate_to: Option<usize>,
    recovered_events: Vec<SessionEvent>,
    revision: String,
}

struct JsonlPreparedSource {
    inspection: SessionInspection,
    session: Arc<Session>,
    revision: String,
    session_length: usize,
    path: PathBuf,
    truncate_to: Option<usize>,
    recovered_events: Vec<SessionEvent>,
    closers: Vec<SessionEvent>,
}

impl PreparedSource for JsonlPreparedSource {
    fn session(&self) -> &Arc<Session> {
        &self.session
    }
}

/// Per-session append-only JSONL storage.
pub struct JsonlSessionPersistence {
    root: PathBuf,
    pack_chunks: bool,
    compression: JsonlCompression,
    sessions: Arc<SessionStore>,
    state: Mutex<HashMap<SessionId, BackendSession>>,
    locks: Mutex<HashMap<SessionId, Arc<AsyncMutex<()>>>>,
    operation_gate: AsyncRwLock<()>,
    encoding_checked: OnceCell<()>,
    write_batch_max_delay: Duration,
    live: Mutex<HashMap<usize, LiveSessionState>>,
    retirements: Mutex<HashMap<SessionId, RetirementEntry>>,
    preparations: SessionPreparations<JsonlPreparedSource, BackendSession>,
    self_weak: std::sync::Weak<Self>,
}

impl std::fmt::Debug for JsonlSessionPersistence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JsonlSessionPersistence")
            .field("root", &self.root)
            .field("pack_chunks", &self.pack_chunks)
            .field("compression", &self.compression)
            .finish_non_exhaustive()
    }
}

impl JsonlSessionPersistence {
    /// Resolves the root once and constructs an unmaterialized backend.
    ///
    /// # Errors
    ///
    /// Rejects a relative/empty root resolution failure or an existing
    /// non-directory root.
    pub fn new(sessions: Arc<SessionStore>, config: JsonlConfig) -> anyhow::Result<Arc<Self>> {
        let context = sessions.context();
        let backend = Self::build(sessions, config)?;
        backend.install_write_path(&context)?;
        Ok(backend)
    }

    /// Constructs against an explicit lifecycle context.
    ///
    /// # Errors
    ///
    /// Returns config, filesystem preflight, or effect-registration failures.
    pub fn new_in_context(
        context: &Context,
        sessions: Arc<SessionStore>,
        config: JsonlConfig,
    ) -> anyhow::Result<Arc<Self>> {
        let backend = Self::build(sessions, config)?;
        backend.install_write_path(context)?;
        Ok(backend)
    }

    pub(crate) fn build(
        sessions: Arc<SessionStore>,
        config: JsonlConfig,
    ) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(
            !config.root.as_os_str().is_empty(),
            "JSONL persistence root is required"
        );
        let root = if config.root.is_absolute() {
            config.root
        } else {
            std::env::current_dir()?.join(config.root)
        };
        if root.exists() {
            anyhow::ensure!(
                root.is_dir(),
                "JSONL persistence root {} is not a directory",
                root.display()
            );
            std::fs::read_dir(&root)?;
        }
        anyhow::ensure!(
            (1..=MAX_WRITE_BATCH_DELAY_MS).contains(&config.write_batch_max_delay_ms),
            "writeBatchMaxDelayMs must be an integer between 1 and 2147483647"
        );
        let preparations = SessionPreparations::new(config.prepared_session_cache_size)?;
        let backend = Arc::new_cyclic(|weak| Self {
            root,
            pack_chunks: config.pack_chunks,
            compression: config.compression,
            write_batch_max_delay: Duration::from_millis(config.write_batch_max_delay_ms),
            sessions,
            state: Mutex::new(HashMap::new()),
            locks: Mutex::new(HashMap::new()),
            operation_gate: AsyncRwLock::new(()),
            encoding_checked: OnceCell::new(),
            live: Mutex::new(HashMap::new()),
            retirements: Mutex::new(HashMap::new()),
            preparations,
            self_weak: weak.clone(),
        });
        Ok(backend)
    }

    fn lock_for(&self, id: &SessionId) -> Arc<AsyncMutex<()>> {
        self.locks
            .lock()
            .entry(id.clone())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    pub(crate) fn install_write_path(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let context = context.clone();
        let weak = Arc::downgrade(self);
        context.own(EffectHandle::new(
            "JSONL persistence write path",
            move || {
                Box::pin(async move {
                    if let Some(backend) = weak.upgrade() {
                        let _closing = backend.operation_gate.write().await;
                        backend.drain_all().await?;
                    }
                    Ok(())
                })
            },
        ))?;
        let weak = Arc::downgrade(self);
        context.events().on_sync(
            &context,
            "session/created",
            move |_, args| {
                let session = required_session(&args)?;
                if let Some(backend) = weak.upgrade() {
                    backend.init_for(&session)?;
                }
                Ok(EventReply::Undefined)
            },
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
        )?;

        let weak = Arc::downgrade(self);
        context.events().on_sync(
            &context,
            "session/event",
            move |_, args| {
                let session = required_session(&args)?;
                let event = args
                    .get::<SessionEvent>(1)
                    .ok_or_else(|| anyhow::anyhow!("session/event lacks an event"))?;
                if let Some(backend) = weak.upgrade() {
                    let live = backend.init_for(&session)?;
                    live.writes.enqueue(&event)?;
                }
                Ok(EventReply::Undefined)
            },
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
        )?;

        let weak = Arc::downgrade(self);
        context.events().on(
            &context,
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
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
        )?;

        let weak = Arc::downgrade(self);
        context.events().on_sync(
            &context,
            "session/disposed",
            move |_, args| {
                let session = required_session(&args)?;
                if let Some(backend) = weak.upgrade() {
                    backend.retire(session);
                }
                Ok(EventReply::Undefined)
            },
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
        )?;

        for session in self.sessions.list() {
            self.init_for(&session)?;
        }
        Ok(())
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
            anyhow::bail!("JSONL persistence dispose failed: {}", errors.join("; "))
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
                return Err(Arc::new("JSONL persistence was disposed".to_owned()));
            };
            backend
                .initialize_live(&owned_session, &seed)
                .await
                .map_err(|error| Arc::new(error.to_string()))
        }
        .boxed()
        .shared();

        let weak = Arc::downgrade(self);
        let id = session.id().clone();
        let ready = init.clone();
        let writes = SessionWriteBehind::new(
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
                        .ok_or_else(|| anyhow::anyhow!("JSONL persistence was disposed"))?;
                    backend.append_owned(&id, &events).await
                }
            },
            {
                let id = session.id().clone();
                move |error| {
                    tracing::warn!(session = %id, %error, "JSONL background write failed; events retained");
                }
            },
        );
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

    fn attach_prepared(
        self: &Arc<Self>,
        session: &Arc<Session>,
        reservation: &SessionPreparationReservation<JsonlPreparedSource, BackendSession>,
    ) -> anyhow::Result<LiveSessionState> {
        let source = &reservation.source;
        let mut state = (*reservation.state).clone();
        let cursor = source.inspection.events.len();
        anyhow::ensure!(
            source.session.id() == session.id()
                && Arc::ptr_eq(&source.session, session)
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
            let Some(backend) = weak.upgrade() else {
                return Err(Arc::new("JSONL persistence was disposed".to_owned()));
            };
            backend
                .append_owned(&id, &suffix)
                .await
                .map_err(|error| Arc::new(error.to_string()))
        }
        .boxed()
        .shared();
        let weak = Arc::downgrade(self);
        let id = session.id().clone();
        let ready = init.clone();
        let writes = SessionWriteBehind::new(
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
                        .ok_or_else(|| anyhow::anyhow!("JSONL persistence was disposed"))?;
                    backend.append(&id, &events).await
                }
            },
            |_| {},
        );
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
                anyhow::ensure!(
                    existing_owner == owner,
                    "session {id} is already bound to a different live session in this backend (id collision)"
                );
                return Ok(());
            }
            anyhow::ensure!(
                seed_covers_prefix(seed, &tracked.events),
                "session {id} is already persisted with events that do not match this live session (id collision)"
            );
            let cursor = tracked.events.len();
            tracked.owner = Some(owner);
            self.state.lock().insert(id.clone(), tracked);
            return self.append_core(id, &seed[cursor..]).await;
        }

        if let Some(path) = self.find_log(id).await? {
            let stored = self.read_scan(&path, Some(id)).await?;
            anyhow::ensure!(
                stored.scan.meta.cwd == session.header().cwd,
                "session {id} is already persisted at a different cwd (id collision)"
            );
            anyhow::ensure!(
                seed_covers_prefix(seed, &stored.scan.events),
                "session {id} already has a persisted log on disk that does not match this live session (id collision)"
            );
            if let Some(truncate_to) = stored.truncate_to {
                let file = fs::OpenOptions::new().write(true).open(&path).await?;
                file.set_len(u64::try_from(truncate_to)?).await?;
                file.sync_all().await?;
                if !stored.recovered_events.is_empty() {
                    self.append_lines(&path, &stored.recovered_events).await?;
                }
            }
            let cursor = stored.scan.events.len();
            self.state.lock().insert(
                id.clone(),
                BackendSession {
                    header: stored.scan.meta,
                    events: stored.scan.events,
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
                .ok_or_else(|| Arc::new("JSONL persistence was disposed".to_owned()))?;
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
                    tracing::warn!(session = %id, %error, "JSONL session retirement failed");
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
        if current.is_none() && self.find_log(id).await?.is_some() {
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
        validate_normalized_events(&current.header, &combined)?;
        validate_session_events(&combined)?;
        let path = log_path(
            &self.root,
            current.header.cwd.as_deref(),
            id,
            self.compression,
        )?;
        if current.materialized {
            self.append_lines(&path, events).await?;
        } else {
            self.materialize(&current.header, events).await?;
            current.materialized = true;
        }
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

    async fn prepare_core(&self, id: &SessionId) -> anyhow::Result<JsonlPreparedSource> {
        let path = self
            .find_log(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session {id} was not found"))?;
        let stored = self.read_scan(&path, Some(id)).await?;
        let mut balanced = stored.scan.events.clone();
        let closers = interrupted_turn_closers(&balanced);
        balanced.extend(closers.clone());
        validate_session_events(&balanced)?;
        let meta = stored.scan.meta;
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
        Ok(JsonlPreparedSource {
            inspection: SessionInspection {
                meta: session.header().clone(),
                events: balanced,
            },
            session,
            revision: stored.revision,
            session_length,
            path,
            truncate_to: stored.truncate_to,
            recovered_events: stored.recovered_events,
            closers,
        })
    }

    async fn prepared_source_current(&self, source: &JsonlPreparedSource) -> anyhow::Result<bool> {
        match fs::metadata(&source.path).await {
            Ok(metadata) => Ok(revision_identity(&metadata) == source.revision),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    async fn commit_prepared(
        &self,
        source: Arc<JsonlPreparedSource>,
    ) -> anyhow::Result<Option<(Arc<JsonlPreparedSource>, BackendSession)>> {
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
        if !self.prepared_source_current(&source).await? {
            return Ok(None);
        }
        if source.truncate_to.is_some() || !source.closers.is_empty() {
            if let Some(truncate_to) = source.truncate_to {
                let file = fs::OpenOptions::new()
                    .write(true)
                    .open(&source.path)
                    .await?;
                file.set_len(u64::try_from(truncate_to)?).await?;
                file.sync_all().await?;
            }
            let repair = source
                .recovered_events
                .iter()
                .cloned()
                .chain(source.closers.iter().cloned())
                .collect::<Vec<_>>();
            if !repair.is_empty() {
                self.append_lines(&source.path, &repair).await?;
            }
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
    ) -> anyhow::Result<SessionPreparationReservation<JsonlPreparedSource, BackendSession>> {
        loop {
            ensure_not_aborted(signal.as_ref())?;
            self.wait_for_retirement(id).await?;
            ensure_not_aborted(signal.as_ref())?;
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
                            .ok_or_else(|| anyhow::anyhow!("JSONL persistence was disposed"))?;
                        backend.prepare_core(&load_id).await
                    },
                    move |source| async move {
                        let backend = commit_weak
                            .upgrade()
                            .ok_or_else(|| anyhow::anyhow!("JSONL persistence was disposed"))?;
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

    async fn find_log(&self, id: &SessionId) -> anyhow::Result<Option<PathBuf>> {
        self.ensure_root_encoding().await?;
        let encoded = crate::encode_segment(id.as_str())?;
        let mut found = None;
        let mut projects = match fs::read_dir(&self.root).await {
            Ok(projects) => projects,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        while let Some(project) = projects.next_entry().await? {
            if !project.file_type().await?.is_dir() {
                continue;
            }
            for compression in [JsonlCompression::Zstd, JsonlCompression::None] {
                let legacy = project
                    .path()
                    .join(format!("{encoded}{}", compression.suffix()));
                if fs::try_exists(&legacy).await? {
                    anyhow::bail!(
                        "session artifact {} uses the unsupported flat-file layout; use a separate root or move it into a project/session directory before loading",
                        legacy.display()
                    );
                }
            }
            let directory = project.path().join(&encoded);
            let opposite =
                directory.join(format!("session{}", self.compression.opposite().suffix()));
            if fs::try_exists(&opposite).await? {
                anyhow::bail!(
                    "JSONL artifact {} uses the opposite configured encoding",
                    opposite.display()
                );
            }
            let candidate = directory.join(format!("session{}", self.compression.suffix()));
            if !fs::try_exists(&candidate).await? {
                continue;
            }
            anyhow::ensure!(
                found.is_none(),
                "duplicate JSONL session id {id} appears in multiple project directories"
            );
            found = Some(candidate);
        }
        Ok(found)
    }

    async fn ensure_root_encoding(&self) -> anyhow::Result<()> {
        self.encoding_checked
            .get_or_try_init(|| async { self.check_root_encoding().await })
            .await?;
        Ok(())
    }

    async fn check_root_encoding(&self) -> anyhow::Result<()> {
        let mut projects = match fs::read_dir(&self.root).await {
            Ok(projects) => projects,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        while let Some(project) = projects.next_entry().await? {
            if !project.file_type().await?.is_dir() {
                continue;
            }
            let mut entries = fs::read_dir(project.path()).await?;
            while let Some(entry) = entries.next_entry().await? {
                let file_type = entry.file_type().await?;
                if file_type.is_file()
                    && (entry.file_name().to_string_lossy().ends_with(".jsonl")
                        || entry.file_name().to_string_lossy().ends_with(".jsonl.zstd"))
                {
                    anyhow::bail!(
                        "session artifact {} uses the unsupported flat-file layout; use a separate root or move it into a project/session directory before loading",
                        entry.path().display()
                    );
                }
                if !file_type.is_dir() {
                    continue;
                }
                let opposite = entry
                    .path()
                    .join(format!("session{}", self.compression.opposite().suffix()));
                if fs::try_exists(&opposite).await? {
                    anyhow::bail!(
                        "JSONL artifact {} uses the opposite configured encoding",
                        opposite.display()
                    );
                }
            }
        }
        Ok(())
    }

    async fn read_stable(&self, path: &Path) -> anyhow::Result<(Vec<u8>, Metadata)> {
        loop {
            let before = fs::metadata(path).await?;
            let bytes = fs::read(path).await?;
            let after = fs::metadata(path).await?;
            if revision_identity(&before) == revision_identity(&after) {
                return Ok((bytes, after));
            }
        }
    }

    async fn read_scan(
        &self,
        path: &Path,
        expected_id: Option<&SessionId>,
    ) -> anyhow::Result<StoredScan> {
        let (bytes, metadata) = self.read_stable(path).await?;
        let mut stored = self
            .scan_artifact(&bytes)
            .map_err(|error| enrich_format_error(error, path))?;
        stored.revision = revision_identity(&metadata);
        let scan = &stored.scan;
        if let Some(expected_id) = expected_id {
            anyhow::ensure!(
                scan.meta.id == *expected_id,
                "corrupt session log: header id {} does not match requested id {}",
                scan.meta.id,
                expected_id
            );
        }
        let expected_path = log_path(
            &self.root,
            scan.meta.cwd.as_deref(),
            &scan.meta.id,
            self.compression,
        )?;
        anyhow::ensure!(
            expected_path == path || same_file(&expected_path, path).await?,
            "corrupt session log: header location does not match artifact path {}",
            path.display()
        );
        let normalized = normalize_stored_events(&stored.scan.events, &stored.scan.meta.id)?;
        assert_known_events(&normalized, &stored.scan.meta.id).map_err(|error| {
            let location = SessionLocation {
                kind: "jsonl".to_owned(),
                path: path.to_owned(),
            };
            SessionFormatUnsupportedError::new(
                format!("{error} (raw log: {})", path.display()),
                Some(location),
            )
        })?;
        validate_normalized_events(&stored.scan.meta, &normalized)?;
        validate_session_events(&normalized)?;
        stored.scan.events = normalized;
        Ok(stored)
    }

    fn scan_artifact(&self, bytes: &[u8]) -> anyhow::Result<StoredScan> {
        match self.compression {
            JsonlCompression::None => {
                let scan = scan_log(bytes)?;
                let truncate_to =
                    (scan.committed_bytes < bytes.len()).then_some(scan.committed_bytes);
                Ok(StoredScan {
                    scan,
                    truncate_to,
                    recovered_events: Vec::new(),
                    revision: String::new(),
                })
            }
            JsonlCompression::Zstd => Self::scan_zstd_artifact(bytes),
        }
    }

    fn scan_zstd_artifact(bytes: &[u8]) -> anyhow::Result<StoredScan> {
        let structure = scan_zstd_frames(bytes, None)?;
        anyhow::ensure!(
            !structure.frames.is_empty(),
            "empty or header-less Zstandard session log"
        );
        let header =
            decompress_zstd_frame(&bytes[structure.frames[0].start..structure.frames[0].end])?;
        assert_zstd_header_frame(&header)?;
        let mut scanner = SessionLogScanner::new(&header)?;
        for frame in structure.frames.iter().skip(1) {
            let plaintext = decompress_zstd_frame(&bytes[frame.start..frame.end])?;
            scanner.write(&plaintext)?;
        }
        let complete = scanner.checkpoint();
        anyhow::ensure!(
            complete.committed_bytes == complete.input_bytes,
            "corrupt Zstandard session log: complete frame contains a torn JSONL record"
        );
        let complete_event_count = complete.event_count;
        let Some(torn_start) = structure.torn_start else {
            return Ok(StoredScan {
                scan: scanner.finish(),
                truncate_to: None,
                recovered_events: Vec::new(),
                revision: String::new(),
            });
        };

        let recovered_plaintext =
            decompress_zstd_prefix(&bytes[torn_start..]).unwrap_or_else(|_| Vec::new());
        scanner.write(&recovered_plaintext)?;
        let recovered = scanner.finish();
        let recovered_events = recovered.events[complete_event_count..].to_vec();
        Ok(StoredScan {
            scan: recovered,
            truncate_to: Some(torn_start),
            recovered_events,
            revision: String::new(),
        })
    }

    fn decode_committed_content(&self, bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
        if self.compression == JsonlCompression::None {
            return Ok(bytes.to_vec());
        }
        let structure = scan_zstd_frames(bytes, None)?;
        anyhow::ensure!(
            !structure.frames.is_empty(),
            "empty or header-less Zstandard session log"
        );
        let mut plaintext = Vec::new();
        for frame in structure.frames {
            plaintext.extend(decompress_zstd_frame(&bytes[frame.start..frame.end])?);
        }
        Ok(plaintext)
    }

    async fn cold_inspection(
        &self,
        id: &SessionId,
        commit_repair: bool,
    ) -> anyhow::Result<SessionInspection> {
        let path = self
            .find_log(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session {id} was not found"))?;
        let stored = self.read_scan(&path, Some(id)).await?;
        let mut events = stored.scan.events.clone();
        let closers = interrupted_turn_closers(&events);
        if commit_repair {
            if let Some(truncate_to) = stored.truncate_to {
                let file = fs::OpenOptions::new().write(true).open(&path).await?;
                file.set_len(u64::try_from(truncate_to)?).await?;
                file.sync_all().await?;
            }
            let repaired = stored
                .recovered_events
                .iter()
                .cloned()
                .chain(closers.iter().cloned())
                .collect::<Vec<_>>();
            if !repaired.is_empty() {
                self.append_lines(&path, &repaired).await?;
            }
        }
        events.extend(closers);
        validate_session_events(&events)?;
        if commit_repair {
            self.state.lock().insert(
                id.clone(),
                BackendSession {
                    header: stored.scan.meta.clone(),
                    events: events.clone(),
                    materialized: true,
                    owner: None,
                },
            );
        }
        Ok(SessionInspection {
            meta: stored.scan.meta,
            events,
        })
    }

    async fn append_lines(&self, path: &Path, events: &[SessionEvent]) -> anyhow::Result<()> {
        if events.is_empty() {
            return Ok(());
        }
        let lines = format!("{}\n", event_lines(events, self.pack_chunks)?);
        let content = match self.compression {
            JsonlCompression::None => lines.into_bytes(),
            JsonlCompression::Zstd => compress_zstd_frame(lines.as_bytes())?,
        };
        let mut file = fs::OpenOptions::new().append(true).open(path).await?;
        let before = file.metadata().await?.len();
        if let Err(write_error) = async {
            file.write_all(&content).await?;
            file.sync_all().await
        }
        .await
        {
            drop(file);
            let rollback = async {
                let rollback_file = fs::OpenOptions::new().write(true).open(path).await?;
                rollback_file.set_len(before).await?;
                rollback_file.sync_all().await
            }
            .await;
            if let Err(rollback_error) = rollback {
                anyhow::bail!(
                    "failed to append {} and failed to roll it back: append: {}; rollback: {}",
                    path.display(),
                    write_error,
                    rollback_error
                );
            }
            return Err(write_error.into());
        }
        Ok(())
    }

    async fn materialize(
        &self,
        header: &SessionHeader,
        events: &[SessionEvent],
    ) -> anyhow::Result<()> {
        self.ensure_root_encoding().await?;
        let project = crate::project_dir(&self.root, header.cwd.as_deref())?;
        let directory = session_dir(&self.root, header.cwd.as_deref(), &header.id)?;
        Self::create_durable_directory(&self.root, self.root.parent()).await?;
        Self::create_durable_directory(&project, Some(&self.root)).await?;
        Self::create_durable_directory(&directory, Some(&project)).await?;
        let final_path = log_path(
            &self.root,
            header.cwd.as_deref(),
            &header.id,
            self.compression,
        )?;
        let opposite = log_path(
            &self.root,
            header.cwd.as_deref(),
            &header.id,
            self.compression.opposite(),
        )?;
        anyhow::ensure!(
            !fs::try_exists(&opposite).await?,
            "opposite-encoding JSONL artifact already exists at {}",
            opposite.display()
        );
        anyhow::ensure!(
            !fs::try_exists(&final_path).await?,
            "refusing to materialize {}: a log already exists on disk (load/resume it instead)",
            header.id
        );
        let temporary = directory.join(format!(".session-{}.tmp", Uuid::new_v4()));
        let write_result = async {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .await?;
            let header_content = format!("{}\n", header_line(header)?);
            match self.compression {
                JsonlCompression::None => file.write_all(header_content.as_bytes()).await?,
                JsonlCompression::Zstd => {
                    file.write_all(&compress_zstd_frame(header_content.as_bytes())?)
                        .await?;
                }
            }
            if !events.is_empty() {
                let body = format!("{}\n", event_lines(events, self.pack_chunks)?);
                match self.compression {
                    JsonlCompression::None => file.write_all(body.as_bytes()).await?,
                    JsonlCompression::Zstd => {
                        file.write_all(&compress_zstd_frame(body.as_bytes())?)
                            .await?;
                    }
                }
            }
            file.sync_all().await?;
            fs::hard_link(&temporary, &final_path).await?;
            sync_directory(&directory).await?;
            anyhow::Ok(())
        }
        .await;
        let _ = fs::remove_file(&temporary).await;
        write_result
    }

    async fn create_durable_directory(
        directory: &Path,
        parent: Option<&Path>,
    ) -> anyhow::Result<()> {
        fs::create_dir_all(directory).await?;
        if let Some(parent) = parent
            && parent.exists()
        {
            sync_directory(parent).await?;
        }
        Ok(())
    }

    async fn list_artifacts(&self) -> anyhow::Result<Vec<(SessionHeader, PathBuf, Metadata)>> {
        self.ensure_root_encoding().await?;
        let mut output = Vec::new();
        let mut ids = HashSet::new();
        let mut projects = match fs::read_dir(&self.root).await {
            Ok(projects) => projects,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(output),
            Err(error) => return Err(error.into()),
        };
        while let Some(project) = projects.next_entry().await? {
            if !project.file_type().await?.is_dir() {
                continue;
            }
            let mut sessions = fs::read_dir(project.path()).await?;
            while let Some(directory) = sessions.next_entry().await? {
                let file_type = directory.file_type().await?;
                if file_type.is_file()
                    && (directory.file_name().to_string_lossy().ends_with(".jsonl")
                        || directory
                            .file_name()
                            .to_string_lossy()
                            .ends_with(".jsonl.zstd"))
                {
                    anyhow::bail!(
                        "session artifact {} uses the unsupported flat-file layout; use a separate root or move it into a project/session directory before loading",
                        directory.path().display()
                    );
                }
                if !file_type.is_dir() {
                    continue;
                }
                let opposite = directory
                    .path()
                    .join(format!("session{}", self.compression.opposite().suffix()));
                if fs::try_exists(&opposite).await? {
                    anyhow::bail!(
                        "JSONL artifact {} uses the opposite configured encoding",
                        opposite.display()
                    );
                }
                let path = directory
                    .path()
                    .join(format!("session{}", self.compression.suffix()));
                if !fs::try_exists(&path).await? {
                    continue;
                }
                let Some(line) = self.read_first_header_line(&path).await? else {
                    continue;
                };
                let Some(header) = parse_header_meta(&line)? else {
                    continue;
                };
                let expected = log_path(
                    &self.root,
                    header.cwd.as_deref(),
                    &header.id,
                    self.compression,
                )?;
                anyhow::ensure!(
                    expected == path || same_file(&expected, &path).await?,
                    "corrupt session log: header location does not match artifact path {}",
                    path.display()
                );
                anyhow::ensure!(
                    ids.insert(header.id.clone()),
                    "duplicate JSONL session id {} appears in multiple project directories",
                    header.id
                );
                let metadata = fs::metadata(&path).await?;
                output.push((header, path, metadata));
            }
        }
        Ok(output)
    }

    async fn read_first_header_line(&self, path: &Path) -> anyhow::Result<Option<String>> {
        let mut file = fs::File::open(path).await?;
        let mut content = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            let read = file.read(&mut chunk).await?;
            if read == 0 {
                return Ok(None);
            }
            content.extend_from_slice(&chunk[..read]);
            match self.compression {
                JsonlCompression::None => {
                    if let Some(newline) = content.iter().position(|byte| *byte == b'\n') {
                        let line = std::str::from_utf8(&content[..newline])?.to_owned();
                        return Ok(Some(line));
                    }
                }
                JsonlCompression::Zstd => {
                    let structure = scan_zstd_frames(&content, Some(1))?;
                    let Some(frame) = structure.frames.first() else {
                        continue;
                    };
                    let plaintext = decompress_zstd_frame(&content[frame.start..frame.end])?;
                    assert_zstd_header_frame(&plaintext)?;
                    let line = std::str::from_utf8(&plaintext[..plaintext.len() - 1])?.to_owned();
                    return Ok(Some(line));
                }
            }
        }
    }
}

#[async_trait]
impl SessionPersistence for JsonlSessionPersistence {
    fn locate(&self, meta: &SessionHeader) -> Option<SessionLocation> {
        log_path(&self.root, meta.cwd.as_deref(), &meta.id, self.compression)
            .ok()
            .map(|path| SessionLocation {
                kind: "jsonl".to_owned(),
                path,
            })
    }

    fn supports_raw_artifacts(&self) -> bool {
        true
    }

    async fn read_raw(
        &self,
        id: &SessionId,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Option<SessionRawArtifact>> {
        ensure_not_aborted(signal.as_ref())?;
        self.wait_for_retirement(id).await?;
        ensure_not_aborted(signal.as_ref())?;
        let Some(path) = self.find_log(id).await? else {
            return Ok(None);
        };
        let (bytes, _) = self.read_stable(&path).await?;
        ensure_not_aborted(signal.as_ref())?;
        let content = String::from_utf8(self.decode_committed_content(&bytes)?)?;
        let first = content.split('\n').next().unwrap_or_default();
        let meta = parse_header_meta(first)?
            .ok_or_else(|| anyhow::anyhow!("corrupt session log: invalid header line"))?;
        anyhow::ensure!(meta.id == *id, "corrupt session log: header id mismatch");
        Ok(Some(SessionRawArtifact {
            meta,
            filename: "session.jsonl".to_owned(),
            content,
        }))
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
            self.find_log(&meta.id).await?.is_none(),
            "session {} already has a persisted log on disk; load/resume it instead of creating",
            meta.id
        );
        let mut state = self.state.lock();
        anyhow::ensure!(
            !state.contains_key(&meta.id),
            "session {} is already registered with persistence",
            meta.id
        );
        state.insert(
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
            let reusable = source.session.events().len() == source.session_length;
            pool.release(&reservation, reusable);
        }))
    }

    async fn load(&self, id: &SessionId) -> anyhow::Result<SessionInspection> {
        self.wait_for_retirement(id).await?;
        if let Some(live) = self.sessions.get(id) {
            let events = live.events();
            self.flush_existing(&live).await?;
            let balance = validate_session_events(&events)?;
            anyhow::ensure!(
                balance.open_turn.is_none(),
                "cannot crash-repair live session {id} with an open turn"
            );
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
        ensure_not_aborted(signal.as_ref())?;
        if let Some(live) = self.sessions.get(id) {
            return Ok(SessionInspection {
                meta: live.header().clone(),
                events: live.events(),
            });
        }
        loop {
            let weak = self.self_weak.clone();
            let load_id = id.clone();
            let source = self
                .preparations
                .inspect(
                    id,
                    move || async move {
                        let backend = weak
                            .upgrade()
                            .ok_or_else(|| anyhow::anyhow!("JSONL persistence was disposed"))?;
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
            if self.prepared_source_current(&source).await? {
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
        ensure_not_aborted(signal.as_ref())?;
        self.wait_for_retirement(id).await?;
        ensure_not_aborted(signal.as_ref())?;
        let path = self
            .find_log(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("session {id} was not found"))?;
        let stored = self.read_scan(&path, Some(id)).await?;
        ensure_not_aborted(signal.as_ref())?;
        Ok(SessionInspection {
            meta: stored.scan.meta,
            events: stored
                .scan
                .events
                .into_iter()
                .filter(|event| event.seq >= from_seq)
                .collect(),
        })
    }

    async fn list(&self, signal: Option<AbortSignal>) -> anyhow::Result<Vec<SessionHeader>> {
        ensure_not_aborted(signal.as_ref())?;
        let artifacts = self.list_artifacts().await?;
        ensure_not_aborted(signal.as_ref())?;
        Ok(artifacts.into_iter().map(|(header, _, _)| header).collect())
    }

    async fn list_snapshots(
        &self,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SessionPersistenceSnapshot>> {
        ensure_not_aborted(signal.as_ref())?;
        let artifacts = self.list_artifacts().await?;
        ensure_not_aborted(signal.as_ref())?;
        artifacts
            .into_iter()
            .map(|(header, path, metadata)| {
                Ok(SessionPersistenceSnapshot {
                    header,
                    revision: SessionPersistenceRevision::new(format!(
                        "{}:{}",
                        path.display(),
                        revision_identity(&metadata)
                    )),
                })
            })
            .collect()
    }
}

fn ensure_not_aborted(signal: Option<&AbortSignal>) -> anyhow::Result<()> {
    ensure_persistence_not_aborted(signal)
}

fn required_session(args: &EventArgs) -> anyhow::Result<Arc<Session>> {
    args.get::<Session>(0)
        .ok_or_else(|| anyhow::anyhow!("session event lacks a session"))
}

fn session_key(session: &Arc<Session>) -> usize {
    Arc::as_ptr(session) as usize
}

fn seed_covers_prefix(seed: &[SessionEvent], prefix: &[SessionEvent]) -> bool {
    prefix.len() <= seed.len() && prefix.iter().zip(seed).all(|(stored, live)| stored == live)
}

fn enrich_format_error(error: anyhow::Error, path: &Path) -> anyhow::Error {
    match error.downcast::<SessionFormatUnsupportedError>() {
        Ok(unsupported) if unsupported.location.is_none() => SessionFormatUnsupportedError::new(
            format!("{} (raw log: {})", unsupported.message, path.display()),
            Some(SessionLocation {
                kind: "jsonl".to_owned(),
                path: path.to_owned(),
            }),
        )
        .into(),
        Ok(unsupported) => unsupported.into(),
        Err(error) => error,
    }
}

fn assert_zstd_header_frame(plaintext: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(
        !plaintext.is_empty()
            && plaintext.last() == Some(&b'\n')
            && plaintext[..plaintext.len() - 1]
                .iter()
                .all(|byte| *byte != b'\n'),
        "corrupt Zstandard session log: first frame is not exactly one header line"
    );
    Ok(())
}

async fn sync_directory(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        let directory = fs::File::open(path).await?;
        directory.sync_all().await?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

async fn same_file(first: &Path, second: &Path) -> anyhow::Result<bool> {
    match (
        fs::canonicalize(first).await,
        fs::canonicalize(second).await,
    ) {
        (Ok(first), Ok(second)) => Ok(first == second),
        (Err(error), _) | (_, Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(false)
        }
        (Err(error), _) | (_, Err(error)) => Err(error.into()),
    }
}

#[cfg(unix)]
fn revision_identity(metadata: &Metadata) -> String {
    use std::os::unix::fs::MetadataExt as _;

    format!(
        "{}:{}:{}:{}:{}:{}:{}",
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec()
    )
}

#[cfg(not(unix))]
fn revision_identity(metadata: &Metadata) -> String {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_nanos());
    format!("{}:{modified}", metadata.len())
}

#[cfg(test)]
mod tests {
    use seekdeep_cordis::{Context, Fiber};
    use seekdeep_core::session::{AppendOptions, Session};
    use seekdeep_core::session_store::CreateSessionOptions;
    use seekdeep_session_persistence::SessionPersistence;
    use serde_json::json;

    use super::*;

    fn backend(root: &Path) -> (Arc<JsonlSessionPersistence>, Arc<SessionStore>, Context) {
        backend_with_compression(root, JsonlCompression::None)
    }

    fn backend_with_compression(
        root: &Path,
        compression: JsonlCompression,
    ) -> (Arc<JsonlSessionPersistence>, Arc<SessionStore>, Context) {
        let context = Context::new();
        let sessions = SessionStore::install(&context).expect("sessions");
        let backend = JsonlSessionPersistence::new(
            sessions.clone(),
            JsonlConfig {
                root: root.to_owned(),
                pack_chunks: true,
                compression,
                write_batch_max_delay_ms: 200,
                prepared_session_cache_size: 5,
            },
        )
        .expect("backend");
        (backend, sessions, context)
    }

    fn balanced_session(id: &str) -> Arc<Session> {
        let session = Session::create(&SessionId::new(id), None, None).expect("session");
        session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("turn start");
        session
            .append(
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
                AppendOptions::default(),
            )
            .expect("turn end");
        session
    }

    #[tokio::test]
    async fn create_is_lazy_then_append_materializes_and_lists_exact_artifact() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let (backend, _sessions, _context) = backend(temporary.path());
        let session = balanced_session("lazy");
        backend.create(session.header()).await.expect("create");
        assert!(backend.list(None).await.expect("list").is_empty());
        let location = backend.locate(session.header()).expect("location");
        assert!(!location.path.exists());

        backend
            .append(session.id(), &session.events())
            .await
            .expect("append");
        assert!(location.path.exists());
        let mut persisted_header = session.header().clone();
        persisted_header.delegation_depth = Some(0);
        assert_eq!(
            backend.list(None).await.expect("list"),
            vec![persisted_header]
        );
        let inspection = backend.inspect(session.id(), None).await.expect("inspect");
        assert_eq!(inspection.events, session.events());
        let raw = backend
            .read_raw(session.id(), None)
            .await
            .expect("raw")
            .expect("artifact");
        assert_eq!(raw.filename, "session.jsonl");
        assert!(raw.content.ends_with('\n'));
        assert_eq!(raw.content.lines().count(), 3);
        let snapshots = backend.list_snapshots(None).await.expect("snapshots");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].header.id, *session.id());
    }

    #[tokio::test]
    async fn torn_final_record_is_ignored_by_inspect_and_truncated_by_load() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let (backend, _sessions, _context) = backend(temporary.path());
        let session = balanced_session("torn");
        backend.create(session.header()).await.expect("create");
        backend
            .append(session.id(), &session.events())
            .await
            .expect("append");
        let path = backend.locate(session.header()).expect("location").path;
        let clean_len = fs::metadata(&path).await.expect("metadata").len();
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .expect("open");
        file.write_all(br#"{"type":"torn""#)
            .await
            .expect("torn write");
        file.sync_all().await.expect("sync");
        let torn_len = fs::metadata(&path).await.expect("metadata").len();
        assert!(torn_len > clean_len);
        assert_eq!(
            backend
                .inspect(session.id(), None)
                .await
                .expect("inspect")
                .events,
            session.events()
        );
        assert_eq!(
            fs::metadata(&path).await.expect("still torn").len(),
            torn_len
        );
        assert_eq!(
            backend.load(session.id()).await.expect("load").events,
            session.events()
        );
        assert_eq!(
            fs::metadata(&path).await.expect("truncated").len(),
            clean_len
        );
    }

    #[tokio::test]
    async fn cold_open_turn_is_balanced_in_memory_then_committed_on_load() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let (writer, _sessions, _context) = backend(temporary.path());
        let id = SessionId::new("crash-tail");
        let session = Session::create(&id, None, None).expect("session");
        let start = session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("turn start");
        writer.create(session.header()).await.expect("create");
        writer
            .append(&id, &[start])
            .await
            .expect("append open turn");
        let before = writer
            .read_raw(&id, None)
            .await
            .expect("raw")
            .expect("artifact")
            .content;
        assert!(!before.contains("turn/end"));
        drop(writer);

        let (reader, _sessions, _context) = backend(temporary.path());
        let inspected = reader.inspect(&id, None).await.expect("inspect repair");
        assert_eq!(
            inspected.events.last().expect("synthetic").event_type,
            "turn/end"
        );
        let still_open = reader
            .read_raw(&id, None)
            .await
            .expect("raw")
            .expect("artifact")
            .content;
        assert!(!still_open.contains("turn/end"));
        let loaded = reader.load(&id).await.expect("load repair");
        assert_eq!(loaded.events, inspected.events);
        let repaired = reader
            .read_raw(&id, None)
            .await
            .expect("raw")
            .expect("artifact")
            .content;
        assert!(repaired.contains("turn/end"));
        validate_session_events(&loaded.events).expect("balanced");
    }

    #[tokio::test]
    async fn append_rejects_noncontiguous_batches_without_changing_bytes() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let (backend, _sessions, _context) = backend(temporary.path());
        let session = balanced_session("gap");
        backend.create(session.header()).await.expect("create");
        let mut bad = session.events();
        bad[0].seq = 1;
        let error = backend.append(session.id(), &bad).await.expect_err("gap");
        assert!(error.to_string().contains("begin at seq 0"));
        assert!(backend.list(None).await.expect("list").is_empty());
    }

    #[tokio::test]
    async fn zstd_uses_one_checksummed_frame_per_durable_batch_and_exports_jsonl() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let (backend, _sessions, _context) =
            backend_with_compression(temporary.path(), JsonlCompression::Zstd);
        let session = balanced_session("compressed");
        backend.create(session.header()).await.expect("create");
        backend
            .append(session.id(), &session.events())
            .await
            .expect("first batch");
        let path = backend.locate(session.header()).expect("location").path;
        assert_eq!(
            path.extension().and_then(std::ffi::OsStr::to_str),
            Some("zstd")
        );
        let first_bytes = fs::read(&path).await.expect("read");
        let first_scan = scan_zstd_frames(&first_bytes, None).expect("frames");
        assert_eq!(first_scan.frames.len(), 2);
        assert_eq!(first_scan.torn_start, None);
        for frame in &first_scan.frames {
            assert_ne!(first_bytes[frame.start + 4] & 0x04, 0, "checksum bit");
        }
        let header_plaintext = decompress_zstd_frame(
            &first_bytes[first_scan.frames[0].start..first_scan.frames[0].end],
        )
        .expect("header frame");
        assert_zstd_header_frame(&header_plaintext).expect("exact header frame");

        let next = SessionEvent {
            event_type: "session/title".to_owned(),
            seq: 2,
            time: 10,
            data: json!({}),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        };
        backend
            .append(session.id(), std::slice::from_ref(&next))
            .await
            .expect("second batch");
        let bytes = fs::read(&path).await.expect("read appended");
        assert_eq!(
            scan_zstd_frames(&bytes, None).expect("frames").frames.len(),
            3
        );
        let raw = backend
            .read_raw(session.id(), None)
            .await
            .expect("raw")
            .expect("artifact");
        assert!(raw.content.starts_with("{\"type\":\"session\""));
        assert!(raw.content.ends_with('\n'));
        assert_eq!(raw.content.lines().count(), 4);
        assert_eq!(
            backend
                .inspect(session.id(), None)
                .await
                .expect("inspect")
                .events,
            [session.events(), vec![next]].concat()
        );
    }

    #[tokio::test]
    async fn zstd_torn_checksum_recovers_rows_then_load_reframes_them() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let (writer, _sessions, _context) =
            backend_with_compression(temporary.path(), JsonlCompression::Zstd);
        let session = balanced_session("compressed-torn");
        writer.create(session.header()).await.expect("create");
        writer
            .append(session.id(), &session.events())
            .await
            .expect("append");
        let path = writer.locate(session.header()).expect("location").path;
        let bytes = fs::read(&path).await.expect("read");
        let frames = scan_zstd_frames(&bytes, None).expect("scan").frames;
        assert_eq!(frames.len(), 2);
        let original_end = frames[1].end;
        drop(writer);
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .await
            .expect("open");
        file.set_len(u64::try_from(original_end - 2).expect("length"))
            .await
            .expect("tear checksum");
        file.sync_all().await.expect("sync");

        let (reader, _sessions, _context) =
            backend_with_compression(temporary.path(), JsonlCompression::Zstd);
        assert_eq!(
            reader
                .inspect(session.id(), None)
                .await
                .expect("recover in memory")
                .events,
            session.events()
        );
        let repaired = reader.load(session.id()).await.expect("commit repair");
        assert_eq!(repaired.events, session.events());
        let repaired_bytes = fs::read(&path).await.expect("read repaired");
        let repaired_frames = scan_zstd_frames(&repaired_bytes, None).expect("scan repaired");
        assert_eq!(repaired_frames.frames.len(), 2);
        assert_eq!(repaired_frames.torn_start, None);
    }

    #[tokio::test]
    async fn zstd_complete_frame_checksum_corruption_is_fatal() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let (backend, _sessions, _context) =
            backend_with_compression(temporary.path(), JsonlCompression::Zstd);
        let session = balanced_session("bad-checksum");
        backend.create(session.header()).await.expect("create");
        backend
            .append(session.id(), &session.events())
            .await
            .expect("append");
        let path = backend.locate(session.header()).expect("location").path;
        let mut bytes = fs::read(&path).await.expect("read");
        let last = bytes.last_mut().expect("checksum byte");
        *last ^= 0xFF;
        fs::write(&path, bytes).await.expect("corrupt");
        let error = backend
            .inspect(session.id(), None)
            .await
            .expect_err("checksum failure");
        assert!(!error.to_string().is_empty());
    }

    #[tokio::test]
    async fn live_store_events_flush_to_durable_storage_and_survive_reopen() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let (writer, sessions, context) = backend(temporary.path());
        let session = sessions
            .create(
                &context,
                Some(SessionId::new("live")),
                CreateSessionOptions::default(),
            )
            .expect("live session");
        session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("turn start");
        session
            .append(
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
                AppendOptions::default(),
            )
            .expect("turn end");
        assert!(sessions.flush(&session).await.expect("flush listener"));
        let stored = writer.inspect(session.id(), None).await.expect("inspect");
        assert_eq!(stored.events, session.events());

        drop(writer);
        let (reopened, _sessions, _context) = backend(temporary.path());
        assert_eq!(
            reopened.load(session.id()).await.expect("reopen").events,
            session.events()
        );
    }

    #[tokio::test]
    async fn hot_mount_seeds_an_existing_live_session_once() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let context = Context::new();
        let sessions = SessionStore::install(&context).expect("sessions");
        let session = sessions
            .create(
                &context,
                Some(SessionId::new("hot")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("start");
        session
            .append(
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
                AppendOptions::default(),
            )
            .expect("end");
        let backend = JsonlSessionPersistence::new(
            sessions.clone(),
            JsonlConfig {
                root: temporary.path().to_owned(),
                pack_chunks: true,
                compression: JsonlCompression::None,
                write_batch_max_delay_ms: 200,
                prepared_session_cache_size: 5,
            },
        )
        .expect("mount");
        sessions.flush(&session).await.expect("flush");
        assert_eq!(
            backend
                .inspect(session.id(), None)
                .await
                .expect("inspect")
                .events,
            session.events()
        );
        sessions.flush(&session).await.expect("no-op flush");
        assert_eq!(
            backend
                .inspect(session.id(), None)
                .await
                .expect("inspect")
                .events,
            session.events()
        );
    }

    #[tokio::test]
    async fn context_teardown_drains_buffered_live_events() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let context = Context::new();
        let sessions = SessionStore::install(&context).expect("sessions");
        let backend = JsonlSessionPersistence::new(
            sessions.clone(),
            JsonlConfig {
                root: temporary.path().to_owned(),
                pack_chunks: true,
                compression: JsonlCompression::None,
                write_batch_max_delay_ms: 60_000,
                prepared_session_cache_size: 5,
            },
        )
        .expect("backend");
        let session = sessions
            .create(
                &context,
                Some(SessionId::new("dispose-drain")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("start");
        session
            .append(
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
                AppendOptions::default(),
            )
            .expect("end");
        assert!(
            backend.live.lock()[&session_key(&session)]
                .writes
                .has_work()
        );
        context.fiber().restart().await.expect("teardown");
        assert_eq!(
            backend
                .cold_inspection(session.id(), false)
                .await
                .expect("durable after teardown")
                .events,
            session.events()
        );
    }

    #[tokio::test]
    async fn inspect_then_prepare_reuses_exact_session_and_publication_attaches_reservation() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let (backend, sessions, _context) = backend(temporary.path());
        let stored = balanced_session("prepared");
        backend.create(stored.header()).await.expect("create");
        backend
            .append(stored.id(), &stored.events())
            .await
            .expect("append");
        let inspected = backend.inspect(stored.id(), None).await.expect("inspect");
        assert_eq!(inspected.events, stored.events());

        let first = backend
            .prepare(&sessions, stored.id(), None)
            .await
            .expect("first preparation");
        let first_session = first.session().clone();
        drop(first);
        let second = backend
            .prepare(&sessions, stored.id(), None)
            .await
            .expect("reused preparation");
        assert!(Arc::ptr_eq(&first_session, second.session()));
        assert_eq!(second.session().first_live_seq(), 2);
        assert_eq!(
            second
                .session()
                .events()
                .last()
                .expect("end seed")
                .event_type,
            "session/end-seed"
        );

        let resumed = second.session().clone();
        let detach = sessions.enter(&resumed).expect("enter");
        sessions
            .announce(&resumed)
            .expect("announce exact reservation");
        drop(second);
        sessions.flush(&resumed).await.expect("flush resume suffix");
        let raw = backend
            .read_raw(resumed.id(), None)
            .await
            .expect("raw")
            .expect("artifact");
        assert!(raw.content.contains("session/end-seed"));
        detach.dispose().await.expect("detach");
    }

    #[tokio::test]
    async fn cached_inspection_rejects_alias_publication_for_same_identity() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let (backend, sessions, _context) = backend(temporary.path());
        let stored = balanced_session("prepared-alias");
        backend.create(stored.header()).await.expect("create");
        backend
            .append(stored.id(), &stored.events())
            .await
            .expect("append");
        backend
            .inspect(stored.id(), None)
            .await
            .expect("cache inspection");
        let alias = Session::create(stored.id(), None, Some(stored.header().clone()))
            .expect("alias session");
        let detach = sessions.enter(&alias).expect("enter alias");
        let error = sessions
            .announce(&alias)
            .expect_err("alias must be rejected");
        assert!(error.to_string().contains("persisted state already owns"));
        detach.dispose().await.expect("detach alias");
    }

    #[tokio::test]
    async fn settled_ownerless_collision_is_not_replayed_during_teardown() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let (backend, sessions, context) = backend(temporary.path());
        let stored = balanced_session("ownerless-collision");
        backend.create(stored.header()).await.expect("create");
        backend
            .append(stored.id(), &stored.events())
            .await
            .expect("append");
        backend.load(stored.id()).await.expect("ownerless load");

        let fresh = sessions
            .create(
                &context,
                Some(stored.id().clone()),
                CreateSessionOptions::default(),
            )
            .expect("fresh collision");
        fresh
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("fresh event");
        assert!(sessions.flush(&fresh).await.is_err());
        context
            .fiber()
            .dispose()
            .await
            .expect("teardown does not replay settled initialization failure");
    }

    #[tokio::test]
    async fn session_disposal_drains_buffered_events_before_releasing_live_owner() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let (backend, sessions, context) = backend(temporary.path());
        let owner_fiber = Fiber::active_child("jsonl buffered owner");
        let owner = context.with_fiber(owner_fiber.clone());
        let session = sessions
            .create(
                &owner,
                Some(SessionId::new("jsonl-buffered-retirement")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("start");
        session
            .append(
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
                AppendOptions::default(),
            )
            .expect("end");
        owner_fiber.dispose().await.expect("dispose owner");

        let loaded = tokio::time::timeout(Duration::from_secs(1), backend.load(session.id()))
            .await
            .expect("retirement-fenced load timed out")
            .expect("load");
        assert_eq!(
            loaded
                .events
                .iter()
                .map(|event| event.seq)
                .collect::<Vec<_>>(),
            [0, 1]
        );
        tokio::time::timeout(Duration::from_secs(1), context.fiber().dispose())
            .await
            .expect("context teardown timed out")
            .expect("dispose context");
    }

    #[tokio::test]
    async fn public_append_and_backend_teardown_are_ordered_by_lifecycle_gate() {
        let temporary = tempfile::tempdir().expect("tempdir");
        let (backend, _sessions, context) = backend(temporary.path());
        let session = balanced_session("operation-gate");
        backend.create(session.header()).await.expect("create");

        let closing = backend.operation_gate.write().await;
        let append = tokio::spawn({
            let backend = backend.clone();
            let id = session.id().clone();
            let events = session.events();
            async move { backend.append(&id, &events).await }
        });
        tokio::task::yield_now().await;
        assert!(!append.is_finished(), "append overtook closing writer");
        drop(closing);
        append.await.expect("append join").expect("append");

        let in_flight = backend.operation_gate.read().await;
        let dispose = tokio::spawn({
            let fiber = context.fiber().clone();
            async move { fiber.dispose().await }
        });
        tokio::task::yield_now().await;
        assert!(
            !dispose.is_finished(),
            "teardown overtook in-flight append lease"
        );
        drop(in_flight);
        dispose.await.expect("dispose join").expect("dispose");
    }
}
