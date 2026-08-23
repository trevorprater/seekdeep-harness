//! Workspace baseline, incremental-frame, and unary-action owner.

use std::{cell::RefCell, rc::Rc};

use futures::{FutureExt, future::LocalBoxFuture};
use indexmap::{IndexMap, IndexSet};
use seekdeep_identity::{SessionId, WorkspaceId};
use serde_json::{Value, json};

use crate::{
    ClientRpcError, ClientRpcResult, ClientWorkspace, ClientWorkspaceView, Notifier,
    NotifierScheduler, RuntimeDisposer, SessionTaskSpawner, SessionTransport,
    SessionTransportRequest, WorkspaceCreateInput, internal_error, workspace_view,
};

/// Monotone workspace-list arrival lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceListPhase {
    /// No successful baseline has arrived.
    Pending,
    /// At least one baseline succeeded.
    Ready,
}

/// Current workspace-list request state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceListState {
    /// No request is active and the last request did not fail.
    Idle,
    /// One shared baseline request is active.
    Loading,
    /// The last baseline request failed.
    Error,
}

/// Immutable workspace-list snapshot.
pub struct WorkspaceListSnapshot {
    /// Host rows in durable display order.
    pub items: Rc<Vec<Rc<ClientWorkspaceView>>>,
    /// Registry-global archive set in Host order.
    pub archived_session_ids: Rc<Vec<SessionId>>,
    /// Pull activity axis.
    pub state: WorkspaceListState,
    /// First-success lifecycle.
    pub phase: WorkspaceListPhase,
    /// Last business or transport failure.
    pub error: Option<ClientRpcError>,
}

/// Workspace manager construction seams.
pub struct WorkspaceManagerOptions {
    /// Snapshot notification scheduler.
    pub scheduler: Rc<dyn NotifierScheduler>,
    /// Detached reconnect refresh owner.
    pub spawner: Rc<dyn SessionTaskSpawner>,
    /// Target-compatible `Date.parse` implementation.
    pub parse_date: Rc<dyn Fn(&str) -> f64>,
}

#[derive(Clone)]
enum WorkspaceDelta {
    Upsert(Rc<ClientWorkspaceView>),
    Remove(WorkspaceId),
    Order(Rc<Vec<WorkspaceId>>),
}

/// Workspace-related Host frame.
#[derive(Clone)]
pub enum WorkspaceHostFrame {
    /// Full changed Workspace view.
    Changed(Rc<ClientWorkspaceView>),
    /// Removed durable registration.
    Removed(WorkspaceId),
    /// Full durable Workspace order.
    OrderChanged(Rc<Vec<WorkspaceId>>),
    /// Full registry-global archive set.
    ArchivedSessionsChanged(Rc<Vec<SessionId>>),
}

/// Shared Workspace baseline refresh operation.
pub type WorkspaceRefreshTask = futures::future::Shared<LocalBoxFuture<'static, ()>>;

struct ManagerState {
    items: Rc<Vec<Rc<ClientWorkspace>>>,
    item_views_source: Option<Rc<Vec<Rc<ClientWorkspace>>>>,
    item_views_cache: Rc<Vec<Rc<ClientWorkspaceView>>>,
    archived_session_ids: Rc<Vec<SessionId>>,
    list_state: WorkspaceListState,
    phase: WorkspaceListPhase,
    error: Option<ClientRpcError>,
    list_inflight: Option<(u64, WorkspaceRefreshTask)>,
    next_list_token: u64,
    refresh_frames: Option<Rc<RefCell<Vec<WorkspaceDelta>>>>,
    archived_supersedes_refresh: bool,
    order_request_generation: u64,
    order_frame_generation: u64,
    committed_order: Vec<WorkspaceId>,
    removed_ids: IndexSet<WorkspaceId>,
    snapshot: Rc<WorkspaceListSnapshot>,
}

/// Workspace object cluster driven by one list baseline and changed-frame upserts.
pub struct WorkspaceManager {
    transport: Rc<dyn SessionTransport>,
    options: WorkspaceManagerOptions,
    state: RefCell<ManagerState>,
    notifier: Rc<Notifier>,
}

