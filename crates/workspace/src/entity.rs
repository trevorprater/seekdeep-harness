//! Registry-owned Workspace entity implementation.

use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use futures::{FutureExt as _, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_core::session::SessionId;
use thiserror::Error;

use crate::spec::WorkspaceRecord;
use crate::{WorkspaceHost, WorkspaceId, now_iso, parse_record, path_string, realpath_normalize};

/// A manual session move named a session or anchor absent from the account.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct WorkspaceMoveInvalidError {
    /// Exact source diagnostic.
    pub message: String,
}

/// Live directory availability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceStatus {
    /// Directory exists and is a directory.
    Ok,
    /// Directory is currently unusable.
    MissingDir,
}

impl WorkspaceStatus {
    /// Exact source wire value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::MissingDir => "missing-dir",
        }
    }
}

enum Mutation {
    Unchanged,
    Changed(WorkspaceRecord),
}

#[derive(Debug, Error)]
#[error("workspace record unchanged (internal sentinel)")]
struct Unchanged;

/// Stable workspace entity backed by one domain record.
pub struct Workspace {
    host: Arc<WorkspaceHost>,
    id: WorkspaceId,
    record: Mutex<WorkspaceRecord>,
    next_mutation: AtomicU64,
    record_mutation: Mutex<u64>,
}

impl fmt::Debug for Workspace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Workspace")
            .field("id", &self.id)
            .field("record", &self.record.lock())
            .finish_non_exhaustive()
    }
}

impl Workspace {
    pub(crate) fn new(
        host: Arc<WorkspaceHost>,
        id: WorkspaceId,
        record: WorkspaceRecord,
    ) -> Arc<Self> {
        Arc::new(Self {
            host,
            id,
            record: Mutex::new(record),
            next_mutation: AtomicU64::new(0),
            record_mutation: Mutex::new(0),
        })
    }

    /// Stable generated id.
    #[must_use]
    pub fn id(&self) -> &WorkspaceId {
        &self.id
    }

    /// Immutable canonical directory.
    #[must_use]
    pub fn path(&self) -> String {
        self.record.lock().path.clone()
    }

    /// Current display title.
    #[must_use]
    pub fn title(&self) -> String {
        self.record.lock().title.clone()
    }

    /// Immutable ISO creation instant.
    #[must_use]
    pub fn created_at(&self) -> String {
        self.record.lock().created_at.clone()
    }

    /// ISO instant of the last committed mutation.
    #[must_use]
    pub fn updated_at(&self) -> String {
        self.record.lock().updated_at.clone()
    }

    /// Header-validated ordered session membership projection.
    #[must_use]
    pub fn session_ids(&self) -> Vec<SessionId> {
        let record = self.record.lock();
        record
            .session_ids
            .iter()
            .filter(|id| self.host.session_path(id).as_deref() == Some(record.path.as_str()))
            .cloned()
            .collect()
    }

