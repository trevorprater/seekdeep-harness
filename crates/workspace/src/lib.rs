//! Durable workspace registry with canonical paths and validated session ownership.

mod entity;
mod invariant;
mod paths;
mod spec;

pub use entity::{Workspace, WorkspaceMoveInvalidError, WorkspaceStatus};
pub use invariant::{INVARIANT_NAME, register_invariant};
pub use paths::realpath_normalize;
pub use spec::{
    PendingMutation, PendingOperation, WorkspaceDomainState, WorkspaceRecord, workspace_domain_spec,
};

use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::Path,
    sync::{Arc, Weak},
};

use chrono::{SecondsFormat, TimeZone as _, Utc};
use futures::{FutureExt as _, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_cordis::{
    Context, Plugin, ServiceKey,
    fiber::{DisposeFuture, EffectHandle},
};
use seekdeep_core::{
    session::{SessionHeader, SessionId},
    session_store::{SESSIONS, SessionStore},
};
use seekdeep_session_persistence::{
    SESSION_PERSISTENCE, SessionPersistence, SessionPersistenceService,
};
use seekdeep_storage_domain::{Domain, DomainFacility, KvTable, STORAGE_DOMAIN};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// Typed Cordis seat corresponding to `ctx.workspaceRegistry`.
pub const WORKSPACE_REGISTRY: ServiceKey<WorkspaceRegistry> = ServiceKey::new("workspaceRegistry");

/// Cordis plugin name retained by loader-facing diagnostics.
pub const NAME: &str = "workspaceRegistry";
/// Startup cannot run until both durable dependencies are active.
pub const INJECT: &[&str] = &["storageDomain", "sessionPersistence"];

/// Builds the dependency-driven Workspace service plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, _config| {
        Box::pin(async move {
            let domains = context.get(STORAGE_DOMAIN).ok_or_else(|| {
                anyhow::anyhow!("workspaceRegistry lost required storageDomain service")
            })?;
            let persistence: Arc<SessionPersistenceService> =
                context.get(SESSION_PERSISTENCE).ok_or_else(|| {
                    anyhow::anyhow!("workspaceRegistry lost required sessionPersistence service")
                })?;
            let registry = WorkspaceRegistry::open(
                context.clone(),
                &domains,
                persistence.persistence(),
                context.get(SESSIONS),
            )
            .await?;
            let cleanup_registry = registry.clone();
            let cleanup = EffectHandle::new("workspace.domainClose", move || -> DisposeFuture {
                Box::pin(async move { cleanup_registry.close().await })
            });
            if let Err(error) = context.own(cleanup) {
                if let Err(close) = registry.close().await {
                    return Err(anyhow::anyhow!(
                        "{error}: workspace close failed: {close:#}"
                    ));
                }
                return Err(error.into());
            }
            registry.provide(&context)?;
            Ok(())
        })
    })
}

seekdeep_util::string_brand!(
    /// Stable workspace record identity. Paths are deliberately not identities.
    pub struct WorkspaceId;
);

/// A reorder named a source or anchor absent from durable registry order.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("cannot reorder unknown workspace '{workspace_id}'")]
pub struct WorkspaceOrderInvalidError {
    /// Missing source or anchor.
    pub workspace_id: WorkspaceId,
}

/// An archive request named no live or persisted session.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error(
    "cannot archive session '{session_id}': live sessions and session persistence hold no such session"
)]
pub struct WorkspaceUnknownSessionError {
    /// Definitely unknown session.
    pub session_id: SessionId,
}

/// Two failures from a primary registry write and its required rollback.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct WorkspaceAggregateError {
    /// Source-compatible aggregate diagnostic.
    pub message: String,
    /// Primary failure followed by rollback failure.
    pub errors: Vec<anyhow::Error>,
}

type Operation = BoxFuture<'static, ()>;

/// Durable Workspace registry.
pub struct WorkspaceRegistry {
    domain: Arc<Domain>,
    table: Arc<KvTable>,
    host: Arc<WorkspaceHost>,
    state: Mutex<WorkspaceDomainState>,
    entities: Mutex<HashMap<WorkspaceId, Arc<Workspace>>>,
    operations: mpsc::UnboundedSender<Operation>,
    self_weak: Weak<Self>,
}

