//! Session instance cluster, ordered list projection, and pre-instantiation frame routing.

use std::{cell::RefCell, rc::Rc};

use futures::{FutureExt, future::LocalBoxFuture};
use indexmap::{IndexMap, IndexSet};
use seekdeep_identity::{RpcId, SessionId};
use serde_json::{Map, Value, json};

use crate::{
    ClientRpcError, ClientRpcResult, ClientSession, ConversationNodeAssembler, LineageLogger,
    Notifier, NotifierScheduler, PendingInteractionStatus, ProjectionValueStore, RuntimeDisposer,
    SessionListEntry, SessionMuxFrame, SessionOptions, SessionTaskSpawner, SessionTransport,
    SessionTransportRequest, TitledSessionSummary, flatten_lineage,
};

/// Host summary used by the Client Session list.
#[derive(Clone, Debug, PartialEq)]
pub struct ManagerSessionSummary {
    /// Stable Session identity.
    pub session_id: SessionId,
    /// Last activity epoch milliseconds.
    pub updated_at: i64,
    /// Current Agent running bit.
    pub running: bool,
    /// Whether the durable log is empty.
    pub blank: bool,
    /// Parent Session for lineage.
    pub parent_session_id: Option<SessionId>,
    /// Coarse origin.
    pub origin: Option<String>,
    /// Working directory.
    pub cwd: Option<String>,
    /// Agent preset identity.
    pub agent_preset: Option<String>,
    /// Partial per-key projection baseline from `session.list`.
    pub projections: Option<crate::ProjectionsBaseline<Value>>,
}

/// Session-list request lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionListState {
    /// No pull in flight and no current failure.
    Idle,
    /// Pull in flight.
    Loading,
    /// Latest pull failed.
    Error,
}

/// First-success arrival lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionListPhase {
    /// No successful baseline yet.
    Pending,
    /// At least one baseline landed; monotonic.
    Ready,
}

/// Immutable manager list snapshot.
pub struct ManagerListSnapshot {
    /// Flattened lineage rows with stable entry identities.
    pub items: Rc<Vec<Rc<SessionListEntry>>>,
    /// Selected listed Session.
    pub current: Option<SessionId>,
    /// Pull lifecycle.
    pub state: SessionListState,
    /// First-success lifecycle.
    pub phase: SessionListPhase,
    /// Latest list failure.
    pub error: Option<ClientRpcError>,
    /// Background jobs by Session; absent means empty.
    pub jobs_by_session: Rc<IndexMap<SessionId, Rc<Vec<Value>>>>,
}

/// Mux envelope routed through the manager.
pub struct ManagerMuxEnvelope {
    /// Carrier correlation identity.
    pub rpc_id: RpcId,
    /// Owning Session.
    pub session_id: SessionId,
    /// Manager-special or Session-local frame.
    pub frame: ManagerMuxFrame,
}

/// Mux frame classes owned by the manager.
pub enum ManagerMuxFrame {
    /// Session-local frame.
    Session(SessionMuxFrame),
    /// Host-computed whole projection value.
    Projection {
        /// Projection key.
        key: String,
        /// Whole immutable value.
        value: Rc<Value>,
        /// Durable watermark.
        seq: i64,
    },
    /// Whole background-job set.
    Jobs(Vec<Value>),
    /// Stream failure already consumed by the Controller.
    StreamError,
    /// Merge-extensible unknown frame.
    Unknown,
}

/// Host stream envelope routed through the manager.
pub enum ManagerHostFrame {
    /// Newly published Session.
    Added(ManagerSessionSummary),
    /// Session removal or subagent activation detachment.
    Removed {
        /// Session identity.
        session_id: SessionId,
    },
    /// Running-bit change.
    Status {
        /// Session identity.
        session_id: SessionId,
        /// New running state.
        running: bool,
    },
    /// Unpositioned live Agent failure.
    AgentError {
        /// Session identity.
        session_id: SessionId,
        /// Stable rendered message.
        message: String,
    },
    /// Merge-extensible unknown frame.
    Unknown,
}

