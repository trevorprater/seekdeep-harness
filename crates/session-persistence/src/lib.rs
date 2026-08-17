//! Backend-neutral durable session persistence contract.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use seekdeep_cordis::{Context, ServiceKey, fiber::EffectHandle};
use seekdeep_core::{
    preparation::SessionPreparation,
    session::{SessionEvent, SessionHeader, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_llm::AbortSignal;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Typed Cordis seat corresponding to `ctx.sessionPersistence`.
pub const SESSION_PERSISTENCE: ServiceKey<SessionPersistenceService> =
    ServiceKey::new("sessionPersistence");

/// Default completed cold-preparation cache capacity.
pub const DEFAULT_PREPARED_SESSION_CACHE_SIZE: usize = 5;
/// Default fixed write-behind coalescing window in milliseconds.
pub const DEFAULT_WRITE_BATCH_MAX_DELAY_MS: u64 = 200;
/// Maximum timer delay supported by the source runtime.
pub const MAX_WRITE_BATCH_DELAY_MS: u64 = 2_147_483_647;
/// Stable invariant companion name.
pub const INVARIANT_NAME: &str = "session-persistence-invariant";
const PACKAGE_NAME: &str = "@deepseek-ai/seekdeep-session-persistence";

/// Object-safe persistence implementation published through Cordis.
#[derive(Clone)]
pub struct SessionPersistenceService(Arc<dyn SessionPersistence>);

impl std::fmt::Debug for SessionPersistenceService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("SessionPersistenceService")
            .field(&"dyn SessionPersistence")
            .finish()
    }
}

impl SessionPersistenceService {
    /// Wraps one concrete or erased persistence implementation.
    #[must_use]
    pub fn new(persistence: Arc<dyn SessionPersistence>) -> Arc<Self> {
        Arc::new(Self(persistence))
    }

    /// Returns the object-safe persistence implementation.
    #[must_use]
    pub fn persistence(&self) -> Arc<dyn SessionPersistence> {
        self.0.clone()
    }

    /// Publishes this implementation on the source-compatible Cordis seat.
    ///
    /// # Errors
    ///
    /// Returns inactive-fiber or duplicate-service failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> anyhow::Result<EffectHandle> {
        Ok(context.provide(SESSION_PERSISTENCE, self.clone())?)
    }
}

/// Registers the package's explained-empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant-registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}

/// Shared cold-load caching and exclusive unpublished-session reservations.
pub mod preparations;
/// Read-time migration and format-support validation for stored event logs.
pub mod stored_events;
/// Bounded per-session live-event batching.
pub mod write_behind;

/// Stored format version cannot be read by this build.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct SessionFormatUnsupportedError {
    /// Human-readable upgrade/refusal diagnostic.
    pub message: String,
    /// Raw artifact involved, when known.
    pub location: Option<SessionLocation>,
}

impl SessionFormatUnsupportedError {
    /// Creates a version refusal with optional artifact location.
    #[must_use]
    pub fn new(message: impl Into<String>, location: Option<SessionLocation>) -> Self {
        Self {
            message: message.into(),
            location,
        }
    }
}

/// A committed stored prefix is structurally inconsistent.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct SessionPersistenceCorruptionError(pub String);

/// Builds the standard unsupported-version refusal.
#[must_use]
pub fn session_format_version_refusal(id: &str, version: &serde_json::Value) -> String {
    let current = seekdeep_core::session::SESSION_FORMAT_VERSION;
    if version
        .as_f64()
        .is_some_and(|value| value > f64::from(current))
    {
        format!(
            "session \"{id}\" uses log format v{version}, but this harness reads only v{current}: the log was written by a newer harness — upgrade the harness to open it"
        )
    } else {
        format!(
            "session \"{id}\" uses log format v{version}, older than the supported v{current}, and this build ships no upgrade path for it"
        )
    }
}

/// Opaque backend/source-qualified log revision.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionPersistenceRevision(String);