impl fmt::Debug for WorkspaceRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkspaceRegistry")
            .field("state", &self.state.lock())
            .field("entities", &self.entities.lock().keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl WorkspaceRegistry {
    /// Opens, recovers, bootstraps, validates, and publishes a registry.
    ///
    /// # Errors
    ///
    /// Returns persistence, domain, path-indexing, or durable-consistency failures.
    pub async fn open(
        context: Context,
        domains: &Arc<DomainFacility>,
        persistence: Arc<dyn SessionPersistence>,
        sessions: Option<Arc<SessionStore>>,
    ) -> anyhow::Result<Arc<Self>> {
        let domain = domains.open(workspace_domain_spec()?).await?;
        let result = Self::finish_open(context, domain.clone(), persistence, sessions).await;
        if result.is_err() {
            let _ = domain.close().await;
        }
        result
    }

    async fn finish_open(
        context: Context,
        domain: Arc<Domain>,
        persistence: Arc<dyn SessionPersistence>,
        sessions: Option<Arc<SessionStore>>,
    ) -> anyhow::Result<Arc<Self>> {
        let table = domain.table("workspaces")?;
        let state = parse_state(domain.global_get()?)?;
        let (operations, mut receiver) = mpsc::unbounded_channel::<Operation>();
        let host = Arc::new(WorkspaceHost {
            context,
            table: table.clone(),
            persistence,
            sessions,
            headers: Mutex::new(HashMap::new()),
            session_paths: Mutex::new(HashMap::new()),
            invalid_session_paths: Mutex::new(HashMap::new()),
        });
        let registry = Arc::new_cyclic(|weak| Self {
            domain,
            table,
            host,
            state: Mutex::new(state),
            entities: Mutex::new(HashMap::new()),
            operations,
            self_weak: weak.clone(),
        });
        tokio::spawn(async move {
            while let Some(operation) = receiver.recv().await {
                operation.await;
            }
        });

        registry.recover_pending_mutation().await?;
        registry.validate_stored_state()?;
        if !registry.state.lock().initialized {
            let headers = registry.host.persistence.list(None).await?;
            registry.host.replace_header_index(&headers).await;
            registry.bootstrap(&headers).await?;
        } else if !registry.table.is_empty()? {
            let headers = registry.host.persistence.list(None).await?;
            registry.host.replace_header_index(&headers).await;
        }
        registry.host.index_live_sessions().await;
        registry.validate_stored_state()?;
        registry.rebuild_entities()?;
        registry.report_filtered_candidates()?;
        Ok(registry)
    }

    /// Provides the initialized registry until the returned effect is disposed.
    ///
    /// # Errors
    ///
    /// Returns a duplicate-service or inactive-context failure.
    pub fn provide(self: &Arc<Self>, context: &Context) -> anyhow::Result<EffectHandle> {
        Ok(context.provide(WORKSPACE_REGISTRY, self.clone())?)
    }

    /// Closes the owned workspace domain after draining accepted writes.
    pub fn close(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        self.domain.close()
    }

    /// Creates or reuses the workspace for one existing canonical directory.
    #[must_use]
    pub fn create(
        self: &Arc<Self>,
        path: String,
        title: Option<String>,
    ) -> BoxFuture<'static, anyhow::Result<Arc<Workspace>>> {
        let registry = self.clone();
        eager(async move {
            let canonical = realpath_normalize(&path).await?;
            if !tokio::fs::metadata(&canonical).await?.is_dir() {
                anyhow::bail!(
                    "cannot create a workspace at '{}': path is not a directory",
                    canonical.display()
                );
            }
            registry
                .enqueue(move |registry| {
                    async move { registry.create_canonical(canonical, title).await }.boxed()
                })
                .await
        })
    }

    /// Looks up one cached entity by stable id.
    #[must_use]
    pub fn get(&self, id: &WorkspaceId) -> Option<Arc<Workspace>> {
        self.entities.lock().get(id).cloned()
    }

    /// Returns a fresh entity list in durable order.
    ///
    /// # Errors
    ///
    /// Fails loud if the cache and durable order were externally diverged.
    pub fn list(&self) -> anyhow::Result<Vec<Arc<Workspace>>> {
        let commit_lock = self.domain.commit_lock();
        let _commit = commit_lock.lock();
        self.list_unlocked()
    }

    fn list_unlocked(&self) -> anyhow::Result<Vec<Arc<Workspace>>> {
        self.state
            .lock()
            .workspace_ids
            .iter()
            .map(|id| {
                self.entities.lock().get(id).cloned().ok_or_else(|| {
                    anyhow::anyhow!("workspace registry order references missing workspace '{id}'")
                })
            })
            .collect()
    }

    /// Deletes only one workspace registration, retaining directories and sessions.
    #[must_use]
    pub fn delete(self: &Arc<Self>, id: WorkspaceId) -> BoxFuture<'static, anyhow::Result<bool>> {
        self.enqueue(move |registry| async move { registry.delete_known(&id).await }.boxed())
    }

    /// Moves one workspace before an anchor, or to the end without an anchor.
    #[must_use]
    pub fn insert_before(
        self: &Arc<Self>,
        id: WorkspaceId,
        before_id: Option<WorkspaceId>,
    ) -> BoxFuture<'static, anyhow::Result<Vec<WorkspaceId>>> {
        self.enqueue(move |registry| {
            async move {
                let state = registry.state.lock().clone();
                if !state.workspace_ids.contains(&id) {
                    return Err(WorkspaceOrderInvalidError { workspace_id: id }.into());
                }
                if let Some(before) = &before_id
                    && !state.workspace_ids.contains(before)
                {
                    return Err(WorkspaceOrderInvalidError {
                        workspace_id: before.clone(),
                    }
                    .into());
                }
                if before_id.as_ref() == Some(&id) {
                    return Ok(state.workspace_ids);
                }
                let mut workspace_ids = state
                    .workspace_ids
                    .iter()
                    .filter(|workspace_id| **workspace_id != id)
                    .cloned()
                    .collect::<Vec<_>>();
                let at = before_id
                    .as_ref()
                    .and_then(|before| workspace_ids.iter().position(|item| item == before))
                    .unwrap_or(workspace_ids.len());
                workspace_ids.insert(at, id);
                if workspace_ids == state.workspace_ids {
                    return Ok(state.workspace_ids);
                }
                registry
                    .set_state(WorkspaceDomainState {
                        workspace_ids: workspace_ids.clone(),
                        ..state
                    })
                    .await?;
                Ok(workspace_ids)
            }
            .boxed()
        })
    }

    /// Current registry-global archive order.
    #[must_use]
    pub fn archived_session_ids(&self) -> Vec<SessionId> {
        let commit_lock = self.domain.commit_lock();
        let _commit = commit_lock.lock();
        self.state.lock().archived_session_ids.clone()
    }

    /// Captures a coherent Host baseline and domain-event watermark.
    ///
    /// # Errors
    ///
    /// Fails loud if durable order and entity cache are inconsistent.
    pub fn host_projection_baseline(
        &self,
        domains: &DomainFacility,
    ) -> anyhow::Result<(Vec<Arc<Workspace>>, Vec<SessionId>, u64)> {
        let commit_lock = self.domain.commit_lock();
        let _commit = commit_lock.lock();
        Ok((
            self.list_unlocked()?,
            self.state.lock().archived_session_ids.clone(),
            domains.change_sequence(),
        ))
    }

    /// Archives one known live or persisted session without changing accounting.
    #[must_use]
    pub fn archive_session(
        self: &Arc<Self>,
        session_id: SessionId,
    ) -> BoxFuture<'static, anyhow::Result<()>> {
        self.enqueue(move |registry| {
            async move {
                if registry
                    .state
                    .lock()
                    .archived_session_ids
                    .contains(&session_id)
                {
                    return Ok(());
                }
                if !registry.host.session_known(&session_id).await? {
                    return Err(WorkspaceUnknownSessionError { session_id }.into());
                }
                let mut state = registry.state.lock().clone();
                state.archived_session_ids.push(session_id);
                registry.set_state(state).await
            }
            .boxed()
        })
    }

    /// Resolves an owned existing directory without mutating the registry.
    ///
    /// # Errors
    ///
    /// Preserves canonicalization failures for missing or inaccessible paths.
    pub async fn resolve_by_path(&self, path: &str) -> anyhow::Result<Option<Arc<Workspace>>> {
        let canonical = realpath_normalize(path).await?;
        Ok(self
            .entities
            .lock()
            .values()
            .find(|entity| entity.path() == canonical)
            .cloned())
    }

    fn enqueue<T, F>(self: &Arc<Self>, operation: F) -> BoxFuture<'static, anyhow::Result<T>>
    where
        T: Send + 'static,
        F: FnOnce(Arc<Self>) -> BoxFuture<'static, anyhow::Result<T>> + Send + 'static,
    {
        let Some(registry) = self.self_weak.upgrade() else {
            return async { anyhow::bail!("workspace registry was dropped") }.boxed();
        };
        let (send, receive) = oneshot::channel();
        let job = async move {
            let result = async {
                registry.recover_pending_mutation().await?;
                operation(registry.clone()).await
            }
            .await;
            let _ = send.send(result);
        }
        .boxed();
        if self.operations.send(job).is_err() {
            return async { anyhow::bail!("workspace registry operation queue stopped") }.boxed();
        }
        async move {
            receive
                .await
                .map_err(|_| anyhow::anyhow!("workspace registry operation queue stopped"))?
        }
        .boxed()
    }

    async fn set_state(&self, state: WorkspaceDomainState) -> anyhow::Result<()> {
        let registry = self.self_weak.clone();
        let committed_state = state.clone();
        self.domain
            .global_set_with_commit(serde_json::to_value(&state)?, move |_| {
                if let Some(registry) = registry.upgrade() {
                    *registry.state.lock() = committed_state;
                }
            })
            .await
    }

    async fn create_canonical(
        &self,
        canonical: std::path::PathBuf,
        title: Option<String>,
    ) -> anyhow::Result<Arc<Workspace>> {
        let canonical = path_string(&canonical)?;
        if let Some(entity) = self
            .entities
            .lock()
            .values()
            .find(|entity| entity.path() == canonical)
            .cloned()
        {
            return Ok(entity);
        }
        let state = self.state.lock().clone();
        let id = WorkspaceId::new(Uuid::new_v4().to_string());
        let now = now_iso();
        let record = WorkspaceRecord {
            path: canonical.clone(),
            title: title.unwrap_or_else(|| basename(&canonical)),
            session_ids: Vec::new(),
            created_at: now.clone(),
            updated_at: now,
        };
        let entity = Workspace::new(self.host.clone(), id.clone(), record.clone());
        self.entities.lock().insert(id.clone(), entity.clone());
        let pending = WorkspaceDomainState {
            pending_mutation: Some(PendingMutation {
                operation: PendingOperation::Create,
                workspace_id: id.clone(),
            }),
            ..state.clone()
        };
        if let Err(error) = self.set_state(pending).await {
            self.entities.lock().remove(&id);
            return Err(error);
        }
        if let Err(error) = self
            .table
            .put(id.to_string(), serde_json::to_value(&record)?)
            .await
        {
            self.entities.lock().remove(&id);
            if let Err(rollback) = self.set_state(state.clone()).await {
                return Err(aggregate(
                    error,
                    rollback,
                    format!(
                        "workspace '{id}' record write and pending-marker rollback both failed"
                    ),
                ));
            }
            return Err(error);
        }
        let mut committed = WorkspaceDomainState {
            initialized: true,
            workspace_ids: vec![id.clone()],
            archived_session_ids: state.archived_session_ids.clone(),
            pending_mutation: None,
        };
        committed.workspace_ids.extend(state.workspace_ids.clone());
        if let Err(error) = self.set_state(committed).await {
            self.entities.lock().remove(&id);
            if let Err(rollback) = self.table.delete(id.to_string()).await {
                return Err(aggregate(
                    error,
                    rollback,
                    format!(
                        "workspace '{id}' order write and record rollback both failed; the pending marker remains recoverable"
                    ),
                ));
            }
            if let Err(rollback) = self.set_state(state).await {
                return Err(aggregate(
                    error,
                    rollback,
                    format!("workspace '{id}' order write and pending-marker rollback both failed"),
                ));
            }
            return Err(error);
        }
        Ok(entity)
    }

    async fn delete_known(&self, id: &WorkspaceId) -> anyhow::Result<bool> {
        let Some(entity) = self.entities.lock().get(id).cloned() else {
            return Ok(false);
        };
        let state = self.state.lock().clone();
        let next = WorkspaceDomainState {
            initialized: true,
            workspace_ids: state
                .workspace_ids
                .iter()
                .filter(|item| *item != id)
                .cloned()
                .collect(),
            archived_session_ids: state.archived_session_ids.clone(),
            pending_mutation: None,
        };
        self.set_state(WorkspaceDomainState {
            pending_mutation: Some(PendingMutation {
                operation: PendingOperation::Delete,
                workspace_id: id.clone(),
            }),
            ..next.clone()
        })
        .await?;
        self.entities.lock().remove(id);
        if let Err(error) = self.table.delete(id.to_string()).await {
            self.entities.lock().insert(id.clone(), entity);
            if let Err(rollback) = self.set_state(state).await {
                self.entities.lock().remove(id);
                return Err(aggregate(
                    error,
                    rollback,
                    format!(
                        "workspace '{id}' record deletion and registry-order rollback both failed"
                    ),
                ));
            }
            return Err(error);
        }
        if let Err(error) = self.set_state(next).await {
            tracing::warn!(workspace = %id, %error, "workspace was deleted but its pending marker could not be cleared");
        }
        Ok(true)
    }

    async fn recover_pending_mutation(&self) -> anyhow::Result<()> {
        let state = self.state.lock().clone();
        let Some(pending) = state.pending_mutation.clone() else {
            return Ok(());
        };
        if state.workspace_ids.contains(&pending.workspace_id) {
            anyhow::bail!(
                "workspace domain is inconsistent: pending {} workspace '{}' is still present in registry order",
                pending.operation,
                pending.workspace_id
            );
        }
        self.table.delete(pending.workspace_id.to_string()).await?;
        self.set_state(WorkspaceDomainState {
            pending_mutation: None,
            ..state
        })
        .await
    }

    fn validate_stored_state(&self) -> anyhow::Result<()> {
        let state = self.state.lock().clone();
        let entries = workspace_entries(&self.table)?;
        let mut order = HashSet::new();
        for id in &state.workspace_ids {
            if !order.insert(id.clone()) {
                anyhow::bail!(
                    "workspace domain is inconsistent: registry order repeats workspace '{id}'"
                );
            }
            if self.table.get(id.as_str())?.is_none() {
                anyhow::bail!(
                    "workspace domain is inconsistent: registry order references missing workspace '{id}'"
                );
            }
        }
        if state.initialized && order.len() != entries.len() {
            let orphan = entries
                .iter()
                .map(|(id, _)| id)
                .find(|id| !order.contains(*id))
                .expect("table/order size mismatch has an orphan");
            anyhow::bail!(
                "workspace domain is inconsistent: workspace '{orphan}' is absent from registry order"
            );
        }
        let mut paths = HashMap::<String, WorkspaceId>::new();
        let mut accounted = HashMap::<SessionId, WorkspaceId>::new();
        for (id, record) in entries {
            if let Some(holder) = paths.insert(record.path.clone(), id.clone()) {
                anyhow::bail!(
                    "workspace domain is inconsistent: path '{}' is claimed by both workspace '{}' and workspace '{}'",
                    record.path,
                    holder,
                    id
                );
            }
            for session_id in record.session_ids {
                if let Some(holder) = accounted.insert(session_id.clone(), id.clone()) {
                    anyhow::bail!(
                        "workspace domain is inconsistent: session '{session_id}' is accounted by both workspace '{holder}' and workspace '{id}'"
                    );
                }
            }
        }
        Ok(())
    }

    fn rebuild_entities(&self) -> anyhow::Result<()> {
        let mut entities = self.entities.lock();
        entities.clear();
        for id in &self.state.lock().workspace_ids {
            let raw = self
                .table
                .get(id.as_str())?
                .ok_or_else(|| anyhow::anyhow!("workspace '{id}' disappeared while rebuilding"))?;
            let record = parse_record(raw)?;
            entities.insert(
                id.clone(),
                Workspace::new(self.host.clone(), id.clone(), record),
            );
        }
        Ok(())
    }

    fn report_filtered_candidates(&self) -> anyhow::Result<()> {
        for entity in self.entities.lock().values() {
            let record = parse_record(
                self.table
                    .get(entity.id().as_str())?
                    .expect("cached entity has durable record"),
            )?;
            for session_id in record.session_ids {
                let path = self.host.session_path(&session_id);
                if path.as_deref() == Some(record.path.as_str()) {
                    continue;
                }
                let reason = self
                    .host
                    .invalid_session_paths
                    .lock()
                    .get(&session_id)
                    .cloned()
                    .unwrap_or_else(|| {
                        if self.host.headers.lock().contains_key(&session_id) {
                            format!(
                                "canonical cwd '{}' differs from workspace path '{}'",
                                path.unwrap_or_default(),
                                record.path
                            )
                        } else {
                            "session header is missing".to_owned()
                        }
                    });
                tracing::warn!(workspace = %entity.id(), session = %session_id, %reason, "filtered session from workspace membership");
            }
        }
        Ok(())
    }

    async fn bootstrap(&self, headers: &[SessionHeader]) -> anyhow::Result<()> {
        let state = self.state.lock().clone();
        let groups = self.bootstrap_groups(headers);
        self.merge_bootstrap_groups(&groups).await?;
        let workspace_ids = self.bootstrap_order(&groups, &state)?;
        if workspace_ids != state.workspace_ids {
            self.set_state(WorkspaceDomainState {
                initialized: false,
                workspace_ids: workspace_ids.clone(),
                archived_session_ids: state.archived_session_ids.clone(),
                pending_mutation: None,
            })
            .await?;
        }
        self.set_state(WorkspaceDomainState {
            initialized: true,
            workspace_ids,
            archived_session_ids: state.archived_session_ids,
            pending_mutation: None,
        })
        .await
    }

    fn bootstrap_groups(&self, headers: &[SessionHeader]) -> Vec<BootstrapGroup> {
        let mut groups = HashMap::<String, Vec<SessionHeader>>::new();
        for header in headers {
            if let Some(path) = self.host.session_path(&header.id) {
                groups.entry(path).or_default().push(header.clone());
            }
        }
        let mut groups = groups
            .into_iter()
            .map(|(path, mut headers)| {
                headers.sort_by(compare_headers);
                let newest_at = headers[0].created_at;
                BootstrapGroup {
                    path,
                    headers,
                    newest_at,
                }
            })
            .collect::<Vec<_>>();
        groups.sort_by(|left, right| {
            right
                .newest_at
                .cmp(&left.newest_at)
                .then_with(|| left.path.cmp(&right.path))
        });
        groups
    }

    async fn merge_bootstrap_groups(&self, groups: &[BootstrapGroup]) -> anyhow::Result<()> {
        let entries = workspace_entries(&self.table)?;
        let mut by_path = entries
            .iter()
            .map(|(id, record)| (record.path.clone(), id.clone()))
            .collect::<HashMap<_, _>>();
        let mut accounted = HashMap::<SessionId, WorkspaceId>::new();
        for (id, record) in &entries {
            for session_id in &record.session_ids {
                accounted.insert(session_id.clone(), id.clone());
            }
        }
        for group in groups {
            if let Some(id) = by_path.get(&group.path).cloned() {
                let current = parse_record(
                    self.table
                        .get(id.as_str())?
                        .expect("bootstrap path index references table record"),
                )?;
                let historical = group
                    .headers
                    .iter()
                    .map(|header| header.id.clone())
                    .filter(|session_id| {
                        accounted.get(session_id).is_none_or(|holder| holder == &id)
                    })
                    .collect::<Vec<_>>();
                let historical_set = historical.iter().cloned().collect::<HashSet<_>>();
                let mut session_ids = historical.clone();
                session_ids.extend(
                    current
                        .session_ids
                        .iter()
                        .filter(|id| !historical_set.contains(*id))
                        .cloned(),
                );
                if session_ids != current.session_ids {
                    self.table
                        .update(id.to_string(), move |raw| {
                            let mut record = parse_record(raw.clone())?;
                            record.session_ids = session_ids;
                            record.updated_at = now_iso();
                            Ok(serde_json::to_value(record)?)
                        })
                        .await?;
                }
                for session_id in historical {
                    accounted.insert(session_id, id.clone());
                }
            } else {
                let session_ids = group
                    .headers
                    .iter()
                    .map(|header| header.id.clone())
                    .filter(|session_id| !accounted.contains_key(session_id))
                    .collect::<Vec<_>>();
                if session_ids.is_empty() {
                    continue;
                }
                let id = WorkspaceId::new(Uuid::new_v4().to_string());
                let created_at = millis_iso(group.newest_at)?;
                let record = WorkspaceRecord {
                    path: group.path.clone(),
                    title: basename(&group.path),
                    session_ids: session_ids.clone(),
                    created_at: created_at.clone(),
                    updated_at: created_at,
                };
                self.table
                    .put(id.to_string(), serde_json::to_value(record)?)
                    .await?;
                by_path.insert(group.path.clone(), id.clone());
                for session_id in session_ids {
                    accounted.insert(session_id, id.clone());
                }
            }
        }
        Ok(())
    }

    fn bootstrap_order(
        &self,
        groups: &[BootstrapGroup],
        state: &WorkspaceDomainState,
    ) -> anyhow::Result<Vec<WorkspaceId>> {
        let group_rank = groups
            .iter()
            .map(|group| (group.path.clone(), group.newest_at))
            .collect::<HashMap<_, _>>();
        let prior_rank = state
            .workspace_ids
            .iter()
            .enumerate()
            .map(|(index, id)| (id.clone(), index))
            .collect::<HashMap<_, _>>();
        let mut entries = workspace_entries(&self.table)?;
        entries.sort_by(|(left_id, left), (right_id, right)| {
            let left_time = group_rank
                .get(&left.path)
                .copied()
                .unwrap_or_else(|| parse_iso_millis(&left.created_at));
            let right_time = group_rank
                .get(&right.path)
                .copied()
                .unwrap_or_else(|| parse_iso_millis(&right.created_at));
            right_time
                .cmp(&left_time)
                .then_with(|| {
                    prior_rank
                        .get(left_id)
                        .copied()
                        .unwrap_or(usize::MAX)
                        .cmp(&prior_rank.get(right_id).copied().unwrap_or(usize::MAX))
                })
                .then_with(|| left_id.as_str().cmp(right_id.as_str()))
        });
        Ok(entries.into_iter().map(|(id, _)| id).collect())
    }
}