/// Manager construction seams.
pub struct SessionManagerOptions {
    /// Notification scheduler shared with Sessions.
    pub scheduler: Rc<dyn NotifierScheduler>,
    /// Detached task owner shared with Sessions.
    pub spawner: Rc<dyn SessionTaskSpawner>,
    /// Browser time-zone resolver.
    pub resolve_time_zone: Rc<dyn Fn() -> Result<String, String>>,
    /// Creates one fresh Session-owned assembler.
    pub create_conversation: Rc<dyn Fn() -> ConversationNodeAssembler>,
    /// Injected clock for Host-added summaries and local entity echoes.
    pub clock: Rc<dyn Fn() -> i64>,
    /// Contained diagnostic sink.
    pub report: Rc<dyn Fn(String)>,
}

#[derive(Clone)]
enum ListMutation {
    Upsert(ManagerSessionSummary),
    Remove(SessionId),
    Status(SessionId, bool),
    Activity(SessionId, i64),
    Engaged(SessionId),
}

#[derive(Clone)]
struct BufferedFrame {
    rpc_id: RpcId,
    frame: SessionMuxFrame,
}

struct ManagerState {
    sessions: IndexMap<SessionId, Rc<ClientSession>>,
    pending_buffers: IndexMap<SessionId, Vec<BufferedFrame>>,
    pending_interactions: IndexMap<SessionId, IndexMap<String, PendingInteractionStatus>>,
    completed: IndexSet<SessionId>,
    previous_running: IndexMap<SessionId, bool>,
    projection_stores: IndexMap<SessionId, Rc<ProjectionValueStore<Value>>>,
    summaries: Vec<ManagerSessionSummary>,
    list_state: SessionListState,
    list_phase: SessionListPhase,
    list_error: Option<ClientRpcError>,
    next_list_token: u64,
    list_inflight: Option<(u64, futures::future::Shared<LocalBoxFuture<'static, ()>>)>,
    list_mutations: Option<Rc<RefCell<Vec<ListMutation>>>>,
    jobs_by_session: IndexMap<SessionId, Rc<Vec<Value>>>,
    selected: Option<SessionId>,
    entry_cache: IndexMap<SessionId, Rc<SessionListEntry>>,
    items_cache: Rc<Vec<Rc<SessionListEntry>>>,
    jobs_cache: Option<Rc<IndexMap<SessionId, Rc<Vec<Value>>>>>,
    snapshot: Rc<ManagerListSnapshot>,
}

/// Lazy resident Session cluster and authoritative list projection.
pub struct SessionManager {
    transport: Rc<dyn SessionTransport>,
    options: SessionManagerOptions,
    state: RefCell<ManagerState>,
    notifier: Rc<Notifier>,
}

impl SessionManager {
    /// Creates one manager with an optional restored selection candidate.
    #[must_use]
    pub fn new(
        transport: Rc<dyn SessionTransport>,
        restored_selection: Option<SessionId>,
        options: SessionManagerOptions,
    ) -> Rc<Self> {
        let initial = Rc::new(ManagerListSnapshot {
            items: Rc::new(Vec::new()),
            current: None,
            state: SessionListState::Idle,
            phase: SessionListPhase::Pending,
            error: None,
            jobs_by_session: Rc::new(IndexMap::new()),
        });
        Rc::new_cyclic(move |weak: &std::rc::Weak<Self>| {
            let weak = weak.clone();
            let notifier = Notifier::new(
                Rc::new(move || {
                    if let Some(manager) = weak.upgrade() {
                        manager.rebuild_snapshot();
                    }
                }),
                options.scheduler.clone(),
            );
            Self {
                transport,
                state: RefCell::new(ManagerState {
                    sessions: IndexMap::new(),
                    pending_buffers: IndexMap::new(),
                    pending_interactions: IndexMap::new(),
                    completed: IndexSet::new(),
                    previous_running: IndexMap::new(),
                    projection_stores: IndexMap::new(),
                    summaries: Vec::new(),
                    list_state: SessionListState::Idle,
                    list_phase: SessionListPhase::Pending,
                    list_error: None,
                    next_list_token: 0,
                    list_inflight: None,
                    list_mutations: None,
                    jobs_by_session: IndexMap::new(),
                    selected: restored_selection,
                    entry_cache: IndexMap::new(),
                    items_cache: Rc::new(Vec::new()),
                    jobs_cache: None,
                    snapshot: initial,
                }),
                options,
                notifier,
            }
        })
    }

    /// Subscribes to committed list-snapshot changes.
    #[must_use]
    pub fn subscribe(&self, listener: Rc<dyn Fn()>) -> RuntimeDisposer {
        self.notifier.subscribe(listener)
    }

