//! Root Workspaces service projected against the Sessions object layer.

use std::{cell::RefCell, rc::Rc};

use futures::{FutureExt, future::LocalBoxFuture};
use indexmap::IndexMap;
use seekdeep_identity::{SessionId, WorkspaceId};
use serde_json::{Value, json};

use crate::{
    ClientRpcError, ClientRpcResult, ClientWorkspaceView, RuntimeDisposer, RuntimeSessionListState,
    SessionCreateFailure, SessionRuntime, SessionTaskSpawner, SessionTransport,
    SessionTransportRequest, WorkspaceCreateInput, WorkspaceHostFrame, WorkspaceListPhase,
    WorkspaceListState, WorkspaceManager, WorkspaceManagerOptions, WorkspaceRefreshTask,
};

/// Workspace list plus two-baseline readiness and default-target projection.
#[derive(Clone)]
pub struct RuntimeWorkspaceListState {
    /// Host Workspace order.
    pub items: Rc<Vec<Rc<ClientWorkspaceView>>>,
    /// Registry-global archived Sessions.
    pub archived_session_ids: Rc<Vec<SessionId>>,
    /// Pull activity axis.
    pub state: WorkspaceListState,
    /// First-success lifecycle.
    pub phase: WorkspaceListPhase,
    /// Last baseline failure.
    pub error: Option<ClientRpcError>,
    /// Whether Workspace and Session baselines have both succeeded.
    pub baselines_ready: bool,
    /// Most recently active Workspace with Host-order tie breaking.
    pub recent_workspace_id: Option<WorkspaceId>,
}

/// Workspace runtime construction seams.
pub struct WorkspaceRuntimeOptions {
    /// Snapshot scheduler and manager reconnect owner.
    pub manager: WorkspaceManagerOptions,
    /// Detached initial-selection and New Session task owner.
    pub spawner: Rc<dyn SessionTaskSpawner>,
    /// Non-fatal background failure reporter.
    pub report: Rc<dyn Fn(&str)>,
}

/// Structured Workspace create failure.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[error("workspace create failed: {code}: {message}", code=.rpc_error.code, message=.rpc_error.message)]
pub struct WorkspaceCreateFailure {
    /// Host business or folded transport error.
    pub rpc_error: ClientRpcError,
}

/// Structured directory browse failure.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[error("directory browse failed: {code}: {message}", code=.rpc_error.code, message=.rpc_error.message)]
pub struct DirectoryBrowseFailure {
    /// Host business error.
    pub rpc_error: ClientRpcError,
}

/// Directory browse request rejection.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum DirectoryBrowseCallFailure {
    /// Generated-client transport rejection.
    #[error("{0}")]
    Transport(String),
    /// Host business rejection.
    #[error(transparent)]
    Business(DirectoryBrowseFailure),
}

/// Generic Workspace service action rejection.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum WorkspaceActionFailure {
    /// Generated-client transport rejection.
    #[error("{0}")]
    Transport(String),
    /// Host business rejection rendered with its exact source message.
    #[error("{message}")]
    Business {
        /// Source-visible message.
        message: String,
        /// Host error.
        error: Box<ClientRpcError>,
    },
    /// Malformed success response.
    #[error("{0}")]
    Malformed(String),
}

/// Session connection failure used by Workspace selection flows.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum WorkspaceConnectFailure {
    /// The selected Workspace is absent.
    #[error("workspaces.connectWorkspace: unknown workspace {0}")]
    UnknownWorkspace(WorkspaceId),
    /// Session creation failed.
    #[error(transparent)]
    SessionCreate(#[from] SessionCreateFailure),
}

type ConnectTask =
    futures::future::Shared<LocalBoxFuture<'static, Result<SessionId, WorkspaceConnectFailure>>>;