struct WorkspaceHost {
    context: Context,
    table: Arc<KvTable>,
    persistence: Arc<dyn SessionPersistence>,
    sessions: Option<Arc<SessionStore>>,
    headers: Mutex<HashMap<SessionId, SessionHeader>>,
    session_paths: Mutex<HashMap<SessionId, String>>,
    invalid_session_paths: Mutex<HashMap<SessionId, String>>,
}

impl fmt::Debug for WorkspaceHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkspaceHost")
            .field("table", &self.table)
            .field("sessions", &self.current_sessions().is_some())
            .field("headers", &self.headers.lock().len())
            .finish_non_exhaustive()
    }
}

impl WorkspaceHost {
    fn current_sessions(&self) -> Option<Arc<SessionStore>> {
        self.context.get(SESSIONS).or_else(|| self.sessions.clone())
    }

    fn session_path(&self, id: &SessionId) -> Option<String> {
        self.session_paths.lock().get(id).cloned()
    }

    async fn replace_header_index(&self, headers: &[SessionHeader]) {
        self.headers.lock().clear();
        self.session_paths.lock().clear();
        self.invalid_session_paths.lock().clear();
        self.index_headers(headers).await;
    }

    async fn index_headers(&self, headers: &[SessionHeader]) {
        for header in headers {
            self.index_header(header).await;
        }
    }