    /// Returns the cached list snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Rc<ManagerListSnapshot> {
        self.notifier.ensure_fresh();
        self.state.borrow().snapshot.clone()
    }

    /// Selects one listed Session and consumes its completion reminder.
    ///
    /// # Errors
    ///
    /// Returns when the Session is unknown to the current list.
    pub fn select(&self, session_id: &SessionId) -> Result<(), String> {
        let mut state = self.state.borrow_mut();
        if !state
            .summaries
            .iter()
            .any(|summary| summary.session_id == *session_id)
        {
            return Err(format!("sessions.select: unknown session {session_id}"));
        }
        state.selected = Some(session_id.clone());
        state.completed.shift_remove(session_id);
        drop(state);
        self.notifier.notify_now();
        Ok(())
    }

    /// Clears the current selection synchronously.
    pub fn clear_selection(&self) {
        self.state.borrow_mut().selected = None;
        self.notifier.notify_now();
    }

    /// Drops one materialized Session; durable history rebuilds it later.
    pub fn drop_session(&self, session_id: &SessionId) {
        self.state.borrow_mut().sessions.shift_remove(session_id);
    }

    /// Lazily returns one resident Session, replaying buffered answerable and queue frames first.
    #[must_use]
    pub fn get(self: &Rc<Self>, session_id: &SessionId) -> Rc<ClientSession> {
        if let Some(session) = self.state.borrow().sessions.get(session_id) {
            return session.clone();
        }
        let projections = self.projection_store(session_id);
        let weak = Rc::downgrade(self);
        let session = ClientSession::new(
            session_id.clone(),
            self.transport.clone(),
            SessionOptions {
                address: None,
                parent_available: false,
                projections: Some(projections),
                conversation: Some((self.options.create_conversation)()),
                scheduler: self.options.scheduler.clone(),
                spawner: self.options.spawner.clone(),
                resolve_time_zone: self.options.resolve_time_zone.clone(),
                on_engaged: Some(Rc::new(move |session_id| {
                    if let Some(manager) = weak.upgrade() {
                        manager.record_mutation(&ListMutation::Engaged(session_id));
                    }
                })),
                report: self.options.report.clone(),
            },
        );
        let (buffered, summary) = {
            let mut state = self.state.borrow_mut();
            state.sessions.insert(session_id.clone(), session.clone());
            (
                state
                    .pending_buffers
                    .shift_remove(session_id)
                    .unwrap_or_default(),
                state
                    .summaries
                    .iter()
                    .find(|summary| summary.session_id == *session_id)
                    .cloned(),
            )
        };
        for buffered in buffered {
            session.handle_mux_envelope(buffered.rpc_id, buffered.frame);
        }
        if let Some(summary) = summary {
            session.handle_blank(summary.blank);
            session.handle_running(summary.running);
        }
        session
    }

    /// Rebuilds every materialized Session after one Registry transaction.
    pub fn rebuild_conversation_registry(&self) {
        for session in self.state.borrow().sessions.values() {
            session.rebuild_conversation_registry();
        }
    }

