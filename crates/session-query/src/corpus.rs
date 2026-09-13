//! Live/persisted logical-corpus resolution for session-query.

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{Session, SessionEvent, SessionHeader, SessionId},
    session_store::{SESSIONS, SessionStore},
};
use seekdeep_llm::AbortSignal;
use seekdeep_session_persistence::{
    SESSION_PERSISTENCE, SessionInspection, SessionPersistence, SessionPersistenceCorruptionError,
    ensure_persistence_not_aborted,
};

use crate::{
    config::{SessionQueryError, SessionQueryErrorCode},
    sources::assert_session_headers_compatible,
    types::SessionRecord,
};

/// Detached source selected for one exact read.
#[derive(Clone, Debug, PartialEq)]
pub struct LogicalSession {
    /// Cloned source header.
    pub header: SessionHeader,
    /// Cloned raw event log.
    pub events: Vec<SessionEvent>,
}

/// Source visible during one synchronous batch projection.
///
/// In Rust the source owns its clones, so the projection may retain nothing
/// borrowed; callers clone any retained output.
pub type LogicalSessionSource = LogicalSession;

/// One source-projection result in a batch logical-corpus observation.
#[derive(Debug)]
pub enum LogicalProjectionResult<Value> {
    /// Successful projection.
    Fulfilled {
        /// Requested session id.
        session_id: SessionId,
        /// Projected value.
        value: Value,
    },
    /// Operational failure isolated to this session.
    Rejected {
        /// Requested session id.
        session_id: SessionId,
        /// Original failure from source resolution or projection.
        reason: Arc<anyhow::Error>,
    },
}

/// Resolves a live-preferred corpus against the persistence service mounted now.
pub struct SessionCorpus {
    context: Context,
    persisted_inspect_concurrency: usize,
}

impl SessionCorpus {
    /// Creates a corpus resolver that reads the live and persisted stores from
    /// `context` at call time.
    #[must_use]
    pub fn new(context: &Context, persisted_inspect_concurrency: u64) -> Arc<Self> {
        Arc::new(Self {
            context: context.clone(),
            persisted_inspect_concurrency: usize::try_from(persisted_inspect_concurrency)
                .unwrap_or(usize::MAX),
        })
    }

    fn persistence(&self) -> Option<Arc<dyn SessionPersistence>> {
        self.context
            .get(SESSION_PERSISTENCE)
            .map(|service| service.persistence())
    }