/// Narrow Sessions face consumed by the sibling Workspaces domain.
pub trait WorkspaceSessionsPort {
    /// Current Session-list projection.
    fn list_snapshot(&self) -> Rc<RuntimeSessionListState>;
    /// Subscribes to Session-list changes.
    fn subscribe(&self, listener: Rc<dyn Fn()>) -> RuntimeDisposer;
    /// Creates one Session targeted at a Workspace.
    fn create(
        &self,
        workspace_id: WorkspaceId,
    ) -> LocalBoxFuture<'static, Result<SessionId, SessionCreateFailure>>;
    /// Selects one Session.
    ///
    /// # Errors
    ///
    /// Returns when the Session is not addressable.
    fn open(&self, session_id: &SessionId) -> Result<(), String>;
    /// Clears the current selection.
    fn clear(&self);
}

/// Adapter exposing only [`WorkspaceSessionsPort`] from a [`SessionRuntime`].
pub struct SessionRuntimeWorkspacePort {
    runtime: Rc<SessionRuntime>,
}

impl SessionRuntimeWorkspacePort {
    /// Narrows one Sessions service for the Workspaces domain.
    #[must_use]
    pub fn new(runtime: Rc<SessionRuntime>) -> Rc<Self> {
        Rc::new(Self { runtime })
    }
}

impl WorkspaceSessionsPort for SessionRuntimeWorkspacePort {
    fn list_snapshot(&self) -> Rc<RuntimeSessionListState> {
        self.runtime.list_snapshot()
    }

    fn subscribe(&self, listener: Rc<dyn Fn()>) -> RuntimeDisposer {
        self.runtime.subscribe(listener)
    }

    fn create(
        &self,
        workspace_id: WorkspaceId,
    ) -> LocalBoxFuture<'static, Result<SessionId, SessionCreateFailure>> {
        let runtime = self.runtime.clone();
        async move {
            runtime
                .create(json!({"workspaceId":workspace_id.as_str()}))
                .await
        }
        .boxed_local()
    }

    fn open(&self, session_id: &SessionId) -> Result<(), String> {
        self.runtime.open(session_id)
    }

    fn clear(&self) {
        self.runtime.clear();
    }
}

struct WorkspaceServiceState {
    list: Rc<RuntimeWorkspaceListState>,
    listeners: IndexMap<u64, Rc<dyn Fn()>>,
    next_listener: u64,
    connecting: IndexMap<WorkspaceId, (u64, ConnectTask)>,
    next_connect_token: u64,
    initial_selection_started: bool,
}

/// Real Workspace object layer and Host actions.
pub struct WorkspaceRuntime {
    transport: Rc<dyn SessionTransport>,
    sessions: Rc<dyn WorkspaceSessionsPort>,
    manager: Rc<WorkspaceManager>,
    options: WorkspaceRuntimeOptions,
    state: RefCell<WorkspaceServiceState>,
}

impl WorkspaceRuntime {
    /// Creates the root Workspaces service.
    #[must_use]
    pub fn new(
        transport: Rc<dyn SessionTransport>,
        sessions: &Rc<dyn WorkspaceSessionsPort>,
        options: WorkspaceRuntimeOptions,
    ) -> Rc<Self> {
        let manager = WorkspaceManager::new(
            transport.clone(),
            WorkspaceManagerOptions {
                scheduler: options.manager.scheduler.clone(),
                spawner: options.manager.spawner.clone(),
                parse_date: options.manager.parse_date.clone(),
            },
        );
        let runtime = Rc::new(Self {
            transport,
            sessions: sessions.clone(),
            manager: manager.clone(),
            state: RefCell::new(WorkspaceServiceState {
                list: Rc::new(RuntimeWorkspaceListState {
                    items: Rc::new(Vec::new()),
                    archived_session_ids: Rc::new(Vec::new()),
                    state: WorkspaceListState::Idle,
                    phase: WorkspaceListPhase::Pending,
                    error: None,
                    baselines_ready: false,
                    recent_workspace_id: None,
                }),
                listeners: IndexMap::new(),
                next_listener: 0,
                connecting: IndexMap::new(),
                next_connect_token: 0,
                initial_selection_started: false,
            }),
            options,
        });
        let weak = Rc::downgrade(&runtime);
        let _manager_subscription = manager.subscribe(Rc::new(move || {
            if let Some(runtime) = weak.upgrade() {
                runtime.project();
            }
        }));
        let weak = Rc::downgrade(&runtime);
        let _session_subscription = sessions.subscribe(Rc::new(move || {
            if let Some(runtime) = weak.upgrade() {
                runtime.project();
            }
        }));
        runtime.project();
        runtime
    }