    /// Full `session.list` refresh with one shared in-flight operation.
    pub fn refresh_list(self: &Rc<Self>) -> futures::future::Shared<LocalBoxFuture<'static, ()>> {
        if let Some((_, inflight)) = &self.state.borrow().list_inflight {
            return inflight.clone();
        }
        let (token, established, mutations) = {
            let mut state = self.state.borrow_mut();
            state.list_state = SessionListState::Loading;
            state.list_error = None;
            state.next_list_token = state.next_list_token.wrapping_add(1);
            let mutations = Rc::new(RefCell::new(Vec::new()));
            state.list_mutations = Some(mutations.clone());
            (state.next_list_token, state.summaries.clone(), mutations)
        };
        self.notifier.mark_dirty();
        let weak = Rc::downgrade(self);
        let task = async move {
            if let Some(manager) = weak.upgrade() {
                manager
                    .run_list_refresh(token, established, mutations)
                    .await;
            }
        }
        .boxed_local()
        .shared();
        self.state.borrow_mut().list_inflight = Some((token, task.clone()));
        task
    }

    async fn run_list_refresh(
        self: &Rc<Self>,
        token: u64,
        established: Vec<ManagerSessionSummary>,
        mutations: Rc<RefCell<Vec<ListMutation>>>,
    ) {
        let result = match self
            .transport
            .call(SessionTransportRequest {
                method: "session.list".to_owned(),
                payload: json!({}),
            })
            .await
        {
            Ok(ClientRpcResult::Success(Some(value))) => parse_list(&value),
            Ok(ClientRpcResult::Success(None)) => {
                Err(internal_error("session.list response omitted value"))
            }
            Ok(ClientRpcResult::Failure(error)) => Err(error),
            Err(error) => Err(internal_error(error)),
        };
        match result {
            Err(error) => {
                let mut state = self.state.borrow_mut();
                state.list_state = SessionListState::Error;
                state.list_error = Some(error);
            }
            Ok(items) => {
                let phase = self.state.borrow().list_phase;
                let baseline = if phase == SessionListPhase::Pending {
                    items.clone()
                } else {
                    crate::merge_ordered_baseline(&established, &items, |summary| {
                        summary.session_id.clone()
                    })
                };
                {
                    let mut state = self.state.borrow_mut();
                    for summary in &baseline {
                        state
                            .previous_running
                            .entry(summary.session_id.clone())
                            .or_insert(summary.running);
                    }
                    state.summaries = baseline;
                    for mutation in mutations.borrow().iter() {
                        state.summaries = apply_mutation(&state.summaries, mutation);
                        sync_completed(&mut state);
                    }
                    state.list_state = SessionListState::Idle;
                    state.list_phase = SessionListPhase::Ready;
                    sync_completed(&mut state);
                }
                let summaries = self.state.borrow().summaries.clone();
                for summary in &summaries {
                    if let Some(session) = self.state.borrow().sessions.get(&summary.session_id) {
                        session.handle_blank(summary.blank);
                        session.handle_running(summary.running);
                    }
                }
                for summary in &items {
                    let Some(block) = &summary.projections else {
                        continue;
                    };
                    let store = self.projection_store(&summary.session_id);
                    for (key, value) in &block.values {
                        store.apply(key.clone(), value.clone(), block.as_of_seq);
                    }
                }
            }
        }
        {
            let mut state = self.state.borrow_mut();
            if state
                .list_inflight
                .as_ref()
                .is_some_and(|(current, _)| *current == token)
            {
                state.list_mutations = None;
                state.list_inflight = None;
            }
        }
        self.notifier.mark_dirty();
    }

    /// Routes one mux frame without instantiating Sessions for durable frames.
    pub fn handle_mux_envelope(self: &Rc<Self>, envelope: ManagerMuxEnvelope) {
        let ManagerMuxEnvelope {
            rpc_id,
            session_id,
            frame,
        } = envelope;
        match frame {
            ManagerMuxFrame::StreamError | ManagerMuxFrame::Unknown => {}
            ManagerMuxFrame::Projection { key, value, seq } => {
                self.projection_store(&session_id).apply(key, value, seq);
                self.notifier.mark_dirty();
            }
            ManagerMuxFrame::Jobs(jobs) => {
                let mut state = self.state.borrow_mut();
                if jobs.is_empty() {
                    state.jobs_by_session.shift_remove(&session_id);
                } else {
                    state.jobs_by_session.insert(session_id, Rc::new(jobs));
                }
                state.jobs_cache = None;
                drop(state);
                self.notifier.mark_dirty();
            }
            ManagerMuxFrame::Session(frame) => {
                self.handle_session_frame(rpc_id, session_id, frame);
            }
        }
    }

    fn handle_session_frame(
        self: &Rc<Self>,
        rpc_id: RpcId,
        session_id: SessionId,
        frame: SessionMuxFrame,
    ) {
        if let SessionMuxFrame::Event(entry) = &frame
            && entry.event.event_type == "user/message"
            && entry.event.data["source"]["kind"].as_str() == Some("user")
        {
            self.record_mutation(&ListMutation::Activity(
                session_id.clone(),
                entry.event.time,
            ));
        }
        if let SessionMuxFrame::Subscribed { last_seq } = &frame {
            if let Some(store) = self.state.borrow().projection_stores.get(&session_id) {
                store.truncate(i64::try_from(*last_seq).unwrap_or(i64::MAX));
            }
            let mut state = self.state.borrow_mut();
            state.jobs_by_session.shift_remove(&session_id);
            state.jobs_cache = None;
            if let Some(buffer) = state.pending_buffers.get_mut(&session_id) {
                buffer.retain(|buffered| !matches!(buffered.frame, SessionMuxFrame::Queue(_)));
                if buffer.is_empty() {
                    state.pending_buffers.shift_remove(&session_id);
                }
            }
            drop(state);
            self.notifier.mark_dirty();
        }
        self.update_pending_status(&session_id, &rpc_id, &frame);
        if let Some(session) = self.state.borrow().sessions.get(&session_id).cloned() {
            session.handle_mux_envelope(rpc_id, frame);
            return;
        }
        let key = buffered_key(&rpc_id, &frame);
        match frame {
            SessionMuxFrame::ApprovalRequested { .. }
            | SessionMuxFrame::QuestionRequested { .. }
            | SessionMuxFrame::Queue(_) => {
                let mut state = self.state.borrow_mut();
                let buffer = state.pending_buffers.entry(session_id).or_default();
                if let Some(index) = buffer
                    .iter()
                    .position(|buffered| buffered_key(&buffered.rpc_id, &buffered.frame) == key)
                {
                    buffer[index] = BufferedFrame { rpc_id, frame };
                } else {
                    buffer.push(BufferedFrame { rpc_id, frame });
                }
            }
            SessionMuxFrame::ApprovalResolved { .. } | SessionMuxFrame::QuestionResolved { .. } => {
                let mut state = self.state.borrow_mut();
                if let Some(buffer) = state.pending_buffers.get_mut(&session_id) {
                    buffer
                        .retain(|buffered| buffered_key(&buffered.rpc_id, &buffered.frame) != key);
                    if buffer.is_empty() {
                        state.pending_buffers.shift_remove(&session_id);
                    }
                }
            }
            SessionMuxFrame::Event(_)
            | SessionMuxFrame::Subscribed { .. }
            | SessionMuxFrame::Unknown => {}
        }
    }

    /// Routes one Host frame into list upkeep and materialized Sessions.
    pub fn handle_host_frame(&self, frame: ManagerHostFrame) {
        match frame {
            ManagerHostFrame::Added(mut summary) => {
                if summary.updated_at == 0 {
                    summary.updated_at = (self.options.clock)();
                }
                let session_id = summary.session_id.clone();
                let blank = summary.blank;
                self.record_mutation(&ListMutation::Upsert(summary));
                if let Some(session) = self.state.borrow().sessions.get(&session_id) {
                    session.handle_blank(blank);
                }
            }
            ManagerHostFrame::Removed { session_id } => {
                self.record_mutation(&ListMutation::Remove(session_id.clone()));
                let mut state = self.state.borrow_mut();
                if let Some(session) = state.sessions.get(&session_id) {
                    session.handle_removed();
                }
                state.pending_buffers.shift_remove(&session_id);
                state.pending_interactions.shift_remove(&session_id);
                state.jobs_by_session.shift_remove(&session_id);
                state.jobs_cache = None;
                state.projection_stores.shift_remove(&session_id);
                state.completed.shift_remove(&session_id);
            }
            ManagerHostFrame::Status {
                session_id,
                running,
            } => {
                self.record_mutation(&ListMutation::Status(session_id.clone(), running));
                if let Some(session) = self.state.borrow().sessions.get(&session_id) {
                    session.handle_running(running);
                }
            }
            ManagerHostFrame::AgentError {
                session_id,
                message,
            } => {
                if let Some(session) = self.state.borrow().sessions.get(&session_id) {
                    session.handle_agent_error(message);
                }
            }
            ManagerHostFrame::Unknown => {}
        }
    }

    /// Drops connection-generation pending state before the next replay begins.
    pub fn handle_disconnected(&self) {
        let mut changed = false;
        {
            let mut state = self.state.borrow_mut();
            if !state.pending_interactions.is_empty() {
                state.pending_interactions.clear();
                changed = true;
            }
            for buffer in state.pending_buffers.values_mut() {
                let before = buffer.len();
                buffer.retain(|buffered| {
                    !matches!(
                        buffered.frame,
                        SessionMuxFrame::ApprovalRequested { .. }
                            | SessionMuxFrame::QuestionRequested { .. }
                    )
                });
                changed = changed || before != buffer.len();
            }
            state.pending_buffers.retain(|_, buffer| !buffer.is_empty());
        }
        if changed {
            self.notifier.mark_dirty();
        }
    }

    /// Refreshes the list and resyncs only Sessions that have left the cold state.
    pub fn handle_connected(self: &Rc<Self>) {
        let refresh = self.refresh_list();
        self.options.spawner.spawn(refresh.boxed_local());
        for session in self.state.borrow().sessions.values() {
            let resync = session.resync();
            self.options.spawner.spawn(resync);
        }
    }

    fn projection_store(
        self: &Rc<Self>,
        session_id: &SessionId,
    ) -> Rc<ProjectionValueStore<Value>> {
        if let Some(store) = self.state.borrow().projection_stores.get(session_id) {
            return store.clone();
        }
        let store = Rc::new(ProjectionValueStore::new(self.options.scheduler.clone()));
        let weak = Rc::downgrade(self);
        let _subscription = store.subscribe_any(Rc::new(move || {
            if let Some(manager) = weak.upgrade() {
                manager.notifier.mark_dirty();
            }
        }));
        self.state
            .borrow_mut()
            .projection_stores
            .insert(session_id.clone(), store.clone());
        store
    }

    fn update_pending_status(
        &self,
        session_id: &SessionId,
        rpc_id: &RpcId,
        frame: &SessionMuxFrame,
    ) {
        let update = match frame {
            SessionMuxFrame::ApprovalRequested { payload } => Some((
                format!(
                    "a:{}",
                    payload
                        .get("approvalId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                ),
                Some(PendingInteractionStatus::Approval),
            )),
            SessionMuxFrame::ApprovalResolved { approval_id } => {
                Some((format!("a:{approval_id}"), None))
            }
            SessionMuxFrame::QuestionRequested { payload } => {
                Some((format!("q:{rpc_id}"), Some(question_status(payload))))
            }
            SessionMuxFrame::QuestionResolved { question_rpc_id } => {
                Some((format!("q:{question_rpc_id}"), None))
            }
            SessionMuxFrame::Event(_)
            | SessionMuxFrame::Queue(_)
            | SessionMuxFrame::Subscribed { .. }
            | SessionMuxFrame::Unknown => None,
        };
        let Some((key, status)) = update else {
            return;
        };
        let mut state = self.state.borrow_mut();
        match status {
            Some(status) => {
                state
                    .pending_interactions
                    .entry(session_id.clone())
                    .or_default()
                    .insert(key, status);
            }
            None => {
                if let Some(interactions) = state.pending_interactions.get_mut(session_id) {
                    interactions.shift_remove(&key);
                    if interactions.is_empty() {
                        state.pending_interactions.shift_remove(session_id);
                    }
                }
            }
        }
        drop(state);
        self.notifier.mark_dirty();
    }

    fn record_mutation(&self, mutation: &ListMutation) {
        let mut state = self.state.borrow_mut();
        if let Some(mutations) = &state.list_mutations {
            mutations.borrow_mut().push(mutation.clone());
        }
        state.summaries = apply_mutation(&state.summaries, mutation);
        sync_completed(&mut state);
        drop(state);
        self.notifier.mark_dirty();
    }

    fn rebuild_snapshot(&self) {
        let mut state = self.state.borrow_mut();
        let mut titled = Vec::new();
        for summary in &state.summaries {
            let store = state.projection_stores.get(&summary.session_id);
            let title = store
                .and_then(|store| store.get("title"))
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .filter(|title| !title.is_empty());
            let projection_values = store.map(|store| {
                Value::Object(
                    store
                        .values()
                        .iter()
                        .map(|(key, value)| (key.clone(), value.as_ref().clone()))
                        .collect(),
                )
            });
            titled.push(TitledSessionSummary {
                session_id: summary.session_id.clone(),
                title,
                updated_at: summary.updated_at,
                running: summary.running,
                blank: summary.blank,
                parent_session_id: summary.parent_session_id.clone(),
                origin: summary.origin.clone(),
                cwd: summary.cwd.clone(),
                agent_preset: summary.agent_preset.clone(),
                projection_values,
            });
        }
        let pending = state
            .pending_interactions
            .iter()
            .filter_map(|(session_id, interactions)| {
                let statuses = interactions.values().copied().collect::<Vec<_>>();
                let status = statuses
                    .iter()
                    .find(|status| **status != PendingInteractionStatus::Approval)
                    .copied()
                    .or_else(|| statuses.first().copied())?;
                Some((
                    session_id.clone(),
                    Value::String(status_name(status).to_owned()),
                ))
            })
            .collect();
        let completed = state.completed.iter().cloned().collect();
        let warn: LineageLogger = Rc::new(|_| {});
        let fresh = flatten_lineage(&titled, &pending, &completed, &warn);
        let mut items = Vec::new();
        for entry in fresh {
            if let Some(previous) = state.entry_cache.get(&entry.summary.session_id)
                && previous.as_ref() == &entry
            {
                items.push(previous.clone());
            } else {
                let entry = Rc::new(entry);
                state
                    .entry_cache
                    .insert(entry.summary.session_id.clone(), entry.clone());
                items.push(entry);
            }
        }
        state
            .entry_cache
            .retain(|id, _| items.iter().any(|entry| entry.summary.session_id == *id));
        if state.items_cache.len() != items.len()
            || !state
                .items_cache
                .iter()
                .zip(&items)
                .all(|(left, right)| Rc::ptr_eq(left, right))
        {
            state.items_cache = Rc::new(items);
        }
        if state.jobs_cache.is_none() {
            state.jobs_cache = Some(Rc::new(state.jobs_by_session.clone()));
        }
        let current = state.selected.as_ref().filter(|selected| {
            state
                .items_cache
                .iter()
                .any(|entry| entry.summary.session_id == **selected)
        });
        state.snapshot = Rc::new(ManagerListSnapshot {
            items: state.items_cache.clone(),
            current: current.cloned(),
            state: state.list_state,
            phase: state.list_phase,
            error: state.list_error.clone(),
            jobs_by_session: state.jobs_cache.clone().unwrap_or_default(),
        });
    }
}

fn parse_list(value: &Value) -> Result<Vec<ManagerSessionSummary>, ClientRpcError> {
    let items = value
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| internal_error("session.list response requires items"))?;
    items
        .iter()
        .map(|item| {
            let session_id = item
                .get("sessionId")
                .and_then(Value::as_str)
                .ok_or_else(|| internal_error("session.list item requires sessionId"))?;
            let projections = item.get("projections").map(|block| {
                let as_of_seq = block.get("asOfSeq").and_then(Value::as_i64).unwrap_or(-1);
                let values = block
                    .get("values")
                    .and_then(Value::as_object)
                    .map(|values| {
                        values
                            .iter()
                            .map(|(key, value)| (key.clone(), Rc::new(value.clone())))
                            .collect()
                    })
                    .unwrap_or_default();
                crate::ProjectionsBaseline { as_of_seq, values }
            });
            Ok(ManagerSessionSummary {
                session_id: SessionId::new(session_id),
                updated_at: item.get("updatedAt").and_then(Value::as_i64).unwrap_or(0),
                running: item
                    .get("running")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                blank: item.get("blank").and_then(Value::as_bool).unwrap_or(false),
                parent_session_id: item
                    .get("parentSessionId")
                    .and_then(Value::as_str)
                    .map(SessionId::new),
                origin: item
                    .get("origin")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                cwd: item
                    .get("cwd")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                agent_preset: item
                    .get("agentPreset")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                projections,
            })
        })
        .collect()
}

