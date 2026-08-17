//! Exact Workspace API adapter over the durable registry.

use std::{collections::HashSet, sync::Arc};

use futures::{FutureExt as _, StreamExt as _, future::BoxFuture, stream::BoxStream};
use seekdeep_core::session::SessionId;
use seekdeep_llm::AbortSignal;
use seekdeep_storage_domain::{DomainChanged, DomainFacility};
use seekdeep_workspace::{
    Workspace, WorkspaceDomainState, WorkspaceId as RegistryWorkspaceId, WorkspaceMoveInvalidError,
    WorkspaceOrderInvalidError, WorkspaceRecord, WorkspaceRegistry, WorkspaceUnknownSessionError,
};
use tokio::sync::{mpsc, oneshot};

use crate::{
    api::{
        events::HostFrame,
        workspace::{WorkspaceId, WorkspaceView},
    },
    service::{WorkspaceRuntime, WorkspaceRuntimeError, WorkspaceSnapshot},
};

type Operation = BoxFuture<'static, ()>;

struct HostWorkspaceProjection {
    committed_ids: HashSet<WorkspaceId>,
    committed_order: Vec<WorkspaceId>,
    archived_ids: Vec<SessionId>,
}

impl HostWorkspaceProjection {
    fn new(snapshot: WorkspaceSnapshot) -> Self {
        let committed_order = snapshot
            .items
            .iter()
            .map(|workspace| workspace.workspace_id.clone())
            .collect::<Vec<_>>();
        Self {
            committed_ids: committed_order.iter().cloned().collect(),
            committed_order,
            archived_ids: snapshot.archived_session_ids,
        }
    }

    fn apply(
        &mut self,
        change: DomainChanged,
        registry: &WorkspaceRegistry,
    ) -> anyhow::Result<Vec<HostFrame>> {
        if change.domain() != "workspace" {
            return Ok(Vec::new());
        }
        if change.table().is_empty() {
            let DomainChanged::Put { value, .. } = change else {
                return Ok(Vec::new());
            };
            return self.apply_state(serde_json::from_value(value)?, registry);
        }
        if change.table() != "workspaces" {
            return Ok(Vec::new());
        }
        match change {
            DomainChanged::Deleted { key, .. } => {
                let id = WorkspaceId::new(key);
                Ok(self
                    .committed_ids
                    .remove(&id)
                    .then(|| HostFrame::WorkspaceRemoved { workspace_id: id })
                    .into_iter()
                    .collect())
            }
            DomainChanged::Put { key, value, .. } => {
                let id = WorkspaceId::new(&key);
                if self.committed_ids.contains(&id) {
                    Ok(vec![HostFrame::WorkspaceChanged {
                        workspace: WorkspaceRegistryRuntime::changed_view(&key, value)?,
                    }])
                } else {
                    Ok(Vec::new())
                }
            }
        }
    }

    fn apply_state(
        &mut self,
        state: WorkspaceDomainState,
        registry: &WorkspaceRegistry,
    ) -> anyhow::Result<Vec<HostFrame>> {
        let state_order = state
            .workspace_ids
            .iter()
            .map(|id| WorkspaceId::new(id.as_str()))
            .collect::<Vec<_>>();
        let order_changed = state_order.len() == self.committed_order.len()
            && state_order.iter().all(|id| self.committed_ids.contains(id))
            && state_order
                .iter()
                .zip(&self.committed_order)
                .any(|(left, right)| left != right);
        let mut frames = Vec::new();
        for id in &state_order {
            if self.committed_ids.contains(id) {
                continue;
            }
            let registry_id = RegistryWorkspaceId::new(id.as_str());
            let workspace = registry.get(&registry_id).ok_or_else(|| {
                anyhow::anyhow!(
                    "committed workspace registry references missing workspace \"{id}\""
                )
            })?;
            self.committed_ids.insert(id.clone());
            frames.push(HostFrame::WorkspaceChanged {
                workspace: WorkspaceRegistryRuntime::view(&workspace),
            });
        }
        self.committed_order.clone_from(&state_order);
        if order_changed {
            frames.push(HostFrame::WorkspaceOrderChanged {
                workspace_ids: state_order,
            });
        }
        if state.archived_session_ids != self.archived_ids {
            self.archived_ids = state.archived_session_ids;
            frames.push(HostFrame::ArchivedSessionsChanged {
                archived_session_ids: self.archived_ids.clone(),
            });
        }
        Ok(frames)
    }
}