    async fn index_header(&self, header: &SessionHeader) {
        self.headers
            .lock()
            .insert(header.id.clone(), header.clone());
        self.session_paths.lock().remove(&header.id);
        let Some(cwd) = &header.cwd else {
            self.invalid_session_paths
                .lock()
                .insert(header.id.clone(), "header has no cwd".to_owned());
            return;
        };
        match realpath_normalize(cwd).await {
            Ok(path) => match tokio::fs::metadata(&path).await {
                Ok(metadata) if metadata.is_dir() => match path_string(&path) {
                    Ok(path) => {
                        self.session_paths.lock().insert(header.id.clone(), path);
                        self.invalid_session_paths.lock().remove(&header.id);
                    }
                    Err(_) => {
                        self.invalid_session_paths
                            .lock()
                            .insert(header.id.clone(), format!("cwd '{cwd}' does not resolve"));
                    }
                },
                Ok(_) => {
                    self.invalid_session_paths
                        .lock()
                        .insert(header.id.clone(), format!("cwd '{cwd}' is not a directory"));
                }
                Err(_) => {
                    self.invalid_session_paths
                        .lock()
                        .insert(header.id.clone(), format!("cwd '{cwd}' does not resolve"));
                }
            },
            Err(_) => {
                self.invalid_session_paths
                    .lock()
                    .insert(header.id.clone(), format!("cwd '{cwd}' does not resolve"));
            }
        }
    }