fn apply_mutation(
    summaries: &[ManagerSessionSummary],
    mutation: &ListMutation,
) -> Vec<ManagerSessionSummary> {
    match mutation {
        ListMutation::Upsert(incoming) => {
            let Some(existing) = summaries
                .iter()
                .find(|summary| summary.session_id == incoming.session_id)
            else {
                let mut next = vec![incoming.clone()];
                next.extend_from_slice(summaries);
                return next;
            };
            let mut filled = existing.clone();
            filled.blank = existing.blank && incoming.blank;
            if filled.cwd.is_none() {
                filled.cwd.clone_from(&incoming.cwd);
            }
            if filled.parent_session_id.is_none() {
                filled
                    .parent_session_id
                    .clone_from(&incoming.parent_session_id);
            }
            if filled.origin.is_none() {
                filled.origin.clone_from(&incoming.origin);
            }
            if incoming.agent_preset.is_some() {
                filled.agent_preset.clone_from(&incoming.agent_preset);
            }
            summaries
                .iter()
                .map(|summary| {
                    if summary.session_id == incoming.session_id {
                        filled.clone()
                    } else {
                        summary.clone()
                    }
                })
                .collect()
        }
        ListMutation::Remove(session_id) => summaries
            .iter()
            .filter(|summary| summary.session_id != *session_id)
            .cloned()
            .collect(),
        ListMutation::Status(session_id, running) => summaries
            .iter()
            .map(|summary| {
                if summary.session_id == *session_id
                    && (summary.running != *running || *running && summary.blank)
                {
                    let mut summary = summary.clone();
                    summary.running = *running;
                    summary.blank = summary.blank && !running;
                    summary
                } else {
                    summary.clone()
                }
            })
            .collect(),
        ListMutation::Activity(session_id, updated_at) => summaries
            .iter()
            .map(|summary| {
                if summary.session_id == *session_id && *updated_at > summary.updated_at {
                    let mut summary = summary.clone();
                    summary.updated_at = *updated_at;
                    summary
                } else {
                    summary.clone()
                }
            })
            .collect(),
        ListMutation::Engaged(session_id) => summaries
            .iter()
            .map(|summary| {
                if summary.session_id == *session_id && summary.blank {
                    let mut summary = summary.clone();
                    summary.blank = false;
                    summary
                } else {
                    summary.clone()
                }
            })
            .collect(),
    }
}

