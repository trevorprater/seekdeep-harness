//! Root Sessions service: manager projection, persisted selection, scope, and provide lifetimes.

use std::{cell::RefCell, rc::Rc};

use futures::FutureExt;
use indexmap::{IndexMap, IndexSet};
use seekdeep_identity::SessionId;
use serde_json::Value;

use crate::{
    ClientRpcError, ClientRpcResult, ClientSession, RuntimeDisposer, SessionListEntry,
    SessionListPhase, SessionManager, SessionProvideChannel, SessionProvideChannelHost,
    SessionProvideDescriptor, SessionProvideError, SessionProvideInfo, SessionProvideRegistration,
    SessionProvideSubscription, SubagentAddress, SubagentCatalogSnapshot,
};

/// Wire-schema search result bound exposed to presentation plugins.
pub const SESSION_SEARCH_RESULT_LIMIT: usize = 100;

/// Display-facing Session summary projected from the manager list.
#[derive(Clone)]
pub struct RuntimeSessionSummary {
    /// Stable Session identity.
    pub id: SessionId,
    /// Durable title when present.
    pub title: Option<String>,
    /// Durable title, workspace basename, or Session id.
    pub display_title: String,
    /// Working directory.
    pub cwd: Option<String>,
    /// Agent preset identity.
    pub agent_preset: Option<String>,
    /// Parent lineage identity.
    pub parent_id: Option<SessionId>,
    /// Coarse origin.
    pub origin: Option<String>,
    /// Current running bit.
    pub running: bool,
    /// Current pending interaction status.
    pub pending_interaction: Option<Value>,
    /// Unread completion reminder.
    pub completed: bool,
    /// Empty-log bit.
    pub blank: bool,
    /// Last activity timestamp.
    pub updated_at: i64,
    /// Current Host-computed projections.
    pub projection_values: Option<Value>,
}

/// Immutable root Sessions service list snapshot.
pub struct RuntimeSessionListState {
    /// Host-list order.
    pub ids: Rc<Vec<SessionId>>,
    /// Host rows plus addressed breadcrumb rows.
    pub by_id: Rc<IndexMap<SessionId, Rc<RuntimeSessionSummary>>>,
    /// Validated current Session.
    pub current: Option<SessionId>,
    /// First-success list phase.
    pub phase: SessionListPhase,
    /// Direct-child catalogs.
    pub subagents_by_parent: Rc<IndexMap<SessionId, Rc<SubagentCatalogSnapshot>>>,
    /// Background jobs by Session.
    pub jobs_by_session: Rc<IndexMap<SessionId, Rc<Vec<Value>>>>,
    /// Current catalog-derived address.
    pub current_address: Option<SubagentAddress>,
}

/// Persisted current Session and optional addressed route.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionSelection {
    /// Selected Session.
    pub session_id: Option<SessionId>,
    /// Direct-parent transport address.
    pub subagent_address: Option<SubagentAddress>,
}

/// Injected durable selection cell.
pub trait SessionSelectionStorage {
    /// Loads the last persisted selection.
    fn load(&self) -> SessionSelection;
    /// Atomically stores one selected route.
    fn store(&self, selection: &SessionSelection);
    /// Clears durable selection.
    fn clear(&self);
}

/// One minted Agent-scoped Client context marker.
pub struct RuntimeSessionScope {
    /// Session identity tag.
    pub session_id: SessionId,
    /// Target-specific context payload.
    pub payload: Value,
    dispose: RuntimeDisposer,
}

impl RuntimeSessionScope {
    /// Creates one scope marker with its exact teardown.
    #[must_use]
    pub fn new(session_id: SessionId, payload: Value, dispose: RuntimeDisposer) -> Rc<Self> {
        Rc::new(Self {
            session_id,
            payload,
            dispose,
        })
    }

    /// Disposes the whole scope fiber once.
    pub fn dispose(&self) {
        self.dispose.dispose();
    }
}