impl SessionPersistenceRevision {
    /// Brands one backend-owned revision token.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Exposes the opaque representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Lightweight immutable stored identity and revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionPersistenceSnapshot {
    /// Detached session metadata.
    pub header: SessionHeader,
    /// Source-qualified change token.
    pub revision: SessionPersistenceRevision,
}

/// Validated immutable logical session view.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionInspection {
    /// Durable metadata.
    pub meta: SessionHeader,
    /// Contiguous logical event log.
    pub events: Vec<SessionEvent>,
}

/// Backend-owned raw artifact contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRawArtifact {
    /// Header parsed from the artifact.
    pub meta: SessionHeader,
    /// Base filename without a physical encoding suffix.
    pub filename: String,
    /// Exact decoded artifact text.
    pub content: String,
}

/// Independent local artifact location, when a backend owns one per session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLocation {
    /// Backend-specific artifact kind.
    pub kind: String,
    /// Absolute target path, materialized or not.
    pub path: PathBuf,
}

/// Durable append-only session storage.
#[async_trait]
pub trait SessionPersistence: Send + Sync + 'static {
    /// Resolves a per-session artifact location without materializing it.
    fn locate(&self, meta: &SessionHeader) -> Option<SessionLocation>;

    /// Whether one verbatim raw artifact exists per materialized session.
    fn supports_raw_artifacts(&self) -> bool;

    /// Reads exact decoded backend artifact text.
    async fn read_raw(
        &self,
        _id: &SessionId,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Option<SessionRawArtifact>> {
        ensure_not_aborted(signal.as_ref())?;
        anyhow::bail!("this session persistence backend does not expose raw artifacts")
    }

    /// Registers new immutable session metadata.
    async fn create(&self, meta: &SessionHeader) -> anyhow::Result<()>;

    /// Durably persists one contiguous append batch.
    async fn append(&self, id: &SessionId, events: &[SessionEvent]) -> anyhow::Result<()>;

    /// Loads and durably repairs one balanced logical view.
    async fn load(&self, id: &SessionId) -> anyhow::Result<SessionInspection>;

    /// Inspects a logical view without committing cold recovery.
    async fn inspect(
        &self,
        id: &SessionId,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection>;

    /// Reads the valid stored physical suffix beginning at `from_seq`.
    async fn read_from(
        &self,
        id: &SessionId,
        from_seq: u64,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionInspection>;

    /// Lists materialized session headers without loading full logs.
    async fn list(&self, signal: Option<AbortSignal>) -> anyhow::Result<Vec<SessionHeader>>;

    /// Lists materialized headers and cheap change tokens.
    async fn list_snapshots(
        &self,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SessionPersistenceSnapshot>>;

    /// Prepares the exact unpublished session used by resume.
    async fn prepare(
        &self,
        sessions: &Arc<SessionStore>,
        id: &SessionId,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionPreparation> {
        ensure_not_aborted(signal.as_ref())?;
        let loaded = self.load(id).await?;
        ensure_not_aborted(signal.as_ref())?;
        anyhow::ensure!(
            loaded.meta.id == *id,
            "persisted session header id {:?} does not match requested id {:?}",
            loaded.meta.id,
            id
        );
        let session = sessions.prepare(
            Some(id.clone()),
            CreateSessionOptions {
                seed: Some(loaded.events),
                cwd: loaded.meta.cwd,
                parent_session: loaded.meta.parent_session,
                created_at: Some(loaded.meta.created_at),
                seed_length: loaded.meta.seed_length,
                origin: loaded.meta.origin,
                delegation_depth: loaded.meta.delegation_depth,
                agent_preset: loaded.meta.agent_preset,
            },
        )?;
        Ok(SessionPreparation::without_release(session))
    }
}

fn ensure_not_aborted(signal: Option<&AbortSignal>) -> anyhow::Result<()> {
    if signal.is_some_and(AbortSignal::is_aborted) {
        anyhow::bail!("session persistence operation aborted")
    }
    Ok(())
}