    async fn index_live_sessions(&self) {
        let Some(sessions) = self.current_sessions() else {
            return;
        };
        let headers = sessions
            .list()
            .into_iter()
            .map(|session| session.header().clone())
            .collect::<Vec<_>>();
        self.index_headers(&headers).await;
    }

    async fn read_session_header(&self, id: &SessionId) -> anyhow::Result<SessionHeader> {
        if let Some(session) = self
            .current_sessions()
            .as_ref()
            .and_then(|sessions| sessions.get(id))
        {
            let header = session.header().clone();
            self.headers.lock().insert(id.clone(), header.clone());
            return Ok(header);
        }
        if let Some(header) = self.headers.lock().get(id).cloned() {
            return Ok(header);
        }
        let headers = self.persistence.list(None).await?;
        self.index_headers(&headers).await;
        self.headers.lock().get(id).cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "cannot validate session '{id}': session persistence holds no such session"
            )
        })
    }

    async fn session_known(&self, id: &SessionId) -> anyhow::Result<bool> {
        if self
            .current_sessions()
            .as_ref()
            .and_then(|sessions| sessions.get(id))
            .is_some()
            || self.headers.lock().contains_key(id)
        {
            return Ok(true);
        }
        let headers = self.persistence.list(None).await?;
        self.index_headers(&headers).await;
        Ok(self.headers.lock().contains_key(id))
    }
}