impl WorkspaceManager {
    /// Creates an empty manager.
    #[must_use]
    pub fn new(transport: Rc<dyn SessionTransport>, options: WorkspaceManagerOptions) -> Rc<Self> {
        Rc::new_cyclic(|weak: &std::rc::Weak<Self>| {
            let initial = Rc::new(WorkspaceListSnapshot {
                items: Rc::new(Vec::new()),
                archived_session_ids: Rc::new(Vec::new()),
                state: WorkspaceListState::Idle,
                phase: WorkspaceListPhase::Pending,
                error: None,
            });
            let manager = weak.clone();
            let notifier = Notifier::new(
                Rc::new(move || {
                    if let Some(manager) = manager.upgrade() {
                        manager.rebuild_snapshot();
                    }
                }),
                options.scheduler.clone(),
            );
            Self {
                transport,
                state: RefCell::new(ManagerState {
                    items: Rc::new(Vec::new()),
                    item_views_source: None,
                    item_views_cache: Rc::new(Vec::new()),
                    archived_session_ids: Rc::new(Vec::new()),
                    list_state: WorkspaceListState::Idle,
                    phase: WorkspaceListPhase::Pending,
                    error: None,
                    list_inflight: None,
                    next_list_token: 0,
                    refresh_frames: None,
                    archived_supersedes_refresh: false,
                    order_request_generation: 0,
                    order_frame_generation: 0,
                    committed_order: Vec::new(),
                    removed_ids: IndexSet::new(),
                    snapshot: initial,
                }),
                options,
                notifier,
            }
        })
    }

    /// Refreshes `workspace.list`, sharing one in-flight request.
    pub fn refresh(self: &Rc<Self>) -> WorkspaceRefreshTask {
        if let Some((_, task)) = &self.state.borrow().list_inflight {
            return task.clone();
        }
        let (token, frames) = {
            let mut state = self.state.borrow_mut();
            state.list_state = WorkspaceListState::Loading;
            state.error = None;
            state.next_list_token = state.next_list_token.wrapping_add(1);
            let frames = Rc::new(RefCell::new(Vec::new()));
            state.refresh_frames = Some(frames.clone());
            (state.next_list_token, frames)
        };
        self.notifier.mark_dirty();
        let weak = Rc::downgrade(self);
        let task = async move {
            if let Some(manager) = weak.upgrade() {
                manager.run_refresh(token, frames).await;
            }
        }
        .boxed_local()
        .shared();
        self.state.borrow_mut().list_inflight = Some((token, task.clone()));
        task
    }

    /// Creates or resolves a real Workspace and installs its unary echo.
    pub async fn create(self: &Rc<Self>, input: WorkspaceCreateInput) -> ClientRpcResult<Value> {
        let workspace = ClientWorkspace::local(
            self.transport.clone(),
            self.options.scheduler.clone(),
            input,
        );
        let Some(completion) = workspace.materialize() else {
            return ClientRpcResult::Failure(internal_error(
                "a local Workspace must be materializable",
            ));
        };
        let result = completion.await;
        if let ClientRpcResult::Success(Some(value)) = &result
            && let Some(view) = workspace_view(value.get("workspace"))
        {
            self.upsert(Rc::new(view), Some(workspace));
        }
        result
    }

    /// Renames one Workspace and installs its returned view.
    ///
    /// # Errors
    ///
    /// Returns generated-client transport rejection.
    pub async fn rename(
        &self,
        workspace_id: &WorkspaceId,
        title: &str,
    ) -> Result<ClientRpcResult<Value>, String> {
        let result = self
            .call(
                "workspace.rename",
                json!({"workspaceId":workspace_id.as_str(),"title":title}),
            )
            .await?;
        if let ClientRpcResult::Success(Some(value)) = &result
            && let Some(view) = workspace_view(value.get("workspace"))
        {
            self.upsert(Rc::new(view), None);
        }
        Ok(result)
    }

    /// Deletes one Workspace and installs the removal echo synchronously.
    ///
    /// # Errors
    ///
    /// Returns generated-client transport rejection.
    pub async fn delete(
        &self,
        workspace_id: &WorkspaceId,
    ) -> Result<ClientRpcResult<Value>, String> {
        let result = self
            .call(
                "workspace.delete",
                json!({"workspaceId":workspace_id.as_str()}),
            )
            .await?;
        if matches!(result, ClientRpcResult::Success(_)) {
            self.remove(workspace_id, true);
        }
        Ok(result)
    }