    /// Replaces the title durably, including same-value writes like the source spread operation.
    #[must_use]
    pub fn set_title(self: &Arc<Self>, title: String) -> BoxFuture<'static, anyhow::Result<()>> {
        self.mutate(move |mut record| {
            record.title = title;
            Ok(Mutation::Changed(record))
        })
    }

    /// Prepends a session after immutable-header cwd validation.
    #[must_use]
    pub fn attach_session(
        self: &Arc<Self>,
        session_id: SessionId,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        let entity = self.clone();
        super::eager(async move {
            let snapshot = entity.record.lock().clone();
            if !snapshot.session_ids.contains(&session_id) {
                let header = entity.host.read_session_header(&session_id).await?;
                let cwd = header.cwd.ok_or_else(|| {
                    anyhow::anyhow!(
                        "cannot attach session '{}' to workspace '{}': its stored header carries no cwd to validate against",
                        session_id,
                        snapshot.path
                    )
                })?;
                let canonical = realpath_normalize(&cwd).await.map_err(|error| {
                    anyhow::anyhow!(error).context(format!(
                        "cannot attach session '{}' to workspace '{}': its cwd '{}' does not resolve, so it cannot be validated",
                        session_id, snapshot.path, cwd
                    ))
                })?;
                if !tokio::fs::metadata(&canonical).await?.is_dir() {
                    anyhow::bail!(
                        "cannot attach session '{}' to workspace '{}': its cwd '{}' is not a directory",
                        session_id,
                        snapshot.path,
                        cwd
                    );
                }
                let canonical = path_string(&canonical)?;
                if canonical != snapshot.path {
                    anyhow::bail!(
                        "cannot attach session '{}' to workspace '{}': its cwd resolves to '{}'",
                        session_id,
                        snapshot.path,
                        canonical
                    );
                }
                entity
                    .host
                    .session_paths
                    .lock()
                    .insert(session_id.clone(), canonical);
                entity.host.invalid_session_paths.lock().remove(&session_id);
            }
            entity
                .mutate(move |mut record| {
                    if record.session_ids.contains(&session_id) {
                        Ok(Mutation::Unchanged)
                    } else {
                        record.session_ids.insert(0, session_id);
                        Ok(Mutation::Changed(record))
                    }
                })
                .await
        })
    }

    /// Moves an accounted session before an anchor or to the end.
    #[must_use]
    pub fn insert_session_before(
        self: &Arc<Self>,
        session_id: SessionId,
        before_session_id: Option<SessionId>,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        self.mutate(move |mut record| {
            if !record.session_ids.contains(&session_id) {
                return Err(WorkspaceMoveInvalidError {
                    message: format!(
                        "cannot move session '{}' in workspace '{}': the session is not accounted",
                        session_id, record.path
                    ),
                }
                .into());
            }
            if let Some(before) = &before_session_id
                && !record.session_ids.contains(before)
            {
                return Err(WorkspaceMoveInvalidError {
                    message: format!(
                        "cannot move session '{}' before '{}' in workspace '{}': the anchor session is not accounted",
                        session_id, before, record.path
                    ),
                }
                .into());
            }
            if before_session_id.as_ref() == Some(&session_id) {
                return Ok(Mutation::Unchanged);
            }
            let prior = record.session_ids.clone();
            record.session_ids.retain(|id| id != &session_id);
            let at = before_session_id
                .as_ref()
                .and_then(|before| record.session_ids.iter().position(|id| id == before))
                .unwrap_or(record.session_ids.len());
            record.session_ids.insert(at, session_id);
            if record.session_ids == prior {
                Ok(Mutation::Unchanged)
            } else {
                Ok(Mutation::Changed(record))
            }
        })
    }

    /// Idempotently detaches a session candidate without touching its log.
    #[must_use]
    pub fn detach_session(
        self: &Arc<Self>,
        session_id: SessionId,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        self.mutate(move |mut record| {
            let previous = record.session_ids.len();
            record.session_ids.retain(|id| id != &session_id);
            if record.session_ids.len() == previous {
                Ok(Mutation::Unchanged)
            } else {
                Ok(Mutation::Changed(record))
            }
        })
    }

    /// Checks the live directory without mutating the durable record.
    pub async fn status(&self) -> WorkspaceStatus {
        match tokio::fs::metadata(self.path()).await {
            Ok(metadata) if metadata.is_dir() => WorkspaceStatus::Ok,
            Ok(_) | Err(_) => WorkspaceStatus::MissingDir,
        }
    }

    fn mutate<F>(self: &Arc<Self>, mutation: F) -> BoxFuture<'static, anyhow::Result<()>>
    where
        F: FnOnce(WorkspaceRecord) -> anyhow::Result<Mutation> + Send + 'static,
    {
        let entity = self.clone();
        let mutation_number = self.next_mutation.fetch_add(1, Ordering::AcqRel) + 1;
        let id = self.id.to_string();
        let host = self.host.clone();
        let table = host.table.clone();
        let committed_entity = entity.clone();
        let future = table.update_with_commit(
            id,
            move |raw| {
                let current = parse_record(raw.clone())?;
                let changed = mutation(current.clone())?;
                let unchanged = matches!(changed, Mutation::Unchanged);
                let mut next = match changed {
                    Mutation::Unchanged => current.clone(),
                    Mutation::Changed(record) => record,
                };
                next.session_ids
                    .retain(|id| host.session_path(id).as_deref() == Some(next.path.as_str()));
                if unchanged && next.session_ids.len() == current.session_ids.len() {
                    return Err(Unchanged.into());
                }
                next.updated_at = now_iso();
                Ok(serde_json::to_value(next)?)
            },
            move |raw| {
                let next: WorkspaceRecord = serde_json::from_value(raw.clone())
                    .expect("Workspace mutation produced its declared record schema");
                let mut landed = committed_entity.record_mutation.lock();
                if mutation_number >= *landed {
                    *committed_entity.record.lock() = next;
                    *landed = mutation_number;
                }
            },
        );
        async move {
            match future.await {
                Ok(_) => Ok(()),
                Err(error) if error.downcast_ref::<Unchanged>().is_some() => Ok(()),
                Err(error) => Err(error),
            }
        }
        .boxed()
    }
}