/// Host API business behavior backed by the durable Workspace registry.
pub struct WorkspaceRegistryRuntime {
    registry: Arc<WorkspaceRegistry>,
    domains: Arc<DomainFacility>,
    operations: mpsc::UnboundedSender<Operation>,
}

impl std::fmt::Debug for WorkspaceRegistryRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceRegistryRuntime")
            .field("registry", &self.registry)
            .finish_non_exhaustive()
    }
}

impl WorkspaceRegistryRuntime {
    /// Creates the adapter and its eager source-compatible create/rename/delete chain.
    #[must_use]
    pub fn new(registry: Arc<WorkspaceRegistry>, domains: Arc<DomainFacility>) -> Arc<Self> {
        let (operations, mut receiver) = mpsc::unbounded_channel::<Operation>();
        tokio::spawn(async move {
            while let Some(operation) = receiver.recv().await {
                operation.await;
            }
        });
        Arc::new(Self {
            registry,
            domains,
            operations,
        })
    }

    fn enqueue<T>(
        &self,
        operation: impl std::future::Future<Output = T> + Send + 'static,
    ) -> BoxFuture<'static, T>
    where
        T: Send + 'static,
    {
        let (send, receive) = oneshot::channel();
        let job = async move {
            let _ = send.send(operation.await);
        }
        .boxed();
        assert!(
            self.operations.send(job).is_ok(),
            "Workspace API operation queue stopped while its runtime is live"
        );
        async move {
            receive
                .await
                .expect("Workspace API operation queue stopped before replying")
        }
        .boxed()
    }

    fn view(workspace: &Workspace) -> WorkspaceView {
        WorkspaceView {
            workspace_id: WorkspaceId::new(workspace.id().as_str()),
            path: workspace.path(),
            title: workspace.title(),
            session_ids: workspace.session_ids(),
            created_at: workspace.created_at(),
            updated_at: workspace.updated_at(),
        }
    }

    fn changed_view(id: &str, value: serde_json::Value) -> anyhow::Result<WorkspaceView> {
        let record: WorkspaceRecord = serde_json::from_value(value)?;
        Ok(WorkspaceView {
            workspace_id: WorkspaceId::new(id),
            path: record.path,
            title: record.title,
            session_ids: record.session_ids,
            created_at: record.created_at,
            updated_at: record.updated_at,
        })
    }
}

impl WorkspaceRuntime for WorkspaceRegistryRuntime {
    fn list(&self) -> anyhow::Result<WorkspaceSnapshot> {
        Ok(WorkspaceSnapshot {
            items: self
                .registry
                .list()?
                .iter()
                .map(|workspace| Self::view(workspace))
                .collect(),
            archived_session_ids: self.registry.archived_session_ids(),
        })
    }