/// Injected scope factory.
pub trait SessionScopeFactory {
    /// Mints one scope tagged by Session identity.
    fn create(&self, session_id: &SessionId) -> Rc<RuntimeSessionScope>;
}

/// Stable per-Session assembly binding.
pub struct RuntimeSessionBinding {
    /// Session identity.
    pub session_id: SessionId,
    /// Outward resident Session face.
    pub session: Rc<ClientSession>,
    /// Agent-scoped context marker.
    pub scope: Rc<RuntimeSessionScope>,
}

type RuntimeProvideInfo =
    SessionProvideInfo<Rc<ClientSession>, Value, Rc<crate::ProjectionValueStore<Value>>>;
type RuntimeProvideChannel = SessionProvideChannel<
    Rc<ClientSession>,
    Value,
    Rc<crate::ProjectionValueStore<Value>>,
    Rc<RuntimeSessionBinding>,
>;

struct ScopeRecord {
    binding: Rc<RuntimeSessionBinding>,
    provide_info: Rc<RuntimeProvideInfo>,
}

struct ServiceState {
    list: Rc<RuntimeSessionListState>,
    persisted_selection: SessionSelection,
    listeners: IndexMap<u64, Rc<dyn Fn()>>,
    next_listener: u64,
    scopes: IndexMap<SessionId, ScopeRecord>,
    watched: Option<SessionId>,
    deferred_removals: IndexSet<SessionId>,
}

/// Service construction seams.
pub struct SessionRuntimeOptions {
    /// Durable selection cell.
    pub selection: Rc<dyn SessionSelectionStorage>,
    /// Agent scope factory.
    pub scopes: Rc<dyn SessionScopeFactory>,
    /// Detached open/catalog task owner.
    pub spawner: Rc<dyn crate::SessionTaskSpawner>,
    /// Session-keyed Slot Store prune callback.
    pub prune_store_scope: Rc<dyn Fn(&SessionId)>,
}

struct ProvideHost {
    service: std::rc::Weak<SessionRuntime>,
}