    /// Optimistically reorders one Workspace and arbitrates Host frames over unary echoes.
    pub fn insert_before(
        self: &Rc<Self>,
        workspace_id: &WorkspaceId,
        before_workspace_id: Option<&WorkspaceId>,
    ) -> LocalBoxFuture<'static, Result<ClientRpcResult<Value>, String>> {
        let (request_generation, frame_generation, optimistic) = {
            let mut state = self.state.borrow_mut();
            state.order_request_generation = state.order_request_generation.wrapping_add(1);
            let local = item_views(&mut state)
                .iter()
                .map(|workspace| workspace.workspace_id.clone())
                .collect::<Vec<_>>();
            (
                state.order_request_generation,
                state.order_frame_generation,
                insert_id_before(&local, workspace_id, before_workspace_id),
            )
        };
        self.install_order(&optimistic, false);
        let mut payload = serde_json::Map::from_iter([(
            "workspaceId".to_owned(),
            Value::String(workspace_id.as_str().to_owned()),
        )]);
        if let Some(before) = before_workspace_id {
            payload.insert(
                "beforeWorkspaceId".to_owned(),
                Value::String(before.as_str().to_owned()),
            );
        }
        let manager = self.clone();
        async move {
            let result = manager
                .call("workspace.insertBefore", Value::Object(payload))
                .await;
            let current = {
                let state = manager.state.borrow();
                request_generation == state.order_request_generation
                    && frame_generation == state.order_frame_generation
            };
            match result {
                Ok(result) => {
                    if current {
                        if let ClientRpcResult::Success(Some(value)) = &result
                            && let Some(order) = workspace_ids(value.get("workspaceIds"))
                        {
                            manager.install_order(&order, true);
                        } else if matches!(result, ClientRpcResult::Failure(_)) {
                            let committed = manager.state.borrow().committed_order.clone();
                            manager.install_order(&committed, false);
                        }
                    }
                    Ok(result)
                }
                Err(error) => {
                    if current {
                        let committed = manager.state.borrow().committed_order.clone();
                        manager.install_order(&committed, false);
                    }
                    Err(error)
                }
            }
        }
        .boxed_local()
    }

    /// Reorders one accounted Session inside a Workspace.
    ///
    /// # Errors
    ///
    /// Returns generated-client transport rejection.
    pub async fn insert_session_before(
        &self,
        workspace_id: &WorkspaceId,
        session_id: &SessionId,
        before_session_id: Option<&SessionId>,
    ) -> Result<ClientRpcResult<Value>, String> {
        let mut payload = serde_json::Map::from_iter([
            (
                "workspaceId".to_owned(),
                Value::String(workspace_id.as_str().to_owned()),
            ),
            (
                "sessionId".to_owned(),
                Value::String(session_id.as_str().to_owned()),
            ),
        ]);
        if let Some(before) = before_session_id {
            payload.insert(
                "beforeSessionId".to_owned(),
                Value::String(before.as_str().to_owned()),
            );
        }
        let result = self
            .call("workspace.insertSessionBefore", Value::Object(payload))
            .await?;
        if let ClientRpcResult::Success(Some(value)) = &result
            && let Some(view) = workspace_view(value.get("workspace"))
        {
            self.upsert(Rc::new(view), None);
        }
        Ok(result)
    }

    /// Archives one Session and installs the returned full archive set.
    ///
    /// # Errors
    ///
    /// Returns generated-client transport rejection.
    pub async fn archive_session(
        &self,
        session_id: &SessionId,
    ) -> Result<ClientRpcResult<Value>, String> {
        let result = self
            .call(
                "workspace.archiveSession",
                json!({"sessionId":session_id.as_str()}),
            )
            .await?;
        if let ClientRpcResult::Success(Some(value)) = &result
            && let Some(ids) = session_ids(value.get("archivedSessionIds"))
        {
            self.install_archived(Rc::new(ids));
        }
        Ok(result)
    }

    /// Routes one Workspace-related Host frame.
    pub fn handle_host_frame(&self, frame: WorkspaceHostFrame) {
        match frame {
            WorkspaceHostFrame::Changed(workspace) => self.upsert(workspace, None),
            WorkspaceHostFrame::Removed(workspace_id) => self.remove(&workspace_id, false),
            WorkspaceHostFrame::OrderChanged(workspace_ids) => {
                let mut state = self.state.borrow_mut();
                state.order_frame_generation = state.order_frame_generation.wrapping_add(1);
                drop(state);
                self.install_order(&workspace_ids, true);
            }
            WorkspaceHostFrame::ArchivedSessionsChanged(session_ids) => {
                self.install_archived(session_ids);
            }
        }
    }

    /// Starts a detached refresh after a connection-generation change.
    pub fn handle_connected(self: &Rc<Self>) {
        let refresh = self.refresh();
        self.options.spawner.spawn(refresh.boxed_local());
    }

    /// Subscribes to committed workspace snapshot changes.
    #[must_use]
    pub fn subscribe(&self, listener: Rc<dyn Fn()>) -> RuntimeDisposer {
        self.notifier.subscribe(listener)
    }

    /// Returns the cached workspace snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Rc<WorkspaceListSnapshot> {
        self.notifier.ensure_fresh();
        self.state.borrow().snapshot.clone()
    }

    async fn run_refresh(self: &Rc<Self>, token: u64, frames: Rc<RefCell<Vec<WorkspaceDelta>>>) {
        let result = match self
            .transport
            .call(SessionTransportRequest {
                method: "workspace.list".to_owned(),
                payload: json!({}),
            })
            .await
        {
            Ok(result) => result,
            Err(error) => ClientRpcResult::Failure(internal_error(error)),
        };
        match result {
            ClientRpcResult::Success(Some(value)) => {
                let parsed = workspace_list(&value);
                if let Some((mut views, archived)) = parsed {
                    let removed = self.state.borrow().removed_ids.clone();
                    views.retain(|view| !removed.contains(&view.workspace_id));
                    for delta in frames.borrow().iter() {
                        views = apply_workspace_delta(views, delta);
                    }
                    self.install_views(&views);
                    if !self.state.borrow().archived_supersedes_refresh {
                        self.install_archived(Rc::new(archived));
                    }
                    let mut state = self.state.borrow_mut();
                    state.list_state = WorkspaceListState::Idle;
                    state.phase = WorkspaceListPhase::Ready;
                } else {
                    let mut state = self.state.borrow_mut();
                    state.list_state = WorkspaceListState::Error;
                    state.error = Some(internal_error("workspace.list response was malformed"));
                }
            }
            ClientRpcResult::Success(None) => {
                let mut state = self.state.borrow_mut();
                state.list_state = WorkspaceListState::Error;
                state.error = Some(internal_error("workspace.list response omitted value"));
            }
            ClientRpcResult::Failure(error) => {
                let mut state = self.state.borrow_mut();
                state.list_state = WorkspaceListState::Error;
                state.error = Some(error);
            }
        }
        {
            let mut state = self.state.borrow_mut();
            if state
                .list_inflight
                .as_ref()
                .is_some_and(|(current, _)| *current == token)
            {
                state.refresh_frames = None;
                state.archived_supersedes_refresh = false;
                state.list_inflight = None;
            }
        }
        self.notifier.mark_dirty();
    }

    fn install_archived(&self, archived_session_ids: Rc<Vec<SessionId>>) {
        let changed = {
            let mut state = self.state.borrow_mut();
            if state.refresh_frames.is_some() {
                state.archived_supersedes_refresh = true;
            }
            if state.archived_session_ids.as_ref() == archived_session_ids.as_ref() {
                false
            } else {
                state.archived_session_ids = archived_session_ids;
                true
            }
        };
        if changed {
            self.notifier.mark_dirty();
        }
    }

    fn install_order(&self, workspace_ids: &[WorkspaceId], committed: bool) {
        let changed = {
            let mut state = self.state.borrow_mut();
            if committed {
                if let Some(frames) = &state.refresh_frames {
                    frames
                        .borrow_mut()
                        .push(WorkspaceDelta::Order(Rc::new(workspace_ids.to_vec())));
                }
                state.committed_order = workspace_ids.to_vec();
            }
            let rank = workspace_ids
                .iter()
                .enumerate()
                .map(|(index, id)| (id.clone(), index))
                .collect::<IndexMap<_, _>>();
            let mut items = state.items.as_ref().clone();
            items.sort_by_key(|workspace| {
                workspace
                    .snapshot()
                    .view
                    .as_ref()
                    .and_then(|view| rank.get(&view.workspace_id).copied())
                    .unwrap_or(usize::MAX)
            });
            if items
                .iter()
                .zip(state.items.iter())
                .all(|(left, right)| Rc::ptr_eq(left, right))
            {
                false
            } else {
                state.items = Rc::new(items);
                state.item_views_source = None;
                true
            }
        };
        if changed {
            self.notifier.mark_dirty();
        }
    }

    fn upsert(&self, view: Rc<ClientWorkspaceView>, identity: Option<Rc<ClientWorkspace>>) {
        let changed = {
            let mut state = self.state.borrow_mut();
            if state.removed_ids.contains(&view.workspace_id) {
                return;
            }
            if let Some(frames) = &state.refresh_frames {
                frames
                    .borrow_mut()
                    .push(WorkspaceDelta::Upsert(view.clone()));
            }
            let index = state.items.iter().position(|workspace| {
                workspace
                    .snapshot()
                    .view
                    .as_ref()
                    .is_some_and(|known| known.workspace_id == view.workspace_id)
            });
            let installed = index.and_then(|index| state.items[index].snapshot().view.clone());
            if installed.as_ref().is_some_and(|known| {
                (self.options.parse_date)(&view.updated_at)
                    < (self.options.parse_date)(&known.updated_at)
            }) {
                return;
            }
            if !state.committed_order.contains(&view.workspace_id) {
                state.committed_order.insert(0, view.workspace_id.clone());
            }
            let mut items = state.items.as_ref().clone();
            match (index, identity) {
                (None, Some(identity)) => items.insert(0, identity),
                (Some(index), Some(identity)) => items[index] = identity,
                (None, None) => items.insert(
                    0,
                    ClientWorkspace::materialized(
                        self.transport.clone(),
                        self.options.scheduler.clone(),
                        view,
                    ),
                ),
                (Some(index), None) => {
                    let _ = items[index].adopt(view);
                }
            }
            state.items = Rc::new(items);
            state.item_views_source = None;
            true
        };
        if changed {
            self.notifier.mark_dirty();
        }
    }

    fn remove(&self, workspace_id: &WorkspaceId, direct: bool) {
        let removed = {
            let mut state = self.state.borrow_mut();
            if let Some(frames) = &state.refresh_frames {
                frames
                    .borrow_mut()
                    .push(WorkspaceDelta::Remove(workspace_id.clone()));
            }
            state.removed_ids.insert(workspace_id.clone());
            state.committed_order.retain(|id| id != workspace_id);
            let items = state
                .items
                .iter()
                .filter(|workspace| {
                    workspace
                        .snapshot()
                        .view
                        .as_ref()
                        .is_none_or(|view| &view.workspace_id != workspace_id)
                })
                .cloned()
                .collect::<Vec<_>>();
            if items.len() == state.items.len() {
                false
            } else {
                state.items = Rc::new(items);
                state.item_views_source = None;
                true
            }
        };
        if direct {
            self.notifier.notify_now();
        } else if removed {
            self.notifier.mark_dirty();
        }
    }

    fn install_views(&self, views: &[Rc<ClientWorkspaceView>]) {
        let mut state = self.state.borrow_mut();
        let existing = state
            .items
            .iter()
            .filter_map(|workspace| {
                workspace
                    .snapshot()
                    .view
                    .as_ref()
                    .map(|view| (view.workspace_id.clone(), workspace.clone()))
            })
            .collect::<IndexMap<_, _>>();
        let mut installed = IndexMap::<WorkspaceId, Rc<ClientWorkspace>>::new();
        for view in views {
            if let Some(duplicate) = installed.get(&view.workspace_id) {
                let _ = duplicate.adopt(view.clone());
                continue;
            }
            let workspace = existing
                .get(&view.workspace_id)
                .cloned()
                .unwrap_or_else(|| {
                    ClientWorkspace::materialized(
                        self.transport.clone(),
                        self.options.scheduler.clone(),
                        view.clone(),
                    )
                });
            let _ = workspace.adopt(view.clone());
            installed.insert(view.workspace_id.clone(), workspace);
        }
        state.items = Rc::new(installed.into_values().collect());
        state.item_views_source = None;
        state.committed_order = views.iter().map(|view| view.workspace_id.clone()).collect();
    }

    async fn call(&self, method: &str, payload: Value) -> Result<ClientRpcResult<Value>, String> {
        self.transport
            .call(SessionTransportRequest {
                method: method.to_owned(),
                payload,
            })
            .await
    }

    fn rebuild_snapshot(&self) {
        let mut state = self.state.borrow_mut();
        let items = item_views(&mut state);
        state.snapshot = Rc::new(WorkspaceListSnapshot {
            items,
            archived_session_ids: state.archived_session_ids.clone(),
            state: state.list_state,
            phase: state.phase,
            error: state.error.clone(),
        });
    }
}