    /// Lists the complete logical corpus with live precedence and cloned headers.
    ///
    /// # Errors
    ///
    /// Returns cancellation, persistence-listing, or source-conflict failures.
    pub async fn list_sessions(
        &self,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Vec<SessionRecord>> {
        ensure_persistence_not_aborted(signal)?;
        let persistence = self.persistence();
        let persisted = match &persistence {
            None => Vec::new(),
            Some(persistence) => list_persisted(&**persistence, signal).await?,
        };
        ensure_persistence_not_aborted(signal)?;
        let mut records: HashMap<SessionId, SessionRecord> = HashMap::new();
        for header in persisted {
            records.insert(
                header.id.clone(),
                SessionRecord {
                    header: header.clone(),
                    live: false,
                    persisted: true,
                },
            );
        }
        let sessions = self
            .context
            .get(SESSIONS)
            .map(|store| store.list())
            .unwrap_or_default();
        for session in sessions {
            let durable = records.get(session.id()).cloned();
            if let Some(durable) = &durable {
                assert_session_headers_compatible(session.header(), &durable.header)?;
            }
            records.insert(
                session.id().clone(),
                SessionRecord {
                    header: session.header().clone(),
                    live: true,
                    persisted: durable.is_some(),
                },
            );
        }
        let mut result: Vec<SessionRecord> = records.into_values().collect();
        result.sort_by(compare_sessions);
        Ok(result)
    }

    /// Loads one logical source, preferring a detached live snapshot.
    ///
    /// # Errors
    ///
    /// Returns cancellation, persistence, not-found, or source-conflict failures.
    pub async fn load(
        &self,
        session_id: &SessionId,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LogicalSession> {
        ensure_persistence_not_aborted(signal)?;
        let live = self
            .context
            .get(SESSIONS)
            .and_then(|store| store.get(session_id));
        if let Some(live) = live {
            let snapshot = snapshot_live(&live);
            ensure_persistence_not_aborted(signal)?;
            return Ok(snapshot);
        }
        let persistence = self
            .persistence()
            .ok_or_else(|| anyhow::Error::from(not_found(session_id)))?;
        let listed = list_persisted(&*persistence, signal)
            .await?
            .into_iter()
            .find(|header| &header.id == session_id);
        ensure_persistence_not_aborted(signal)?;
        let Some(listed) = listed else {
            return Err(not_found(session_id).into());
        };
        let loaded = inspect_persisted(&*persistence, session_id, signal).await?;
        ensure_persistence_not_aborted(signal)?;
        let attached = self
            .context
            .get(SESSIONS)
            .and_then(|store| store.get(session_id));
        if let Some(attached) = attached {
            let snapshot = snapshot_live(&attached);
            ensure_persistence_not_aborted(signal)?;
            return Ok(snapshot);
        }
        assert_session_headers_compatible(&loaded.meta, &listed)?;
        Ok(LogicalSession {
            header: loaded.meta,
            events: loaded.events,
        })
    }

    /// Projects unique logical sources immediately from one persistence listing.
    ///
    /// # Errors
    ///
    /// Returns cancellation; per-id operational failures are isolated in each
    /// rejected result.
    #[allow(clippy::too_many_lines)]
    pub async fn project_many<Value, F>(
        &self,
        session_ids: &[SessionId],
        project: F,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Vec<LogicalProjectionResult<Value>>>
    where
        Value: Send + 'static,
        F: Fn(&LogicalSessionSource) -> Value + Send + Sync + 'static,
    {
        let project = Arc::new(project);
        let mut ids: Vec<SessionId> = Vec::new();
        for id in session_ids {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        ensure_persistence_not_aborted(signal)?;

        let resolved: Arc<Mutex<HashMap<SessionId, LogicalProjectionResult<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let mut unresolved: Vec<SessionId> = Vec::new();
        let store = self.context.get(SESSIONS);
        for id in &ids {
            match store.as_ref().and_then(|store| store.get(id)) {
                Some(session) => {
                    let source = snapshot_live(&session);
                    let result = project_source(id, &source, &*project, signal)?;
                    resolved.lock().insert(id.clone(), result);
                }
                None => unresolved.push(id.clone()),
            }
        }
        if unresolved.is_empty() {
            return Ok(ordered_results(&ids, take_resolved(&resolved)));
        }

        let Some(persistence) = self.persistence() else {
            for id in &unresolved {
                resolved.lock().insert(
                    id.clone(),
                    LogicalProjectionResult::Rejected {
                        session_id: id.clone(),
                        reason: Arc::new(not_found(id).into()),
                    },
                );
            }
            return Ok(ordered_results(&ids, take_resolved(&resolved)));
        };

        let persisted = match list_persisted(&*persistence, signal).await {
            Ok(persisted) => {
                ensure_persistence_not_aborted(signal)?;
                persisted
            }
            Err(error) => {
                if signal.is_some_and(AbortSignal::is_aborted) {
                    ensure_persistence_not_aborted(signal)?;
                }
                let reason = Arc::new(error);
                for id in &unresolved {
                    resolved.lock().insert(
                        id.clone(),
                        LogicalProjectionResult::Rejected {
                            session_id: id.clone(),
                            reason: reason.clone(),
                        },
                    );
                }
                return Ok(ordered_results(&ids, take_resolved(&resolved)));
            }
        };
        let persisted_by_id: HashMap<SessionId, SessionHeader> = persisted
            .into_iter()
            .map(|header| (header.id.clone(), header))
            .collect();

        let cursor = Arc::new(AtomicUsize::new(0));
        let worker_count = self.persisted_inspect_concurrency.min(unresolved.len());
        let mut handles = Vec::with_capacity(worker_count);
        for _ in 0..worker_count {
            let cursor = cursor.clone();
            let resolved = resolved.clone();
            let persistence = persistence.clone();
            let persisted_by_id = persisted_by_id.clone();
            let store = store.clone();
            let project = project.clone();
            let unresolved = unresolved.clone();
            let signal = signal.cloned();
            handles.push(tokio::spawn(async move {
                loop {
                    ensure_persistence_not_aborted(signal.as_ref())?;
                    let index = cursor.fetch_add(1, Ordering::AcqRel);
                    if index >= unresolved.len() {
                        return Ok(());
                    }
                    let session_id = unresolved[index].clone();
                    let result = resolve_persisted(
                        &session_id,
                        &*persistence,
                        &persisted_by_id,
                        store.as_deref(),
                        &*project,
                        signal.as_ref(),
                    )
                    .await?;
                    resolved.lock().insert(session_id, result);
                }
            }));
        }

        for handle in handles {
            match handle.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    ensure_persistence_not_aborted(signal)?;
                    return Err(error);
                }
                Err(join_error) => return Err(anyhow::Error::from(join_error)),
            }
        }
        ensure_persistence_not_aborted(signal)?;
        Ok(ordered_results(&ids, take_resolved(&resolved)))
    }
}

#[allow(clippy::needless_pass_by_value)]
fn take_resolved<Value>(
    resolved: &Arc<Mutex<HashMap<SessionId, LogicalProjectionResult<Value>>>>,
) -> HashMap<SessionId, LogicalProjectionResult<Value>> {
    std::mem::take(&mut *resolved.lock())
}

fn project_source<Value, F>(
    session_id: &SessionId,
    source: &LogicalSessionSource,
    project: &F,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<LogicalProjectionResult<Value>>
where
    F: Fn(&LogicalSessionSource) -> Value,
{
    ensure_persistence_not_aborted(signal)?;
    let value = project(source);
    ensure_persistence_not_aborted(signal)?;
    Ok(LogicalProjectionResult::Fulfilled {
        session_id: session_id.clone(),
        value,
    })
}

#[allow(clippy::too_many_arguments)]
async fn resolve_persisted<Value, F>(
    session_id: &SessionId,
    persistence: &dyn SessionPersistence,
    persisted_by_id: &HashMap<SessionId, SessionHeader>,
    store: Option<&SessionStore>,
    project: &F,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<LogicalProjectionResult<Value>>
where
    F: Fn(&LogicalSessionSource) -> Value,
{
    let Some(listed) = persisted_by_id.get(session_id) else {
        let attached = store.and_then(|store| store.get(session_id));
        return Ok(match attached {
            Some(attached) => {
                let source = snapshot_live(&attached);
                project_source(session_id, &source, project, signal)?
            }
            None => LogicalProjectionResult::Rejected {
                session_id: session_id.clone(),
                reason: Arc::new(not_found(session_id).into()),
            },
        });
    };

    let outcome: anyhow::Result<LogicalProjectionResult<Value>> = async {
        ensure_persistence_not_aborted(signal)?;
        let loaded = inspect_persisted(persistence, session_id, signal).await?;
        ensure_persistence_not_aborted(signal)?;
        let attached = store.and_then(|store| store.get(session_id));
        if let Some(attached) = attached {
            let source = snapshot_live(&attached);
            return project_source(session_id, &source, project, signal);
        }
        assert_session_headers_compatible(&loaded.meta, listed)?;
        let source = LogicalSession {
            header: loaded.meta,
            events: loaded.events,
        };
        project_source(session_id, &source, project, signal)
    }
    .await;

    match outcome {
        Ok(result) => Ok(result),
        Err(reason) => {
            if signal.is_some_and(AbortSignal::is_aborted) {
                return Err(reason);
            }
            Ok(LogicalProjectionResult::Rejected {
                session_id: session_id.clone(),
                reason: Arc::new(reason),
            })
        }
    }
}

fn snapshot_live(session: &Arc<Session>) -> LogicalSession {
    LogicalSession {
        header: session.header().clone(),
        events: session.events(),
    }
}

fn ordered_results<Value>(
    ids: &[SessionId],
    mut resolved: HashMap<SessionId, LogicalProjectionResult<Value>>,
) -> Vec<LogicalProjectionResult<Value>> {
    ids.iter()
        .map(|id| {
            resolved
                .remove(id)
                .expect("every requested id has a result")
        })
        .collect()
}

async fn list_persisted(
    persistence: &dyn SessionPersistence,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<Vec<SessionHeader>> {
    match persistence.list(signal.cloned()).await {
        Ok(headers) => Ok(headers),
        Err(error) => {
            if signal.is_some_and(AbortSignal::is_aborted) {
                ensure_persistence_not_aborted(signal)?;
            }
            Err(SessionQueryError::new(
                format!(
                    "session persistence listing failed: {}",
                    error_message(&error)
                ),
                SessionQueryErrorCode::SessionQueryPersistenceFailed,
            )
            .into())
        }
    }
}

async fn inspect_persisted(
    persistence: &dyn SessionPersistence,
    session_id: &SessionId,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<SessionInspection> {
    match persistence.inspect(session_id, signal.cloned()).await {
        Ok(inspection) => Ok(inspection),
        Err(error) => {
            if signal.is_some_and(AbortSignal::is_aborted) {
                ensure_persistence_not_aborted(signal)?;
            }
            if error
                .downcast_ref::<SessionPersistenceCorruptionError>()
                .is_some()
            {
                return Err(SessionQueryError::new(
                    format!(
                        "stored session \"{session_id}\" is corrupt: {}",
                        error_message(&error)
                    ),
                    SessionQueryErrorCode::SessionQueryCorruptSession,
                )
                .into());
            }
            Err(SessionQueryError::new(
                format!(
                    "failed to inspect session \"{session_id}\": {}",
                    error_message(&error)
                ),
                SessionQueryErrorCode::SessionQueryPersistenceFailed,
            )
            .into())
        }
    }
}

fn compare_sessions(left: &SessionRecord, right: &SessionRecord) -> std::cmp::Ordering {
    right
        .header
        .created_at
        .cmp(&left.header.created_at)
        .then_with(|| left.header.id.as_str().cmp(right.header.id.as_str()))
}

fn not_found(session_id: &SessionId) -> SessionQueryError {
    SessionQueryError::new(
        format!("session \"{session_id}\" not found"),
        SessionQueryErrorCode::SessionQuerySessionNotFound,
    )
}

fn error_message(error: &anyhow::Error) -> String {
    error.to_string()
}