impl
    SessionProvideChannelHost<
        Rc<ClientSession>,
        Value,
        Rc<crate::ProjectionValueStore<Value>>,
        Rc<RuntimeSessionBinding>,
    > for ProvideHost
{
    fn rebuild_bundles(&self, channel: &RuntimeProvideChannel) -> Result<(), SessionProvideError> {
        let Some(service) = self.service.upgrade() else {
            return Ok(());
        };
        let bindings = service
            .state
            .borrow()
            .scopes
            .values()
            .map(|record| record.binding.clone())
            .collect::<Vec<_>>();
        let rebuilt = bindings
            .iter()
            .map(|binding| {
                channel.materialize_info(&crate::SessionBinding {
                    session_id: binding.session_id.clone(),
                    session: binding.session.clone(),
                    projections: binding.session.projections(),
                    payload: binding.clone(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut state = service.state.borrow_mut();
        for (binding, info) in bindings.into_iter().zip(rebuilt) {
            if let Some(record) = state.scopes.get_mut(&binding.session_id) {
                record.provide_info = info;
            }
        }
        Ok(())
    }

    fn resolve_current(&self) -> Result<Rc<RuntimeProvideInfo>, SessionProvideError> {
        let Some(service) = self.service.upgrade() else {
            return Err(SessionProvideError::new("SessionRuntime was disposed"));
        };
        let current = service.state.borrow().list.current.clone();
        Ok(current
            .and_then(|id| service.resolve(&id).map(|record| record.provide_info))
            .unwrap_or_else(|| service.provide_channel.maybe_info()))
    }

    fn report_subscriber_failure(&self, _message: &str) {}
}

/// Root Sessions service above [`SessionManager`].
pub struct SessionRuntime {
    manager: Rc<SessionManager>,
    options: SessionRuntimeOptions,
    state: RefCell<ServiceState>,
    provide_channel: Rc<RuntimeProvideChannel>,
}

impl SessionRuntime {
    /// Creates the service and projects the manager's current list immediately.
    #[must_use]
    pub fn new(manager: &Rc<SessionManager>, options: SessionRuntimeOptions) -> Rc<Self> {
        let manager = manager.clone();
        let restored = options.selection.load();
        let initial = Rc::new(RuntimeSessionListState {
            ids: Rc::new(Vec::new()),
            by_id: Rc::new(IndexMap::new()),
            current: None,
            phase: SessionListPhase::Pending,
            subagents_by_parent: Rc::new(IndexMap::new()),
            jobs_by_session: Rc::new(IndexMap::new()),
            current_address: None,
        });
        let service = Rc::new_cyclic(|weak| {
            let provide_channel = SessionProvideChannel::new(Rc::new(ProvideHost {
                service: weak.clone(),
            }));
            Self {
                manager: manager.clone(),
                options,
                state: RefCell::new(ServiceState {
                    list: initial,
                    persisted_selection: restored.clone(),
                    listeners: IndexMap::new(),
                    next_listener: 0,
                    scopes: IndexMap::new(),
                    watched: None,
                    deferred_removals: IndexSet::new(),
                }),
                provide_channel,
            }
        });
        service.manager.restore_selection(&restored);
        let weak = Rc::downgrade(&service);
        let _subscription = manager.subscribe(Rc::new(move || {
            if let Some(service) = weak.upgrade() {
                service.project_list();
            }
        }));
        service.project_list();
        service
    }

    /// Current root list snapshot.
    #[must_use]
    pub fn list_snapshot(&self) -> Rc<RuntimeSessionListState> {
        self.state.borrow().list.clone()
    }

    /// Subscribes to synchronous projected-list writes.
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
            if let Some(service) = weak.upgrade() {
                service.state.borrow_mut().listeners.shift_remove(&id);
            }
        })
    }

    /// Atomic current-Session provide snapshot.
    #[must_use]
    pub fn current_provide_info(&self) -> Rc<RuntimeProvideInfo> {
        self.provide_channel.current_snapshot()
    }

    /// Subscribes to current provide-bundle changes.
    #[must_use]
    pub fn subscribe_current_provide(
        self: &Rc<Self>,
        listener: crate::SessionProvideListener,
    ) -> SessionProvideSubscription {
        self.provide_channel.subscribe_current(listener)
    }

    /// Registers one per-Session standard-props provider.
    ///
    /// # Errors
    ///
    /// Returns declaration or live materialization failures.
    pub fn provide(
        self: &Rc<Self>,
        descriptor: SessionProvideDescriptor<
            Rc<ClientSession>,
            Value,
            Rc<crate::ProjectionValueStore<Value>>,
            Rc<RuntimeSessionBinding>,
        >,
    ) -> Result<SessionProvideRegistration, SessionProvideError> {
        self.provide_channel.provide(descriptor)
    }

    /// Selects one listed or retained-address Session.
    ///
    /// # Errors
    ///
    /// Returns unknown-session diagnostics.
    pub fn open(&self, session_id: &SessionId) -> Result<(), String> {
        self.manager.select(session_id)
    }

    /// Selects one healthy direct child.
    ///
    /// # Errors
    ///
    /// Returns catalog validation diagnostics.
    pub fn open_subagent(&self, address: SubagentAddress) -> Result<(), String> {
        self.manager.select_subagent(address)
    }

    /// Resolves one retained or loaded-catalog direct-parent address.
    #[must_use]
    pub fn subagent_address(&self, session_id: &SessionId) -> Option<SubagentAddress> {
        self.manager.navigation_address(session_id)
    }

    /// Marks whether one direct-child catalog is actively consumed.
    pub fn set_subagent_catalog_open(self: &Rc<Self>, parent_session_id: &SessionId, open: bool) {
        self.manager
            .set_subagent_catalog_open(parent_session_id, open);
    }

    /// Refreshes one direct-child catalog.
    pub async fn refresh_subagents(self: &Rc<Self>, parent_session_id: &SessionId) {
        self.manager.refresh_subagents(parent_session_id).await;
    }

    /// Records one Host-confirmed Agent preset switch.
    pub fn note_agent_preset(&self, session_id: &SessionId, agent_preset: &str) {
        self.manager.note_agent_preset(session_id, agent_preset);
    }

    /// Clears current and durable selection.
    pub fn clear(&self) {
        self.manager.clear_selection();
    }

    /// Refreshes the real Session baseline.
    pub async fn refresh(&self) {
        self.manager.refresh_list().await;
    }

    /// Delegates request-local content search.
    pub async fn search(&self, query: &str) -> ClientRpcResult<Value> {
        self.manager.search(query).await
    }

    /// Creates one Session and guarantees synchronous local addressability on return.
    ///
    /// # Errors
    ///
    /// Returns Host business, transport, or malformed success failures.
    pub async fn create(&self, options: Value) -> Result<SessionId, SessionCreateFailure> {
        let requested = options
            .get("sessionId")
            .and_then(Value::as_str)
            .map(SessionId::new);
        match self.manager.create(options).await {
            ClientRpcResult::Success(Some(value)) => {
                self.project_list();
                value
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(SessionId::new)
                    .ok_or_else(|| SessionCreateFailure {
                        error: internal_error("session.create response omitted sessionId"),
                        requested_session_id: requested,
                    })
            }
            ClientRpcResult::Success(None) => Err(SessionCreateFailure {
                error: internal_error("session.create response omitted value"),
                requested_session_id: requested,
            }),
            ClientRpcResult::Failure(error) => {
                self.project_list();
                Err(SessionCreateFailure {
                    error,
                    requested_session_id: requested,
                })
            }
        }
    }

    /// Forks one Session and optionally increments its inherited durable title.
    ///
    /// # Errors
    ///
    /// Returns Host fork, local addressability, or optional rename failures.
    pub async fn fork(
        &self,
        source_session_id: &SessionId,
        at_seq: Option<f64>,
        increase_title: bool,
    ) -> Result<SessionId, SessionForkFailure> {
        let source_title = increase_title
            .then(|| {
                self.state
                    .borrow()
                    .list
                    .by_id
                    .get(source_session_id)
                    .and_then(|summary| summary.title.clone())
            })
            .flatten();
        let at_seq = match at_seq.map(floor_event_seq).transpose() {
            Ok(at_seq) => at_seq,
            Err(error) => {
                return Err(SessionForkFailure {
                    error,
                    source_session_id: source_session_id.clone(),
                    kind: SessionForkFailureKind::Fork,
                });
            }
        };
        let result = self.manager.fork(source_session_id, at_seq).await;
        let child_id = match result {
            ClientRpcResult::Success(Some(value)) => value
                .get("sessionId")
                .and_then(Value::as_str)
                .map(SessionId::new)
                .ok_or_else(|| SessionForkFailure {
                    error: internal_error("session.fork response omitted sessionId"),
                    source_session_id: source_session_id.clone(),
                    kind: SessionForkFailureKind::Internal,
                })?,
            ClientRpcResult::Success(None) => {
                return Err(SessionForkFailure {
                    error: internal_error("session.fork response omitted value"),
                    source_session_id: source_session_id.clone(),
                    kind: SessionForkFailureKind::Internal,
                });
            }
            ClientRpcResult::Failure(error) => {
                self.project_list();
                return Err(SessionForkFailure {
                    error,
                    source_session_id: source_session_id.clone(),
                    kind: SessionForkFailureKind::Fork,
                });
            }
        };
        self.project_list();
        if let Some(source_title) = source_title {
            let binding = self.binding(&child_id).ok_or_else(|| SessionForkFailure {
                error: internal_error(format!(
                    "fork child \"{child_id}\" is not locally addressable"
                )),
                source_session_id: source_session_id.clone(),
                kind: SessionForkFailureKind::Internal,
            })?;
            if let ClientRpcResult::Failure(error) = binding
                .session
                .rename(&increased_fork_title(&source_title))
                .await
            {
                return Err(SessionForkFailure {
                    error,
                    source_session_id: source_session_id.clone(),
                    kind: SessionForkFailureKind::Rename,
                });
            }
        }
        Ok(child_id)
    }

    /// Resolves one already eligible Agent scope without moving the stage.
    #[must_use]
    pub fn scope(&self, session_id: &SessionId) -> Option<Rc<RuntimeSessionScope>> {
        self.resolve(session_id)
            .map(|record| record.binding.scope.clone())
    }

    /// Resolves one stable Session binding without moving the stage.
    #[must_use]
    pub fn binding(&self, session_id: &SessionId) -> Option<Rc<RuntimeSessionBinding>> {
        self.resolve(session_id).map(|record| record.binding)
    }

    /// Rebuilds every resident Session against the current Conversation registries.
    pub fn rebuild_conversation_registry(&self) {
        self.manager.rebuild_conversation_registry();
    }

    fn resolve(&self, session_id: &SessionId) -> Option<ScopeRecord> {
        if let Some(record) = self.state.borrow().scopes.get(session_id) {
            return Some(ScopeRecord {
                binding: record.binding.clone(),
                provide_info: record.provide_info.clone(),
            });
        }
        if !self.eligible(session_id) {
            return None;
        }
        let scope = self.options.scopes.create(session_id);
        let session = self.manager.get(session_id);
        if session.bind_scope().is_err() {
            scope.dispose();
            return None;
        }
        let binding = Rc::new(RuntimeSessionBinding {
            session_id: session_id.clone(),
            session: session.clone(),
            scope,
        });
        let Ok(provide_info) = self
            .provide_channel
            .materialize_info(&crate::SessionBinding {
                session_id: session_id.clone(),
                session,
                projections: binding.session.projections(),
                payload: binding.clone(),
            })
        else {
            binding.session.unbind_scope();
            binding.scope.dispose();
            return None;
        };
        self.state.borrow_mut().scopes.insert(
            session_id.clone(),
            ScopeRecord {
                binding: binding.clone(),
                provide_info: provide_info.clone(),
            },
        );
        Some(ScopeRecord {
            binding,
            provide_info,
        })
    }

    fn eligible(&self, session_id: &SessionId) -> bool {
        let list = self.state.borrow().list.clone();
        list.current.as_ref() == Some(session_id) || list.ids.contains(session_id)
    }

    fn project_list(&self) {
        let manager = self.manager.snapshot();
        let mut ids = Vec::new();
        let mut by_id = IndexMap::new();
        for entry in manager.items.iter() {
            let summary = runtime_summary(entry);
            ids.push(summary.id.clone());
            by_id.insert(summary.id.clone(), Rc::new(summary));
        }
        self.project_breadcrumbs(&manager, &mut by_id);
        self.persist_current_selection(&manager, &by_id);
        let next = Rc::new(RuntimeSessionListState {
            ids: Rc::new(ids),
            by_id: Rc::new(by_id),
            current: manager.current.clone(),
            phase: manager.phase,
            subagents_by_parent: manager.subagents_by_parent.clone(),
            jobs_by_session: manager.jobs_by_session.clone(),
            current_address: manager.current_address.clone(),
        });
        let listeners = {
            let mut state = self.state.borrow_mut();
            state.list = next;
            state.listeners.values().cloned().collect::<Vec<_>>()
        };
        self.follow_current();
        let _ = self.provide_channel.publish_current();
        for listener in listeners {
            listener();
        }
        self.prune_scopes();
    }

    fn persist_current_selection(
        &self,
        manager: &crate::ManagerListSnapshot,
        by_id: &IndexMap<SessionId, Rc<RuntimeSessionSummary>>,
    ) {
        match &manager.current {
            None => {
                let should_clear = self.state.borrow().persisted_selection.session_id.is_some();
                if should_clear {
                    self.options.selection.clear();
                    self.state.borrow_mut().persisted_selection = SessionSelection::default();
                }
            }
            Some(current) if by_id.contains_key(current) => {
                let selection = SessionSelection {
                    session_id: Some(current.clone()),
                    subagent_address: manager.current_address.clone(),
                };
                if self.state.borrow().persisted_selection != selection {
                    self.options.selection.store(&selection);
                    self.state.borrow_mut().persisted_selection = selection;
                }
            }
            Some(_) => {}
        }
    }

    fn project_breadcrumbs(
        &self,
        manager: &crate::ManagerListSnapshot,
        by_id: &mut IndexMap<SessionId, Rc<RuntimeSessionSummary>>,
    ) {
        let (Some(_), Some(current_address)) = (&manager.current, &manager.current_address) else {
            return;
        };
        let mut address = Some(current_address.clone());
        let mut seen = IndexSet::new();
        while let Some(current_address) = address {
            if !seen.insert(current_address.child_session_id.clone()) {
                break;
            }
            let child = manager
                .subagents_by_parent
                .get(&current_address.parent_session_id)
                .and_then(|catalog| {
                    catalog
                        .entries
                        .iter()
                        .find(|entry| entry.session_id() == &current_address.child_session_id)
                });
            let Some(crate::SubagentCatalogEntry::Child {
                id, label, running, ..
            }) = child
            else {
                break;
            };
            let display = label.clone().unwrap_or_else(|| id.as_str().to_owned());
            if let Some(summary) = by_id.get(id).cloned()
                && summary.display_title != display
            {
                by_id.insert(
                    id.clone(),
                    Rc::new(RuntimeSessionSummary {
                        display_title: display.clone(),
                        ..summary.as_ref().clone()
                    }),
                );
            } else {
                by_id.entry(id.clone()).or_insert_with(|| {
                    Rc::new(RuntimeSessionSummary {
                        id: id.clone(),
                        title: None,
                        display_title: display.clone(),
                        cwd: None,
                        agent_preset: None,
                        parent_id: Some(current_address.parent_session_id.clone()),
                        origin: Some("subagent".to_owned()),
                        running: *running,
                        pending_interaction: None,
                        completed: false,
                        blank: false,
                        updated_at: 0,
                        projection_values: None,
                    })
                });
            }
            let parent = by_id.get(&current_address.parent_session_id);
            if parent.is_some_and(|parent| parent.origin.as_deref() != Some("subagent")) {
                break;
            }
            address = self
                .manager
                .navigation_address(&current_address.parent_session_id);
        }
    }

    fn follow_current(&self) {
        let current = {
            let state = self.state.borrow();
            let current = state.list.current.clone();
            if current.is_none()
                || current
                    .as_ref()
                    .is_some_and(|current| !state.list.by_id.contains_key(current))
                || current == state.watched
            {
                return;
            }
            current
        };
        let Some(current) = current else {
            return;
        };
        self.state.borrow_mut().watched = Some(current.clone());
        self.sweep_deferred();
        if let Some(record) = self.resolve(&current) {
            let open = record.binding.session.open();
            self.options.spawner.spawn(open.boxed_local());
            let refresh = self.manager.refresh_subagents(&current);
            self.options.spawner.spawn(refresh.boxed_local());
        }
    }

    fn prune_scopes(&self) {
        let ids = self
            .state
            .borrow()
            .scopes
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            if self.eligible(&id) {
                continue;
            }
            if self.state.borrow().watched.as_ref() == Some(&id) {
                self.state.borrow_mut().deferred_removals.insert(id);
                continue;
            }
            let record = self.state.borrow_mut().scopes.shift_remove(&id);
            self.state.borrow_mut().deferred_removals.shift_remove(&id);
            if let Some(record) = record {
                self.drop_scope(&id, &record);
            }
        }
    }

    fn drop_scope(&self, id: &SessionId, record: &ScopeRecord) {
        record.binding.scope.dispose();
        record.binding.session.unbind_scope();
        (self.options.prune_store_scope)(id);
        self.manager.drop_session(id);
    }

    fn sweep_deferred(&self) {
        let ids = self
            .state
            .borrow()
            .deferred_removals
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            if self.state.borrow().watched.as_ref() == Some(&id) {
                continue;
            }
            if self.eligible(&id) {
                self.state.borrow_mut().deferred_removals.shift_remove(&id);
                continue;
            }
            let record = self.state.borrow_mut().scopes.shift_remove(&id);
            self.state.borrow_mut().deferred_removals.shift_remove(&id);
            if let Some(record) = record {
                self.drop_scope(&id, &record);
            }
        }
    }
}