fn item_views(state: &mut ManagerState) -> Rc<Vec<Rc<ClientWorkspaceView>>> {
    if state
        .item_views_source
        .as_ref()
        .is_some_and(|source| Rc::ptr_eq(source, &state.items))
    {
        return state.item_views_cache.clone();
    }
    state.item_views_source = Some(state.items.clone());
    state.item_views_cache = Rc::new(
        state
            .items
            .iter()
            .filter_map(|workspace| workspace.snapshot().view.clone())
            .collect(),
    );
    state.item_views_cache.clone()
}

fn workspace_list(value: &Value) -> Option<(Vec<Rc<ClientWorkspaceView>>, Vec<SessionId>)> {
    let items = value
        .get("items")?
        .as_array()?
        .iter()
        .map(|value| workspace_view(Some(value)).map(Rc::new))
        .collect::<Option<Vec<_>>>()?;
    let archived = session_ids(value.get("archivedSessionIds"))?;
    Some((items, archived))
}

fn workspace_ids(value: Option<&Value>) -> Option<Vec<WorkspaceId>> {
    value?
        .as_array()?
        .iter()
        .map(|id| id.as_str().map(WorkspaceId::new))
        .collect()
}

fn session_ids(value: Option<&Value>) -> Option<Vec<SessionId>> {
    value?
        .as_array()?
        .iter()
        .map(|id| id.as_str().map(SessionId::new))
        .collect()
}

