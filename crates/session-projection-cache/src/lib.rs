//! Persisted projection cache: durable per-session checkpoints served through
//! the cold-read ladder (cached row + persistence tail + registry restore).

use std::{collections::HashMap, sync::Arc, time::Duration};

use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::{
    Context, EventArgs, EventOptions, EventReply, Plugin, ServiceKey,
    fiber::{DisposeFuture, EffectHandle},
};
use seekdeep_core::{
    session::{Session, SessionEvent, SessionHeader, SessionId},
    session_store::{SESSIONS, SessionStore},
};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::AbortSignal;
use seekdeep_schemastery::Schema;
use seekdeep_session_persistence::{SESSION_PERSISTENCE, SessionPersistence};
use seekdeep_session_projection::{
    ProjectionCheckpoint, ProjectionSnapshot, SESSION_PROJECTIONS, SessionProjectionRegistry,
};
use seekdeep_storage_domain::{
    DomainFacility, DomainSpec, KvTable, STORAGE_DOMAIN, ValueSchema, define_domain, domain_table,
};
use serde::{Deserialize, Serialize};

/// Cordis plugin name retained by Loader diagnostics.
pub const NAME: &str = "session-projection-cache";
/// Services required before the cache opens its durable domain.
pub const INJECT: &[&str] = &[
    "storageDomain",
    "sessionProjections",
    "sessionPersistence",
    "sessions",
];
/// Typed Cordis seat corresponding to `ctx.sessionProjectionCache`.
pub const SESSION_PROJECTION_CACHE: ServiceKey<SessionProjectionCache> =
    ServiceKey::new("sessionProjectionCache");

/// The stored-log identity a record is bound to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointIdentity {
    /// Session creation timestamp distinguishing one lifecycle.
    pub created_at: u64,
    /// Working directory, when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// One session's stored record: identity plus its checkpoint rows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckpointRecord {
    /// Log identity the rows were folded from.
    pub identity: CheckpointIdentity,
    /// Projection checkpoint rows keyed by projection key.
    pub rows: ProjectionCheckpoint,
}

/// The session-projcache domain declaration.
///
/// # Panics
///
/// Panics only on an invalid hard-coded domain declaration.
#[must_use]
pub fn projection_cache_domain_spec() -> DomainSpec {
    let tables = IndexMap::from([(
        "sessions".to_owned(),
        domain_table(ValueSchema::serde::<CheckpointRecord>()),
    )]);
    let spec = DomainSpec {
        name: "session_projcache".to_owned(),
        version: 3,
        global: None,
        tables,
    };
    define_domain(spec).expect("valid domain spec")
}

/// Plugin config: the two write-behind throttle triggers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Committed events per session forcing a durable checkpoint write.
    pub write_every_events: usize,
    /// Longest time a dirty checkpoint may stay unwritten, in milliseconds.
    pub write_interval_ms: u64,
}

/// Source-compatible Loader schema for both positive integer throttle choices.
#[must_use]
pub fn config_schema() -> Schema {
    Schema::object([
        (
            "writeEveryEvents",
            Schema::number().step(1.0).min(1.0).required(),
        ),
        (
            "writeIntervalMs",
            Schema::number().step(1.0).min(1.0).required(),
        ),
    ])
}

fn validate_config(config: Config) -> anyhow::Result<Config> {
    anyhow::ensure!(
        config.write_every_events > 0,
        "writeEveryEvents must be a positive integer"
    );
    anyhow::ensure!(
        config.write_interval_ms > 0,
        "writeIntervalMs must be a positive integer"
    );
    Ok(config)
}

/// Per-session write-behind bookkeeping.
struct DirtyState {
    pending: usize,
    timer: Option<tokio::task::JoinHandle<()>>,
}

struct PreparedWrite {
    identity: CheckpointIdentity,
    rows: ProjectionCheckpoint,
    flush_live_log: bool,
}

/// Projects a header onto the identity fields a record is bound to.
#[must_use]
pub fn identity_of(header: &SessionHeader) -> CheckpointIdentity {
    CheckpointIdentity {
        created_at: header.created_at,
        cwd: header.cwd.clone(),
    }
}

fn identity_matches(stored: &CheckpointIdentity, expected: &CheckpointIdentity) -> bool {
    stored.created_at == expected.created_at && stored.cwd == expected.cwd
}