struct BootstrapGroup {
    path: String,
    headers: Vec<SessionHeader>,
    newest_at: u64,
}

fn compare_headers(left: &SessionHeader, right: &SessionHeader) -> std::cmp::Ordering {
    right
        .created_at
        .cmp(&left.created_at)
        .then_with(|| left.id.as_str().cmp(right.id.as_str()))
}

fn workspace_entries(table: &KvTable) -> anyhow::Result<Vec<(WorkspaceId, WorkspaceRecord)>> {
    table
        .entries()?
        .into_iter()
        .map(|(id, value)| Ok((WorkspaceId::new(id), parse_record(value)?)))
        .collect()
}

fn parse_record(value: serde_json::Value) -> anyhow::Result<WorkspaceRecord> {
    Ok(serde_json::from_value(value)?)
}

fn parse_state(value: serde_json::Value) -> anyhow::Result<WorkspaceDomainState> {
    Ok(serde_json::from_value(value)?)
}

fn eager<T>(
    future: impl std::future::Future<Output = anyhow::Result<T>> + Send + 'static,
) -> BoxFuture<'static, anyhow::Result<T>>
where
    T: Send + 'static,
{
    let (send, receive) = oneshot::channel();
    tokio::spawn(async move {
        let _ = send.send(future.await);
    });
    async move { receive.await.map_err(anyhow::Error::new)? }.boxed()
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn millis_iso(millis: u64) -> anyhow::Result<String> {
    let millis = i64::try_from(millis)?;
    Ok(Utc
        .timestamp_millis_opt(millis)
        .single()
        .ok_or_else(|| anyhow::anyhow!("session createdAt {millis} is outside the ISO date range"))?
        .to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn parse_iso_millis(value: &str) -> u64 {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .and_then(|date| u64::try_from(date.timestamp_millis()).ok())
        .unwrap_or(0)
}

fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn path_string(path: &Path) -> anyhow::Result<String> {
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("canonical workspace path is not valid UTF-8"))
}

fn aggregate(first: anyhow::Error, second: anyhow::Error, message: String) -> anyhow::Error {
    WorkspaceAggregateError {
        message,
        errors: vec![first, second],
    }
    .into()
}