fn apply_workspace_delta(
    mut items: Vec<Rc<ClientWorkspaceView>>,
    delta: &WorkspaceDelta,
) -> Vec<Rc<ClientWorkspaceView>> {
    match delta {
        WorkspaceDelta::Upsert(workspace) => {
            if let Some(index) = items
                .iter()
                .position(|item| item.workspace_id == workspace.workspace_id)
            {
                items[index] = workspace.clone();
            } else {
                items.insert(0, workspace.clone());
            }
            items
        }
        WorkspaceDelta::Remove(workspace_id) => {
            items.retain(|workspace| &workspace.workspace_id != workspace_id);
            items
        }
        WorkspaceDelta::Order(workspace_ids) => {
            let rank = workspace_ids
                .iter()
                .enumerate()
                .map(|(index, id)| (id.clone(), index))
                .collect::<IndexMap<_, _>>();
            items.sort_by_key(|workspace| {
                rank.get(&workspace.workspace_id)
                    .copied()
                    .unwrap_or(usize::MAX)
            });
            items
        }
    }
}

fn insert_id_before(
    ids: &[WorkspaceId],
    id: &WorkspaceId,
    before_id: Option<&WorkspaceId>,
) -> Vec<WorkspaceId> {
    if !ids.contains(id)
        || before_id.is_some_and(|before| !ids.contains(before))
        || before_id == Some(id)
    {
        return ids.to_vec();
    }
    let mut without = ids
        .iter()
        .filter(|candidate| *candidate != id)
        .cloned()
        .collect::<Vec<_>>();
    let at = before_id
        .and_then(|before| without.iter().position(|candidate| candidate == before))
        .unwrap_or(without.len());
    without.insert(at, id.clone());
    without
}