fn sync_completed(state: &mut ManagerState) {
    let seen = state
        .summaries
        .iter()
        .map(|summary| summary.session_id.clone())
        .collect::<IndexSet<_>>();
    for summary in &state.summaries {
        match state.previous_running.get(&summary.session_id).copied() {
            None => {
                state
                    .previous_running
                    .insert(summary.session_id.clone(), summary.running);
            }
            Some(true) if !summary.running => {
                if state.selected.as_ref() != Some(&summary.session_id) {
                    state.completed.insert(summary.session_id.clone());
                }
                state
                    .previous_running
                    .insert(summary.session_id.clone(), false);
            }
            Some(_) if summary.running => {
                state.completed.shift_remove(&summary.session_id);
                state
                    .previous_running
                    .insert(summary.session_id.clone(), true);
            }
            Some(true | false) => {}
        }
    }
    state.previous_running.retain(|id, _| seen.contains(id));
    state.completed.retain(|id| seen.contains(id));
}

fn buffered_key(rpc_id: &RpcId, frame: &SessionMuxFrame) -> Option<String> {
    match frame {
        SessionMuxFrame::ApprovalRequested { payload } => {
            let id = payload
                .get("approvalId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Some(format!("a:{id}"))
        }
        SessionMuxFrame::ApprovalResolved { approval_id } => Some(format!("a:{approval_id}")),
        SessionMuxFrame::QuestionRequested { .. } => Some(format!("q:{rpc_id}")),
        SessionMuxFrame::QuestionResolved { question_rpc_id } => {
            Some(format!("q:{question_rpc_id}"))
        }
        SessionMuxFrame::Queue(_) => Some("queue".to_owned()),
        SessionMuxFrame::Event(_)
        | SessionMuxFrame::Subscribed { .. }
        | SessionMuxFrame::Unknown => None,
    }
}

fn question_status(payload: &Value) -> PendingInteractionStatus {
    let Some(questions) = payload.get("questions").and_then(Value::as_array) else {
        return PendingInteractionStatus::Question;
    };
    let [question] = questions.as_slice() else {
        return PendingInteractionStatus::Question;
    };
    let Some(intent) = question.get("intent") else {
        return PendingInteractionStatus::Question;
    };
    if intent.get("kind").and_then(Value::as_str) != Some("plan-review")
        || question.get("detail").is_none()
        || question.get("multiSelect").and_then(Value::as_bool) == Some(true)
    {
        return PendingInteractionStatus::Question;
    }
    let options = question
        .get("options")
        .and_then(Value::as_array)
        .map_or(&[] as &[Value], Vec::as_slice);
    if options.len() > 2 {
        return PendingInteractionStatus::Question;
    }
    let approve = intent.get("approve").and_then(Value::as_str);
    if options
        .iter()
        .any(|option| option.get("label").and_then(Value::as_str) == approve)
    {
        PendingInteractionStatus::PlanReview
    } else {
        PendingInteractionStatus::Question
    }
}

fn status_name(status: PendingInteractionStatus) -> &'static str {
    match status {
        PendingInteractionStatus::Approval => "approval",
        PendingInteractionStatus::PlanReview => "plan-review",
        PendingInteractionStatus::Question => "question",
    }
}

fn internal_error(message: impl Into<String>) -> ClientRpcError {
    ClientRpcError {
        code: "internal".to_owned(),
        message: message.into(),
        details: Map::new(),
    }
}