/// The persisted projection cache service.
pub struct SessionProjectionCache {
    config: Config,
    table: Arc<KvTable>,
    projections: Arc<SessionProjectionRegistry>,
    persistence: Arc<dyn SessionPersistence>,
    sessions: Arc<SessionStore>,
    dirty: Mutex<HashMap<usize, DirtyState>>,
}

impl std::fmt::Debug for SessionProjectionCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionProjectionCache")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl SessionProjectionCache {
    /// Opens the domain, installs the write path, and constructs the service.
    ///
    /// # Errors
    ///
    /// Returns missing-service, domain-open, or listener-registration failures.
    pub async fn install(context: &Context, config: Config) -> anyhow::Result<Arc<Self>> {
        let config = validate_config(config)?;
        let facility: Arc<DomainFacility> = context
            .get(STORAGE_DOMAIN)
            .ok_or_else(|| anyhow::anyhow!("session-projection-cache requires storageDomain"))?;
        let domain = facility.open(projection_cache_domain_spec()).await?;
        let table = domain.table("sessions")?;
        let domain_for_close = domain.clone();
        context.own(EffectHandle::new(
            "sessionProjectionCache.domainClose",
            move || -> DisposeFuture {
                let domain = domain_for_close.clone();
                Box::pin(async move { domain.close().await })
            },
        ))?;
        let projections: Arc<SessionProjectionRegistry> =
            context.get(SESSION_PROJECTIONS).ok_or_else(|| {
                anyhow::anyhow!("session-projection-cache requires sessionProjections")
            })?;
        let persistence = context
            .get(SESSION_PERSISTENCE)
            .ok_or_else(|| anyhow::anyhow!("session-projection-cache requires sessionPersistence"))?
            .persistence();
        let sessions: Arc<SessionStore> = context
            .get(SESSIONS)
            .ok_or_else(|| anyhow::anyhow!("session-projection-cache requires sessions"))?;

        let service = Arc::new(Self {
            config,
            table,
            projections,
            persistence,
            sessions,
            dirty: Mutex::new(HashMap::new()),
        });
        service.install_write_path(context)?;
        let weak = Arc::downgrade(&service);
        context.own(EffectHandle::synchronous(
            "sessionProjectionCache.timers",
            move || {
                if let Some(service) = weak.upgrade() {
                    service.clear_dirty();
                }
                Ok(())
            },
        ))?;
        context.provide(SESSION_PROJECTION_CACHE, service.clone())?;
        Ok(service)
    }

    /// Returns the validated throttle policy active for this service.
    #[must_use]
    pub const fn config(&self) -> Config {
        self.config
    }

    /// The zero-I/O listing read: whole values viewed straight from the stored
    /// rows, each cut carried with its lowest served-row watermark.
    pub fn cached_snapshot(&self, meta: &SessionHeader) -> Option<ProjectionSnapshot> {
        let record = self.record_for(&meta.id, &identity_of(meta))?;
        let values = self.projections.view_checkpoint(&record.rows).ok()?;
        if values.is_empty() {
            return None;
        }
        let as_of_seq = record.rows.values().map(|row| row.seq).min().unwrap_or(-1);
        Some(ProjectionSnapshot { as_of_seq, values })
    }

    /// Durably checkpoints one live session now.
    ///
    /// # Errors
    ///
    /// Returns checkpoint, flush, or durable-write failures.
    pub async fn write(&self, session: &Arc<Session>) -> anyhow::Result<()> {
        let prepared = self.prepare_write(session)?;
        self.commit_write(session, prepared).await
    }

    fn prepare_write(&self, session: &Arc<Session>) -> anyhow::Result<PreparedWrite> {
        let rows = self.projections.checkpoint(session)?;
        self.mark_clean(session);
        Ok(PreparedWrite {
            identity: identity_of(session.header()),
            rows,
            flush_live_log: self
                .sessions
                .get(session.id())
                .is_some_and(|live| Arc::ptr_eq(&live, session)),
        })
    }

    async fn commit_write(
        &self,
        session: &Arc<Session>,
        prepared: PreparedWrite,
    ) -> anyhow::Result<()> {
        if prepared.flush_live_log {
            self.sessions.flush(session).await?;
        }
        self.put(session.id(), prepared.identity, prepared.rows)
            .await
    }

    /// Cold-reads one persisted session's projections with zero full-log load.
    ///
    /// # Errors
    ///
    /// Returns not-found, persistence-read, or restore failures.
    pub async fn cold_snapshot(
        &self,
        id: &SessionId,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<ProjectionSnapshot> {
        let record = self
            .table
            .get(id.as_str())?
            .and_then(|value| serde_json::from_value::<CheckpointRecord>(value).ok());
        let cached: ProjectionCheckpoint = record
            .as_ref()
            .map_or_else(IndexMap::new, |r| r.rows.clone());
        let floor = self.projections.restore_floor(&cached);
        if floor.is_none() {
            let probe = self.persistence.read_from(id, 0, signal.clone()).await?;
            return Ok(ProjectionSnapshot {
                as_of_seq: probe
                    .events
                    .last()
                    .map_or(-1, |event| i64::try_from(event.seq).unwrap_or(i64::MAX)),
                values: IndexMap::new(),
            });
        }
        let tail = self
            .persistence
            .read_from(id, floor.unwrap_or(0), signal.clone())
            .await?;
        let related = record
            .as_ref()
            .is_none_or(|r| identity_matches(&r.identity, &identity_of(&tail.meta)));
        let restored = if related {
            self.projections
                .restore(&cached, &tail.events, floor.unwrap_or(0))
        } else {
            Err(anyhow::anyhow!("unrelated log identity"))
        };
        let restored = if let Ok(restored) = restored {
            restored
        } else {
            let whole = self.persistence.read_from(id, 0, signal).await?;
            self.projections
                .restore(&IndexMap::new(), &whole.events, 0)?
        };
        self.put_soft(
            id,
            identity_of(&tail.meta),
            restored.checkpoint,
            "cold-read write-back",
        )
        .await;
        Ok(restored.snapshot)
    }

    fn install_write_path(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let weak = Arc::downgrade(self);
        context.events().on_sync(
            context,
            "session/event",
            move |_, args| {
                let session = required_session(&args)?;
                let event = args
                    .get::<SessionEvent>(1)
                    .ok_or_else(|| anyhow::anyhow!("session/event lacks an event"))?;
                if let Some(cache) = weak.upgrade() {
                    cache.on_event(&session, &event);
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
            context,
            "session/disposed",
            move |_, args| {
                let session = required_session(&args)?;
                if let Some(cache) = weak.upgrade() {
                    cache.on_disposed(&session);
                }
                Ok(EventReply::Undefined)
            },
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
        )?;
        Ok(())
    }

    fn on_event(self: &Arc<Self>, session: &Arc<Session>, event: &SessionEvent) {
        if event.event_type == "turn/end" {
            self.start_soft_write(session, "turn/end");
            return;
        }
        let key = session_key(session);
        let interval = self.config.write_interval_ms;
        let every = self.config.write_every_events;
        let mut dirty = self.dirty.lock();
        let state = dirty.entry(key).or_insert_with(|| DirtyState {
            pending: 0,
            timer: None,
        });
        state.pending += 1;
        if state.pending >= every {
            drop(dirty);
            self.start_soft_write(session, "count threshold");
            return;
        }
        if state.timer.is_none() {
            let weak = Arc::downgrade(self);
            let session = session.clone();
            let timer = tokio::time::sleep(Duration::from_millis(interval));
            state.timer = Some(tokio::spawn(async move {
                timer.await;
                if let Some(cache) = weak.upgrade() {
                    cache.timer_fired(&session);
                }
            }));
        }
    }

    fn on_disposed(self: &Arc<Self>, session: &Arc<Session>) {
        self.start_soft_write(session, "detach");
        self.dirty.lock().remove(&session_key(session));
    }

    fn start_soft_write(self: &Arc<Self>, session: &Arc<Session>, trigger: &'static str) {
        let prepared = match self.prepare_write(session) {
            Ok(prepared) => prepared,
            Err(error) => {
                tracing::warn!(session = %session.id(), %error, %trigger, "session projection cache write failed (cache stays stale)");
                return;
            }
        };
        let cache = self.clone();
        let session = session.clone();
        tokio::spawn(async move {
            if let Err(error) = cache.commit_write(&session, prepared).await {
                tracing::warn!(session = %session.id(), %error, %trigger, "session projection cache write failed (cache stays stale)");
            }
        });
    }

    fn timer_fired(self: &Arc<Self>, session: &Arc<Session>) {
        let should_flush = self
            .dirty
            .lock()
            .get_mut(&session_key(session))
            .is_some_and(|state| state.timer.take().is_some());
        if should_flush {
            self.start_soft_write(session, "interval");
        }
    }

    fn mark_clean(&self, session: &Arc<Session>) {
        let mut dirty = self.dirty.lock();
        if let Some(state) = dirty.get_mut(&session_key(session)) {
            state.pending = 0;
            if let Some(timer) = state.timer.take() {
                timer.abort();
            }
        }
    }

    fn clear_dirty(&self) {
        let dirty = std::mem::take(&mut *self.dirty.lock());
        for state in dirty.into_values() {
            if let Some(timer) = state.timer {
                timer.abort();
            }
        }
    }

    async fn put(
        &self,
        id: &SessionId,
        identity: CheckpointIdentity,
        rows: ProjectionCheckpoint,
    ) -> anyhow::Result<()> {
        let record = CheckpointRecord { identity, rows };
        self.table
            .put(id.as_str().to_owned(), serde_json::to_value(record)?)
            .await
    }

    async fn put_soft(
        &self,
        id: &SessionId,
        identity: CheckpointIdentity,
        rows: ProjectionCheckpoint,
        what: &str,
    ) {
        if let Err(error) = self.put(id, identity, rows).await {
            tracing::warn!(session = %id, %error, %what, "session projection cache write-back failed (cache stays stale)");
        }
    }

    fn record_for(
        &self,
        id: &SessionId,
        expected: &CheckpointIdentity,
    ) -> Option<CheckpointRecord> {
        let record: CheckpointRecord = self
            .table
            .get(id.as_str())
            .ok()
            .flatten()
            .and_then(|value| serde_json::from_value(value).ok())?;
        identity_matches(&record.identity, expected).then_some(record)
    }
}

/// Builds the Loader-compatible persisted projection-cache plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, value| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(value)?;
            SessionProjectionCache::install(&context, config).await?;
            Ok(())
        })
    })
    .with_config_validator(|value| {
        config_schema()
            .resolve(value)
            .map_err(|error| anyhow::anyhow!("{error}"))
    })
}