    /// Returns the cached UI-facing Workspace list projection.
    #[must_use]
    pub fn list_snapshot(&self) -> Rc<RuntimeWorkspaceListState> {
        self.state.borrow().list.clone()
    }

    /// Subscribes to synchronous list projection changes.
    #[must_use]
    pub fn subscribe(self: &Rc<Self>, listener: Rc<dyn Fn()>) -> RuntimeDisposer {
        let id = {
            let mut state = self.state.borrow_mut();
            state.next_listener = state.next_listener.wrapping_add(1);
            let id = state.next_listener;
            state.listeners.insert(id, listener);
            id
        };
        let weak = Rc::downgrade(self);
        RuntimeDisposer::new(move || {
            if let Some(runtime) = weak.upgrade() {
                runtime.state.borrow_mut().listeners.shift_remove(&id);
            }
        })
    }

    /// Resolves or creates the blank Session used to enter one Workspace.
    pub fn connect_workspace(self: &Rc<Self>, workspace_id: &WorkspaceId) -> ConnectTask {
        if let Some((_, task)) = self.state.borrow().connecting.get(workspace_id) {
            return task.clone();
        }
        let workspace = self
            .state
            .borrow()
            .list
            .items
            .iter()
            .find(|workspace| &workspace.workspace_id == workspace_id)
            .cloned();
        let Some(workspace) = workspace else {
            let error = WorkspaceConnectFailure::UnknownWorkspace(workspace_id.clone());
            return futures::future::ready(Err(error)).boxed_local().shared();
        };
        let list = self.state.borrow().list.clone();
        let sessions = self.sessions.list_snapshot();
        for id in sessions.ids.iter() {
            let Some(summary) = sessions.by_id.get(id) else {
                continue;
            };
            if summary.blank
                && summary.cwd.as_deref() == Some(workspace.path.as_str())
                && workspace.session_ids.contains(&summary.id)
                && !list.archived_session_ids.contains(&summary.id)
            {
                return futures::future::ready(Ok(summary.id.clone()))
                    .boxed_local()
                    .shared();
            }
        }
        let token = {
            let mut state = self.state.borrow_mut();
            state.next_connect_token = state.next_connect_token.wrapping_add(1);
            state.next_connect_token
        };
        let sessions = self.sessions.clone();
        let weak = Rc::downgrade(self);
        let id = workspace_id.clone();
        let task = async move {
            let result = sessions
                .create(id.clone())
                .await
                .map_err(WorkspaceConnectFailure::SessionCreate);
            if let Some(runtime) = weak.upgrade() {
                let remove = runtime
                    .state
                    .borrow()
                    .connecting
                    .get(&id)
                    .is_some_and(|(current, _)| *current == token);
                if remove {
                    runtime.state.borrow_mut().connecting.shift_remove(&id);
                }
            }
            result
        }
        .boxed_local()
        .shared();
        self.state
            .borrow_mut()
            .connecting
            .insert(workspace_id.clone(), (token, task.clone()));
        task
    }

    /// Whether one Workspace currently owns a coalesced Session-create operation.
    #[must_use]
    pub fn has_inflight_connect(&self, workspace_id: &WorkspaceId) -> bool {
        self.state.borrow().connecting.contains_key(workspace_id)
    }