/// Structured create failure preserving caller-preallocated identity.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[error("session create failed: {code}: {message}", code=.error.code, message=.error.message)]
pub struct SessionCreateFailure {
    /// Host business or folded transport error.
    pub error: ClientRpcError,
    /// Caller-preallocated identity.
    pub requested_session_id: Option<SessionId>,
}

/// Structured fork failure preserving the source identity.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionForkFailure {
    /// Host business, rename, or folded transport error.
    pub error: ClientRpcError,
    /// Fork source identity.
    pub source_session_id: SessionId,
    /// Operation stage that failed after the source call began.
    pub kind: SessionForkFailureKind,
}

/// Source-visible fork failure class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionForkFailureKind {
    /// The Host fork request failed and maps to `SessionForkError`.
    Fork,
    /// The optional post-publication rename failed and maps to plain `Error`.
    Rename,
    /// A malformed success or missing local child maps to plain `Error`.
    Internal,
}

impl std::fmt::Display for SessionForkFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind {
            SessionForkFailureKind::Fork => write!(
                formatter,
                "session fork failed: {}: {}",
                self.error.code, self.error.message
            ),
            SessionForkFailureKind::Rename => write!(
                formatter,
                "fork child rename failed: {}: {}",
                self.error.code, self.error.message
            ),
            SessionForkFailureKind::Internal => formatter.write_str(&self.error.message),
        }
    }
}