    fn create(&self, path: String) -> BoxFuture<'static, anyhow::Result<(WorkspaceView, bool)>> {
        let registry = self.registry.clone();
        self.enqueue(async move {
            if let Some(workspace) = registry.resolve_by_path(&path).await? {
                return Ok((Self::view(&workspace), false));
            }
            let workspace = registry.create(path, None).await?;
            Ok((Self::view(&workspace), true))
        })
    }

    fn rename(
        &self,
        workspace_id: WorkspaceId,
        title: String,
    ) -> BoxFuture<'static, Result<WorkspaceView, WorkspaceRuntimeError>> {
        let registry_id = RegistryWorkspaceId::new(workspace_id.as_str());
        let Some(workspace) = self.registry.get(&registry_id) else {
            return async move { Err(WorkspaceRuntimeError::NotFound(workspace_id)) }.boxed();
        };
        let registry = self.registry.clone();
        self.enqueue(async move {
            if title == workspace.title() {
                return Ok(Self::view(&workspace));
            }
            if registry
                .list()?
                .iter()
                .any(|other| other.id() != workspace.id() && other.title() == title)
            {
                return Err(WorkspaceRuntimeError::NameConflict(title));
            }
            workspace.set_title(title).await?;
            Ok(Self::view(&workspace))
        })
    }

    fn delete(
        &self,
        workspace_id: WorkspaceId,
    ) -> BoxFuture<'static, Result<(), WorkspaceRuntimeError>> {
        let registry = self.registry.clone();
        let registry_id = RegistryWorkspaceId::new(workspace_id.as_str());
        self.enqueue(async move {
            if registry.delete(registry_id).await? {
                Ok(())
            } else {
                Err(WorkspaceRuntimeError::NotFound(workspace_id))
            }
        })
    }

    fn insert_before(
        &self,
        workspace_id: WorkspaceId,
        before_workspace_id: Option<WorkspaceId>,
    ) -> BoxFuture<'static, Result<Vec<WorkspaceId>, WorkspaceRuntimeError>> {
        let registry = self.registry.clone();
        let id = RegistryWorkspaceId::new(workspace_id.as_str());
        let before = before_workspace_id
            .as_ref()
            .map(|id| RegistryWorkspaceId::new(id.as_str()));
        async move {
            match registry.insert_before(id, before).await {
                Ok(ids) => Ok(ids
                    .into_iter()
                    .map(|id| WorkspaceId::new(id.as_str()))
                    .collect()),
                Err(error) => match error.downcast::<WorkspaceOrderInvalidError>() {
                    Ok(error) => Err(WorkspaceRuntimeError::NotFound(WorkspaceId::new(
                        error.workspace_id.as_str(),
                    ))),
                    Err(error) => Err(WorkspaceRuntimeError::Internal(error)),
                },
            }
        }
        .boxed()
    }

    fn insert_session_before(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
        before_session_id: Option<SessionId>,
    ) -> BoxFuture<'static, Result<WorkspaceView, WorkspaceRuntimeError>> {
        let registry_id = RegistryWorkspaceId::new(workspace_id.as_str());
        let Some(workspace) = self.registry.get(&registry_id) else {
            return async move { Err(WorkspaceRuntimeError::NotFound(workspace_id)) }.boxed();
        };
        async move {
            match workspace
                .insert_session_before(session_id, before_session_id)
                .await
            {
                Ok(()) => Ok(Self::view(&workspace)),
                Err(error) => match error.downcast::<WorkspaceMoveInvalidError>() {
                    Ok(error) => Err(WorkspaceRuntimeError::MoveInvalid(error.message)),
                    Err(error) => Err(WorkspaceRuntimeError::Internal(error)),
                },
            }
        }
        .boxed()
    }

    fn archive_session(
        &self,
        session_id: SessionId,
    ) -> BoxFuture<'static, Result<Vec<SessionId>, WorkspaceRuntimeError>> {
        let registry = self.registry.clone();
        async move {
            match registry.archive_session(session_id).await {
                Ok(()) => Ok(registry.archived_session_ids()),
                Err(error) => match error.downcast::<WorkspaceUnknownSessionError>() {
                    Ok(error) => {
                        let message = error.to_string();
                        Err(WorkspaceRuntimeError::UnknownSession {
                            session_id: error.session_id,
                            message,
                        })
                    }
                    Err(error) => Err(WorkspaceRuntimeError::Internal(error)),
                },
            }
        }
        .boxed()
    }

    fn host_events(&self, signal: AbortSignal) -> BoxStream<'static, anyhow::Result<HostFrame>> {
        let mut changes = self.domains.subscribe_sequenced();
        let baseline = self.registry.host_projection_baseline(&self.domains).map(
            |(workspaces, archived_session_ids, sequence)| {
                (
                    WorkspaceSnapshot {
                        items: workspaces
                            .iter()
                            .map(|workspace| Self::view(workspace))
                            .collect(),
                        archived_session_ids,
                    },
                    sequence,
                )
            },
        );
        let registry = self.registry.clone();
        async_stream::try_stream! {
            let (baseline, baseline_sequence) = baseline?;
            let mut projection = HostWorkspaceProjection::new(baseline);
            loop {
                let change = tokio::select! {
                    () = signal.cancelled() => None,
                    change = changes.next() => change,
                };
                let Some(change) = change else { return };
                let (sequence, change) = change?;
                if sequence <= baseline_sequence {
                    continue;
                }
                for frame in projection.apply(change, &registry)? {
                    yield frame;
                }
            }
        }
        .boxed()
    }
}