    /// Starts the one-shot initial Workspace selection policy.
    ///
    /// # Errors
    ///
    /// Returns when the policy was already started.
    pub fn start_initial_selection(self: &Rc<Self>) -> Result<RuntimeDisposer, String> {
        {
            let mut state = self.state.borrow_mut();
            if state.initial_selection_started {
                return Err("workspaces.startInitialSelection: already started".to_owned());
            }
            state.initial_selection_started = true;
        }
        let reconcile_state = Rc::new(RefCell::new(InitialSelectionState::Waiting));
        let disposed = Rc::new(std::cell::Cell::new(false));
        let weak = Rc::downgrade(self);
        let reconcile_state_listener = reconcile_state.clone();
        let disposed_listener = disposed.clone();
        let reconcile = Rc::new(move || {
            if let Some(runtime) = weak.upgrade() {
                runtime.reconcile_initial_selection(&reconcile_state_listener, &disposed_listener);
            }
        });
        let subscription = self.subscribe(reconcile.clone());
        reconcile();
        Ok(RuntimeDisposer::new(move || {
            disposed.set(true);
            subscription.dispose();
        }))
    }

    /// Starts one New Session flow using explicit, current, then recent Workspace targeting.
    pub fn start_session(self: &Rc<Self>, workspace_id: Option<WorkspaceId>) {
        let workspace = self.list_snapshot();
        let sessions = self.sessions.list_snapshot();
        let current_workspace = sessions.current.as_ref().and_then(|current| {
            workspace
                .items
                .iter()
                .find(|workspace| workspace.session_ids.contains(current))
                .map(|workspace| workspace.workspace_id.clone())
        });
        let target = workspace_id
            .or(current_workspace)
            .or_else(|| workspace.recent_workspace_id.clone());
        let Some(target) = target else {
            self.sessions.clear();
            return;
        };
        let task = self.connect_workspace(&target);
        let sessions = self.sessions.clone();
        let report = self.options.report.clone();
        self.options.spawner.spawn(
            async move {
                match task.await {
                    Ok(session_id) => {
                        if let Err(error) = sessions.open(&session_id) {
                            report(&format!("new session failed: {error}"));
                        }
                    }
                    Err(error) => report(&format!("new session failed: {error}")),
                }
            }
            .boxed_local(),
        );
    }

    /// Registers one existing path as a Workspace.
    ///
    /// # Errors
    ///
    /// Returns structured Host business or folded transport failures.
    pub async fn create(
        self: &Rc<Self>,
        input: WorkspaceCreateInput,
    ) -> Result<Rc<ClientWorkspaceView>, WorkspaceCreateFailure> {
        match self.manager.create(input).await {
            ClientRpcResult::Success(Some(value)) => {
                let workspace_id = value
                    .get("workspace")
                    .and_then(|workspace| workspace.get("workspaceId"))
                    .and_then(Value::as_str)
                    .map(WorkspaceId::new);
                workspace_id
                    .and_then(|workspace_id| {
                        self.manager
                            .snapshot()
                            .items
                            .iter()
                            .find(|workspace| workspace.workspace_id == workspace_id)
                            .cloned()
                    })
                    .ok_or_else(|| WorkspaceCreateFailure {
                        rpc_error: malformed_error("workspace.create response omitted workspace"),
                    })
            }
            ClientRpcResult::Success(None) => Err(WorkspaceCreateFailure {
                rpc_error: malformed_error("workspace.create response omitted value"),
            }),
            ClientRpcResult::Failure(rpc_error) => Err(WorkspaceCreateFailure { rpc_error }),
        }
    }

    /// Opens the Host native directory picker.
    ///
    /// # Errors
    ///
    /// Returns transport, business, or malformed-response failures.
    pub async fn pick_directory(&self) -> Result<Option<String>, WorkspaceActionFailure> {
        match self.call("host.pickDirectory", json!({})).await? {
            ClientRpcResult::Success(Some(value)) => value
                .get("path")
                .map(|path| path.as_str().map(ToOwned::to_owned))
                .ok_or_else(|| {
                    WorkspaceActionFailure::Malformed(
                        "directory picker response omitted path".to_owned(),
                    )
                }),
            ClientRpcResult::Success(None) => Err(WorkspaceActionFailure::Malformed(
                "directory picker response omitted value".to_owned(),
            )),
            ClientRpcResult::Failure(error) => {
                Err(business_failure("directory picker", error, false))
            }
        }
    }