impl std::error::Error for SessionForkFailure {}

/// Workspace basename accepting both separators and trailing separators.
#[must_use]
pub fn workspace_title_of(cwd: &str) -> String {
    cwd.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn runtime_summary(entry: &SessionListEntry) -> RuntimeSessionSummary {
    let summary = &entry.summary;
    RuntimeSessionSummary {
        id: summary.session_id.clone(),
        title: summary.title.clone(),
        display_title: summary.title.clone().unwrap_or_else(|| {
            summary
                .cwd
                .as_deref()
                .map(workspace_title_of)
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| summary.session_id.as_str().to_owned())
        }),
        cwd: summary.cwd.clone(),
        agent_preset: summary.agent_preset.clone(),
        parent_id: summary.parent_session_id.clone(),
        origin: summary.origin.clone(),
        running: summary.running,
        pending_interaction: entry.pending_interaction.clone(),
        completed: entry.completed,
        blank: summary.blank,
        updated_at: summary.updated_at,
        projection_values: summary.projection_values.clone(),
    }
}

fn increased_fork_title(title: &str) -> String {
    for (open, close) in [('(', ')'), ('（', '）')] {
        if let Some(prefix) = title.strip_suffix(close)
            && let Some((base, digits)) = prefix.rsplit_once(open)
            && let Ok(number) = digits.parse::<u128>()
        {
            return format!("{base}{open}{}{close}", number + 1);
        }
    }
    format!("{title} (1)")
}

fn internal_error(message: impl Into<String>) -> ClientRpcError {
    ClientRpcError {
        code: "internal".to_owned(),
        message: message.into(),
        details: serde_json::Map::new(),
    }
}

fn floor_event_seq(value: f64) -> Result<u64, ClientRpcError> {
    if !value.is_finite() || !(0.0..=9_007_199_254_740_991.0).contains(&value) {
        return Err(ClientRpcError {
            code: "bad-request".to_owned(),
            message: "invalid payload for session.fork".to_owned(),
            details: serde_json::Map::new(),
        });
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(value.floor() as u64)
}