fn required_session(args: &EventArgs) -> anyhow::Result<Arc<Session>> {
    args.get::<Session>(0)
        .ok_or_else(|| anyhow::anyhow!("session event lacks a session"))
}

fn session_key(session: &Arc<Session>) -> usize {
    Arc::as_ptr(session) as usize
}

/// Registers the package's explained empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        "seekdeep-session-projection-cache",
        InvariantInstaller::noop(),
    )
}

#[cfg(test)]
mod tests {
    use seekdeep_invariants::InvariantConfig;

    use super::*;

    #[test]
    fn domain_spec_declares_one_sessions_table_at_version_three() {
        let spec = projection_cache_domain_spec();
        assert_eq!(spec.name, "session_projcache");
        assert_eq!(spec.version, 3);
        assert!(spec.global.is_none());
        assert_eq!(spec.tables.len(), 1);
        assert!(spec.tables.contains_key("sessions"));
    }

    #[test]
    fn identity_matching_binds_a_record_to_its_lifecycle() {
        let header = SessionHeader::new(SessionId::new("s"));
        let identity = identity_of(&header);
        assert_eq!(identity.created_at, header.created_at);
        assert_eq!(identity.cwd, None);
        assert!(identity_matches(&identity, &identity));
        let mut other = identity.clone();
        other.created_at += 1;
        assert!(!identity_matches(&identity, &other));
    }

    #[tokio::test]
    async fn explained_empty_invariant_reserves_and_releases_package_identity() {
        let context = Context::new();
        let registry =
            InvariantRegistry::install(&context, &InvariantConfig::default()).expect("registry");
        let registration = register_invariant(&registry).expect("register");
        assert!(register_invariant(&registry).is_err());
        registration.dispose().await.expect("dispose");
        register_invariant(&registry).expect("replacement");
    }
}