    /// Lists one directory level through the Host browse capability.
    ///
    /// # Errors
    ///
    /// Returns transport, structured business, or malformed-response failures.
    pub async fn list_directory(
        &self,
        path: Option<&str>,
    ) -> Result<Value, DirectoryBrowseCallFailure> {
        let payload = path.map_or_else(|| json!({}), |path| json!({"path":path}));
        match self
            .call_raw("host.listDirectory", payload)
            .await
            .map_err(DirectoryBrowseCallFailure::Transport)?
        {
            ClientRpcResult::Success(Some(value)) => Ok(value),
            ClientRpcResult::Success(None) => Err(DirectoryBrowseCallFailure::Transport(
                "directory listing response omitted value".to_owned(),
            )),
            ClientRpcResult::Failure(rpc_error) => Err(DirectoryBrowseCallFailure::Business(
                DirectoryBrowseFailure { rpc_error },
            )),
        }
    }

    /// Creates one child directory through the Host browse capability.
    ///
    /// # Errors
    ///
    /// Returns transport, structured business, or malformed-response failures.
    pub async fn create_directory(
        &self,
        path: &str,
        name: &str,
    ) -> Result<String, DirectoryBrowseCallFailure> {
        match self
            .call_raw("host.createDirectory", json!({"path":path,"name":name}))
            .await
            .map_err(DirectoryBrowseCallFailure::Transport)?
        {
            ClientRpcResult::Success(Some(value)) => value
                .get("path")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    DirectoryBrowseCallFailure::Transport(
                        "directory creation response omitted path".to_owned(),
                    )
                }),
            ClientRpcResult::Success(None) => Err(DirectoryBrowseCallFailure::Transport(
                "directory creation response omitted value".to_owned(),
            )),
            ClientRpcResult::Failure(rpc_error) => Err(DirectoryBrowseCallFailure::Business(
                DirectoryBrowseFailure { rpc_error },
            )),
        }
    }

    /// Opens one filesystem path with the Host operating system.
    ///
    /// # Errors
    ///
    /// Returns transport or Host business failures.
    pub async fn open_path(&self, path: &str) -> Result<(), WorkspaceActionFailure> {
        expect_success(
            self.call("host.openPath", json!({"path":path})).await?,
            "path open",
            false,
        )
    }

    /// Renames one Workspace.
    ///
    /// # Errors
    ///
    /// Returns transport, Host business, or malformed-response failures.
    pub async fn rename(
        &self,
        workspace_id: &WorkspaceId,
        title: &str,
    ) -> Result<Rc<ClientWorkspaceView>, WorkspaceActionFailure> {
        let result = self
            .manager
            .rename(workspace_id, title)
            .await
            .map_err(WorkspaceActionFailure::Transport)?;
        installed_workspace(&self.manager, result, "workspace rename")
    }

    /// Deletes one Workspace registration.
    ///
    /// # Errors
    ///
    /// Returns transport or Host business failures.
    pub async fn delete(&self, workspace_id: &WorkspaceId) -> Result<(), WorkspaceActionFailure> {
        let result = self
            .manager
            .delete(workspace_id)
            .await
            .map_err(WorkspaceActionFailure::Transport)?;
        expect_success(result, "workspace delete", true)
    }

    /// Moves one Workspace in durable registry order.
    ///
    /// # Errors
    ///
    /// Returns transport or Host business failures.
    pub async fn insert_before(
        &self,
        workspace_id: &WorkspaceId,
        before_workspace_id: Option<&WorkspaceId>,
    ) -> Result<(), WorkspaceActionFailure> {
        let result = self
            .manager
            .insert_before(workspace_id, before_workspace_id)
            .await
            .map_err(WorkspaceActionFailure::Transport)?;
        expect_success(result, "workspace reorder", true)
    }

    /// Archives one Session in the registry-global set.
    ///
    /// # Errors
    ///
    /// Returns transport or Host business failures.
    pub async fn archive_session(
        &self,
        session_id: &SessionId,
    ) -> Result<(), WorkspaceActionFailure> {
        let result = self
            .manager
            .archive_session(session_id)
            .await
            .map_err(WorkspaceActionFailure::Transport)?;
        expect_success(result, "session archive", true)
    }

    /// Moves one Session in its Workspace manual order.
    ///
    /// # Errors
    ///
    /// Returns transport, Host business, or malformed-response failures.
    pub async fn insert_session_before(
        &self,
        workspace_id: &WorkspaceId,
        session_id: &SessionId,
        before_session_id: Option<&SessionId>,
    ) -> Result<Rc<ClientWorkspaceView>, WorkspaceActionFailure> {
        let result = self
            .manager
            .insert_session_before(workspace_id, session_id, before_session_id)
            .await
            .map_err(WorkspaceActionFailure::Transport)?;
        installed_workspace(&self.manager, result, "workspace move")
    }

    /// Refreshes the Workspace baseline.
    pub fn refresh(self: &Rc<Self>) -> WorkspaceRefreshTask {
        self.manager.refresh()
    }

    /// Routes one Workspace-related Host frame.
    pub fn handle_host_frame(&self, frame: WorkspaceHostFrame) {
        self.manager.handle_host_frame(frame);
    }

    /// Rebuilds the Workspace baseline after connection.
    pub fn handle_connected(&self) {
        self.manager.handle_connected();
    }

    fn reconcile_initial_selection(
        self: &Rc<Self>,
        selection: &Rc<RefCell<InitialSelectionState>>,
        disposed: &Rc<std::cell::Cell<bool>>,
    ) {
        if disposed.get() || *selection.borrow() != InitialSelectionState::Waiting {
            return;
        }
        let workspace = self.list_snapshot();
        if !workspace.baselines_ready {
            return;
        }
        let current = self.sessions.list_snapshot().current.clone();
        let target = workspace.recent_workspace_id.clone();
        if current.is_some() || target.is_none() {
            *selection.borrow_mut() = InitialSelectionState::Done;
            return;
        }
        let target = target.unwrap();
        *selection.borrow_mut() = InitialSelectionState::Connecting;
        let task = self.connect_workspace(&target);
        let weak = Rc::downgrade(self);
        let selection = selection.clone();
        let disposed = disposed.clone();
        let report = self.options.report.clone();
        self.options.spawner.spawn(
            async move {
                match task.await {
                    Ok(session_id) => {
                        if disposed.get() {
                            return;
                        }
                        if let Some(runtime) = weak.upgrade()
                            && runtime.sessions.list_snapshot().current.is_none()
                        {
                            let _ = runtime.sessions.open(&session_id);
                        }
                        *selection.borrow_mut() = InitialSelectionState::Done;
                    }
                    Err(error) => {
                        if disposed.get() {
                            return;
                        }
                        *selection.borrow_mut() = InitialSelectionState::Waiting;
                        report(&format!("initial workspace selection failed: {error}"));
                    }
                }
            }
            .boxed_local(),
        );
    }

    fn project(&self) {
        let workspace = self.manager.snapshot();
        let sessions = self.sessions.list_snapshot();
        let baselines_ready = workspace.phase == WorkspaceListPhase::Ready
            && sessions.phase == crate::SessionListPhase::Ready;
        if sessions
            .current
            .as_ref()
            .is_some_and(|current| workspace.archived_session_ids.contains(current))
        {
            self.sessions.clear();
        }
        let recent_workspace_id = baselines_ready
            .then(|| {
                recent_workspace(
                    &workspace.items,
                    &sessions,
                    &self.options.manager.parse_date,
                )
            })
            .flatten();
        let listeners = {
            let mut state = self.state.borrow_mut();
            state.list = Rc::new(RuntimeWorkspaceListState {
                items: workspace.items.clone(),
                archived_session_ids: workspace.archived_session_ids.clone(),
                state: workspace.state,
                phase: workspace.phase,
                error: workspace.error.clone(),
                baselines_ready,
                recent_workspace_id,
            });
            state.listeners.values().cloned().collect::<Vec<_>>()
        };
        for listener in listeners {
            listener();
        }
    }

    async fn call(
        &self,
        method: &str,
        payload: Value,
    ) -> Result<ClientRpcResult<Value>, WorkspaceActionFailure> {
        self.call_raw(method, payload)
            .await
            .map_err(WorkspaceActionFailure::Transport)
    }

    async fn call_raw(
        &self,
        method: &str,
        payload: Value,
    ) -> Result<ClientRpcResult<Value>, String> {
        self.transport
            .call(SessionTransportRequest {
                method: method.to_owned(),
                payload,
            })
            .await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InitialSelectionState {
    Waiting,
    Connecting,
    Done,
}

fn recent_workspace(
    workspaces: &[Rc<ClientWorkspaceView>],
    sessions: &RuntimeSessionListState,
    parse_date: &Rc<dyn Fn(&str) -> f64>,
) -> Option<WorkspaceId> {
    let mut selected = None;
    let mut selected_time = f64::NEG_INFINITY;
    for workspace in workspaces {
        let mut latest = f64::NEG_INFINITY;
        for session_id in &workspace.session_ids {
            if let Some(session) = sessions.by_id.get(session_id) {
                #[allow(clippy::cast_precision_loss)]
                {
                    latest = latest.max(session.updated_at as f64);
                }
            }
        }
        if latest == f64::NEG_INFINITY {
            latest = parse_date(&workspace.created_at);
        }
        if selected.is_none() || latest > selected_time {
            selected = Some(workspace.workspace_id.clone());
            selected_time = latest;
        }
    }
    selected
}

fn expect_success(
    result: ClientRpcResult<Value>,
    operation: &'static str,
    include_code: bool,
) -> Result<(), WorkspaceActionFailure> {
    match result {
        ClientRpcResult::Success(_) => Ok(()),
        ClientRpcResult::Failure(error) => Err(business_failure(operation, error, include_code)),
    }
}

fn installed_workspace(
    manager: &Rc<WorkspaceManager>,
    result: ClientRpcResult<Value>,
    operation: &'static str,
) -> Result<Rc<ClientWorkspaceView>, WorkspaceActionFailure> {
    match result {
        ClientRpcResult::Success(Some(value)) => value
            .get("workspace")
            .and_then(|workspace| workspace.get("workspaceId"))
            .and_then(Value::as_str)
            .map(WorkspaceId::new)
            .and_then(|workspace_id| {
                manager
                    .snapshot()
                    .items
                    .iter()
                    .find(|workspace| workspace.workspace_id == workspace_id)
                    .cloned()
            })
            .ok_or_else(|| {
                WorkspaceActionFailure::Malformed(format!("{operation} response omitted workspace"))
            }),
        ClientRpcResult::Success(None) => Err(WorkspaceActionFailure::Malformed(format!(
            "{operation} response omitted value"
        ))),
        ClientRpcResult::Failure(error) => Err(business_failure(operation, error, true)),
    }
}

fn business_failure(
    operation: &'static str,
    error: ClientRpcError,
    include_code: bool,
) -> WorkspaceActionFailure {
    let message = if include_code {
        format!("{operation} failed: {}: {}", error.code, error.message)
    } else {
        format!("{operation} failed: {}", error.message)
    };
    WorkspaceActionFailure::Business {
        message,
        error: Box::new(error),
    }
}

fn malformed_error(message: impl Into<String>) -> ClientRpcError {
    ClientRpcError {
        code: "internal".to_owned(),
        message: message.into(),
        details: serde_json::Map::new(),
    }
}
