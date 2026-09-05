//! Resident Client Session window, transport lifecycle, and observable snapshot owner.

use std::{cell::RefCell, rc::Rc};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures::{FutureExt, future::LocalBoxFuture};
use indexmap::IndexMap;
use seekdeep_identity::{MessageId, RpcId, SessionId};
use serde_json::{Map, Value, json};

use crate::{
    AssemblerEventDefinitions, AssemblerViewDefinitions, ConversationEventInput,
    ConversationNodeAssembler, ConversationPublication, Notifier, NotifierScheduler,
    PendingClientResponse, PendingKind, PendingResponder, PendingWait, ProjectionValueStore,
    ProjectionsBaseline, QueueItemInput, RuntimeDisposer, SessionQueueMirror,
};

/// Messages requested per history page.
pub const PAGE_MESSAGES: u64 = 50;

/// Stable Client-facing business or transport error.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientRpcError {
    /// Stable error code.
    pub code: String,
    /// Human-readable message.
    pub message: String,
    /// Code-specific details.
    pub details: Map<String, Value>,
}

/// Business success/failure result with optional success value.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientRpcResult<T> {
    /// Successful operation; value may be omitted.
    Success(Option<T>),
    /// Business or folded transport failure.
    Failure(ClientRpcError),
}

impl<T> ClientRpcResult<T> {
    /// Whether this is the success branch.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Success(_))
    }
}

/// One history row and its optional wire presentation view.
#[derive(Clone)]
pub struct SessionHistoryEntry {
    /// Exact durable event.
    pub event: Rc<crate::ConversationLocationEvent>,
    /// Optional envelope-level presentation view.
    pub view: Option<Rc<Value>>,
}

impl SessionHistoryEntry {
    fn conversation_input(&self) -> ConversationEventInput {
        ConversationEventInput {
            event: self.event.clone(),
            view: self.view.clone(),
        }
    }
}

/// One history response page.
pub struct SessionHistoryPage {
    /// Message-aligned rows in ascending sequence order.
    pub entries: Vec<SessionHistoryEntry>,
    /// Whether an older page remains.
    pub has_more: bool,
    /// Optional Host-computed projection baseline.
    pub projections: Option<ProjectionsBaseline<Value>>,
}

/// Addressed or ordinary history request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionHistoryRequest {
    /// Ordinary Session identity.
    pub session_id: SessionId,
    /// Catalog address for non-activating child history.
    pub address: Option<SubagentAddress>,
    /// Exclusive older-page boundary.
    pub before_seq: Option<u64>,
    /// Requested message count.
    pub max_messages: Option<u64>,
}

/// Generic generated Client call.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionTransportRequest {
    /// Generated namespace and method.
    pub method: String,
    /// Exact JSON-compatible request payload.
    pub payload: Value,
}

/// Injected Session transport boundary.
pub trait SessionTransport {
    /// Reads ordinary or addressed history.
    fn history(
        &self,
        request: SessionHistoryRequest,
    ) -> LocalBoxFuture<'static, Result<ClientRpcResult<SessionHistoryPage>, String>>;

    /// Executes one generated unary method.
    fn call(
        &self,
        request: SessionTransportRequest,
    ) -> LocalBoxFuture<'static, Result<ClientRpcResult<Value>, String>>;
}

/// Injected owner for detached Session lifecycle work.
pub trait SessionTaskSpawner {
    /// Owns one local task until deterministic completion or teardown.
    fn spawn(&self, task: LocalBoxFuture<'static, ()>);
}

/// Catalog-discovered child activation mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentMode {
    /// Read-only one-shot child.
    OneShot,
    /// Continuable child.
    Continuable,
}

/// Direct parent/child address for non-activating subagent transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentAddress {
    /// Direct parent Session.
    pub parent_session_id: SessionId,
    /// Direct child Session.
    pub child_session_id: SessionId,
    /// Activation mode.
    pub mode: SubagentMode,
}

/// History-window lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionOpenState {
    /// Never opened or reset for resync.
    Cold,
    /// Tail request in flight.
    Loading,
    /// Contiguous window installed.
    Open,
    /// Tail request failed.
    Error,
}

/// Input-area shape derived by the Session snapshot owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposerPhase {
    /// Authoritative blank Session with no prompt attempt.
    Blank,
    /// First prompt attempted but no authoritative activity yet.
    Engaging,
    /// Ordinary active conversation.
    Active,
}

/// Operation whose failure occupies the input error strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptOperation {
    /// Prompt admission.
    Send,
    /// Turn interruption.
    Stop,
}

/// Input operation failure.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionPromptError {
    /// Failed operation.
    pub operation: PromptOperation,
    /// Stable business or transport error.
    pub error: ClientRpcError,
}

/// Decoded authenticated image attachment response.
pub struct SessionAttachmentRead {
    /// Durable attachment reference.
    pub attachment: Value,
    /// Decoded bytes.
    pub data: Vec<u8>,
}

/// Current addressed-child transport fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSubagentState {
    /// Catalog address.
    pub address: SubagentAddress,
    /// Latest exact-parent availability hint.
    pub parent_available: bool,
}

/// Reference-stable observable Session snapshot.
#[allow(clippy::struct_excessive_bools)] // Mirrors the source's public independent facts.
pub struct SessionSnapshot {
    /// Session identity.
    pub session_id: SessionId,
    /// Current Chat target snapshot, when registered.
    pub chat: Option<Rc<Value>>,
    /// Stable pending interaction array.
    pub pending: Rc<Vec<Rc<PendingWait>>>,
    /// Authoritative transient queue snapshot.
    pub queue: Rc<Vec<crate::QueuedMessage>>,
    /// Host running bit.
    pub running: bool,
    /// Addressed-child transport fact.
    pub subagent: Option<SessionSubagentState>,
    /// Derived composer phase.
    pub composer_phase: ComposerPhase,
    /// Host removed flag.
    pub removed: bool,
    /// History lifecycle.
    pub open_state: SessionOpenState,
    /// History failure.
    pub open_error: Option<ClientRpcError>,
    /// Older-history availability.
    pub has_more: bool,
    /// Older-page request in flight.
    pub loading_older: bool,
    /// Latest prompt or cancel failure.
    pub prompt_error: Option<SessionPromptError>,
    /// Authoritative empty-log mirror.
    pub blank: bool,
    /// Live unpositioned Agent failure.
    pub last_agent_error: Option<String>,
}

/// Mux frame routed to one Session.
#[derive(Clone)]
pub enum SessionMuxFrame {
    /// Durable event append.
    Event(SessionHistoryEntry),
    /// Authoritative transient queue snapshot.
    Queue(Vec<QueueItemInput>),
    /// New mux-generation durable baseline.
    Subscribed {
        /// Host's current durable tail.
        last_seq: u64,
    },
    /// Tool approval request.
    ApprovalRequested {
        /// Domain fields with envelope members stripped.
        payload: Value,
    },
    /// Tool approval settlement.
    ApprovalResolved {
        /// Approval identity from the request payload.
        approval_id: String,
    },
    /// Structured user question.
    QuestionRequested {
        /// Domain fields with envelope members stripped.
        payload: Value,
    },
    /// Structured question settlement.
    QuestionResolved {
        /// Original question request correlation identity.
        question_rpc_id: RpcId,
    },
    /// Merge-extensible unknown frame.
    Unknown,
}

struct EmptyEvents;

impl AssemblerEventDefinitions for EmptyEvents {
    fn entries(&self) -> Vec<Rc<crate::AssemblerNodeDefinition>> {
        Vec::new()
    }

    fn fallback_entry(&self) -> Option<Rc<crate::AssemblerNodeDefinition>> {
        None
    }
}

struct EmptyViews;

impl AssemblerViewDefinitions for EmptyViews {
    fn entries(&self) -> Vec<Rc<crate::AssemblerViewDefinition>> {
        Vec::new()
    }
}

/// Session construction seams and manager-owned state.
pub struct SessionOptions {
    /// Catalog address selecting child transport.
    pub address: Option<SubagentAddress>,
    /// Latest exact-parent availability.
    pub parent_available: bool,
    /// Manager-owned projection store.
    pub projections: Option<Rc<ProjectionValueStore<Value>>>,
    /// Session-owned Conversation assembler.
    pub conversation: Option<ConversationNodeAssembler>,
    /// Notification scheduler.
    pub scheduler: Rc<dyn NotifierScheduler>,
    /// Detached task owner.
    pub spawner: Rc<dyn SessionTaskSpawner>,
    /// Browser time-zone resolver.
    pub resolve_time_zone: Rc<dyn Fn() -> Result<String, String>>,
    /// First accepted prompt observer.
    pub on_engaged: Option<Rc<dyn Fn(SessionId)>>,
    /// Contained diagnostic sink.
    pub report: Rc<dyn Fn(String)>,
}

#[allow(clippy::struct_excessive_bools)] // One owner keeps the lifecycle transitions atomic.
struct SessionState {
    events: Vec<Rc<crate::ConversationLocationEvent>>,
    views: Vec<Option<Rc<Value>>>,
    base_seq: u64,
    has_more: bool,
    open_state: SessionOpenState,
    open_error: Option<ClientRpcError>,
    open_generation: u64,
    next_open_token: u64,
    open_task: Option<(u64, futures::future::Shared<LocalBoxFuture<'static, ()>>)>,
    loading_older: bool,
    pending: IndexMap<String, Rc<PendingWait>>,
    pending_revision: u64,
    pending_cache: Option<(u64, Rc<Vec<Rc<PendingWait>>>)>,
    running: bool,
    address: Option<SubagentAddress>,
    parent_available: bool,
    prompt_attempted: bool,
    first_prompt_pending_turn: bool,
    blank: bool,
    removed: bool,
    prompt_error: Option<SessionPromptError>,
    last_agent_error: Option<String>,
    live_buffer: Vec<SessionHistoryEntry>,
    stitching: bool,
    subscribed_last_seq: Option<u64>,
    scope_bound: bool,
    snapshot: Rc<SessionSnapshot>,
}

/// Resident Session object: window owner, frame target, and observable source.
pub struct ClientSession {
    session_id: SessionId,
    transport: Rc<dyn SessionTransport>,
    options: SessionOptions,
    state: RefCell<SessionState>,
    queue: RefCell<SessionQueueMirror>,
    conversation: RefCell<ConversationNodeAssembler>,
    projections: Rc<ProjectionValueStore<Value>>,
    notifier: Rc<Notifier>,
}

impl ClientSession {
    /// Creates one resident Session with injected scheduling and transport boundaries.
    #[must_use]
    pub fn new(
        session_id: SessionId,
        transport: Rc<dyn SessionTransport>,
        mut options: SessionOptions,
    ) -> Rc<Self> {
        let projections = options
            .projections
            .take()
            .unwrap_or_else(|| Rc::new(ProjectionValueStore::new(options.scheduler.clone())));
        let conversation = options.conversation.take().unwrap_or_else(|| {
            ConversationNodeAssembler::new(Rc::new(EmptyEvents), Rc::new(EmptyViews))
        });
        let initial = Rc::new(SessionSnapshot {
            session_id: session_id.clone(),
            chat: conversation.snapshot("chat"),
            pending: Rc::new(Vec::new()),
            queue: Rc::new(Vec::new()),
            running: false,
            subagent: options.address.clone().map(|address| SessionSubagentState {
                address,
                parent_available: options.parent_available,
            }),
            composer_phase: ComposerPhase::Blank,
            removed: false,
            open_state: SessionOpenState::Cold,
            open_error: None,
            has_more: false,
            loading_older: false,
            prompt_error: None,
            blank: true,
            last_agent_error: None,
        });
        Rc::new_cyclic(move |weak: &std::rc::Weak<Self>| {
            let rebuild = {
                let weak = weak.clone();
                Rc::new(move || {
                    if let Some(session) = weak.upgrade() {
                        session.rebuild_snapshot();
                    }
                })
            };
            let notifier = Notifier::new(rebuild, options.scheduler.clone());
            Self {
                session_id,
                transport,
                state: RefCell::new(SessionState {
                    events: Vec::new(),
                    views: Vec::new(),
                    base_seq: 0,
                    has_more: false,
                    open_state: SessionOpenState::Cold,
                    open_error: None,
                    open_generation: 0,
                    next_open_token: 0,
                    open_task: None,
                    loading_older: false,
                    pending: IndexMap::new(),
                    pending_revision: 0,
                    pending_cache: None,
                    running: false,
                    address: options.address.clone(),
                    parent_available: options.parent_available,
                    prompt_attempted: false,
                    first_prompt_pending_turn: false,
                    blank: true,
                    removed: false,
                    prompt_error: None,
                    last_agent_error: None,
                    live_buffer: Vec::new(),
                    stitching: false,
                    subscribed_last_seq: None,
                    scope_bound: false,
                    snapshot: initial,
                }),
                queue: RefCell::new(SessionQueueMirror::default()),
                conversation: RefCell::new(conversation),
                projections,
                notifier,
                options,
            }
        })
    }

    /// Host Session identity.
    #[must_use]
    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Host-computed projection values.
    #[must_use]
    pub fn projections(&self) -> Rc<ProjectionValueStore<Value>> {
        self.projections.clone()
    }

    /// Subscribes to committed Session snapshot changes.
    #[must_use]
    pub fn subscribe(&self, listener: Rc<dyn Fn()>) -> RuntimeDisposer {
        self.notifier.subscribe(listener)
    }

    /// Returns the cached reference, rebuilding lazily when dirty.
    #[must_use]
    pub fn snapshot(&self) -> Rc<SessionSnapshot> {
        self.notifier.ensure_fresh();
        self.state.borrow().snapshot.clone()
    }

    /// Reads one registered Conversation target snapshot.
    #[must_use]
    pub fn conversation_snapshot(&self, target: &str) -> Option<Rc<Value>> {
        self.conversation.borrow().snapshot(target)
    }

    /// Binds the single Agent-scoped Client context marker.
    ///
    /// # Errors
    ///
    /// Returns the source diagnostic on a second bind.
    pub fn bind_scope(&self) -> Result<(), String> {
        let mut state = self.state.borrow_mut();
        if state.scope_bound {
            return Err(format!(
                "session {} already has a bound scope",
                self.session_id
            ));
        }
        state.scope_bound = true;
        Ok(())
    }

    /// Releases the scoped marker so a newly minted scope may bind later.
    pub fn unbind_scope(&self) {
        self.state.borrow_mut().scope_bound = false;
    }

    /// First open; concurrent callers share one underlying operation.
    pub fn open(self: &Rc<Self>) -> futures::future::Shared<LocalBoxFuture<'static, ()>> {
        {
            let state = self.state.borrow();
            if state.open_state == SessionOpenState::Open {
                return futures::future::ready(()).boxed_local().shared();
            }
            if let Some((_, task)) = &state.open_task {
                return task.clone();
            }
        }
        let (generation, token) = {
            let mut state = self.state.borrow_mut();
            state.next_open_token = state.next_open_token.wrapping_add(1);
            state.open_state = SessionOpenState::Loading;
            state.open_error = None;
            (state.open_generation, state.next_open_token)
        };
        self.notifier.mark_dirty();
        let weak = Rc::downgrade(self);
        let task = async move {
            if let Some(session) = weak.upgrade() {
                session.do_open(generation).await;
                let mut state = session.state.borrow_mut();
                if state
                    .open_task
                    .as_ref()
                    .is_some_and(|(current, _)| *current == token)
                {
                    state.open_task = None;
                }
            }
        }
        .boxed_local()
        .shared();
        self.state.borrow_mut().open_task = Some((token, task.clone()));
        task
    }

    /// Reconnect rebuild; cold Sessions remain untouched.
    ///
    /// Generation invalidation and window reset occur synchronously.
    #[must_use]
    pub fn resync(self: &Rc<Self>) -> LocalBoxFuture<'static, ()> {
        {
            let mut state = self.state.borrow_mut();
            if state.open_state == SessionOpenState::Cold {
                return futures::future::ready(()).boxed_local();
            }
            state.open_generation = state.open_generation.wrapping_add(1);
            state.open_task = None;
            state.open_state = SessionOpenState::Cold;
            state.open_error = None;
            state.events.clear();
            state.views.clear();
            state.base_seq = 0;
            state.pending.clear();
            state.pending_revision = state.pending_revision.wrapping_add(1);
            state.subscribed_last_seq = None;
            state.live_buffer.clear();
        }
        self.notifier.mark_dirty();
        let open = self.open();
        open.boxed_local()
    }

    /// Sends one ordinary or addressed prompt and mirrors admission failures.
    ///
    /// The blank-to-engaging edge occurs synchronously before this returns the future.
    #[must_use]
    pub fn prompt(
        self: &Rc<Self>,
        content: Vec<Value>,
        mode: impl Into<String>,
    ) -> LocalBoxFuture<'static, ClientRpcResult<Value>> {
        {
            let mut state = self.state.borrow_mut();
            state.prompt_error = None;
            state.last_agent_error = None;
            state.prompt_attempted = true;
            if state.blank {
                state.first_prompt_pending_turn = true;
            }
        }
        self.notifier.mark_dirty();
        let session = self.clone();
        let mode = mode.into();
        async move { session.prompt_after_entry(content, &mode).await }.boxed_local()
    }

    async fn prompt_after_entry(&self, content: Vec<Value>, mode: &str) -> ClientRpcResult<Value> {
        let address = self.state.borrow().address.clone();
        let result = match address {
            None => {
                let zone = match (self.options.resolve_time_zone)() {
                    Ok(zone) => zone,
                    Err(error) => {
                        return self.finish_prompt(ClientRpcResult::Failure(internal_error(error)));
                    }
                };
                self.call_folded(
                    "session.prompt",
                    json!({
                        "sessionId":self.session_id.as_str(),
                        "mode":mode,
                        "content":content,
                        "clientTimeZone":zone
                    }),
                )
                .await
            }
            Some(address) if address.mode == SubagentMode::OneShot => {
                ClientRpcResult::Failure(ClientRpcError {
                    code: "subagent-not-resumable".to_owned(),
                    message: "one-shot subagent conversations are read-only".to_owned(),
                    details: Map::from_iter([(
                        "childSessionId".to_owned(),
                        Value::String(address.child_session_id.as_str().to_owned()),
                    )]),
                })
            }
            Some(address)
                if content
                    .iter()
                    .any(|part| part.get("type").and_then(Value::as_str) == Some("image")) =>
            {
                ClientRpcResult::Failure(ClientRpcError {
                    code: "attachment-error".to_owned(),
                    message: "Image input is unavailable for subagent continuations.".to_owned(),
                    details: Map::from_iter([(
                        "reason".to_owned(),
                        Value::String("SUBAGENT_IMAGE_UNSUPPORTED".to_owned()),
                    )]),
                })
            }
            Some(address) => {
                let zone = match (self.options.resolve_time_zone)() {
                    Ok(zone) => zone,
                    Err(error) => {
                        return self.finish_prompt(ClientRpcResult::Failure(internal_error(error)));
                    }
                };
                match self
                    .call_folded(
                        "subagent.prompt",
                        json!({
                            "parentSessionId":address.parent_session_id.as_str(),
                            "childSessionId":address.child_session_id.as_str(),
                            "mode":"continuable",
                            "content":content.into_iter().filter(|part| {
                                part.get("type").and_then(Value::as_str)==Some("text")
                            }).collect::<Vec<_>>(),
                            "clientTimeZone":zone
                        }),
                    )
                    .await
                {
                    ClientRpcResult::Success(_) => {
                        ClientRpcResult::Success(Some(json!({"accepted":true})))
                    }
                    ClientRpcResult::Failure(error) => ClientRpcResult::Failure(error),
                }
            }
        };
        self.finish_prompt(result)
    }

    /// Cancels one ordinary or continuable child Turn.
    pub async fn cancel(&self) -> ClientRpcResult<Value> {
        let address = self.state.borrow().address.clone();
        let result = match address {
            Some(address) if address.mode == SubagentMode::OneShot => {
                ClientRpcResult::Failure(ClientRpcError {
                    code: "subagent-delivery-unavailable".to_owned(),
                    message: "subagent activation cancellation is unavailable".to_owned(),
                    details: Map::from_iter([(
                        "childSessionId".to_owned(),
                        Value::String(address.child_session_id.as_str().to_owned()),
                    )]),
                })
            }
            Some(address) => {
                self.call_folded(
                    "subagent.interrupt",
                    json!({
                        "parentSessionId":address.parent_session_id.as_str(),
                        "childSessionId":address.child_session_id.as_str(),
                        "mode":"continuable"
                    }),
                )
                .await
            }
            None => {
                self.call_folded(
                    "session.cancel",
                    json!({"sessionId":self.session_id.as_str()}),
                )
                .await
            }
        };
        if let ClientRpcResult::Failure(error) = &result {
            self.state.borrow_mut().prompt_error = Some(SessionPromptError {
                operation: PromptOperation::Stop,
                error: error.clone(),
            });
            self.notifier.mark_dirty();
        }
        result
    }

    /// Resolves one authenticated image attachment and decodes its bytes.
    pub async fn read_attachment(
        &self,
        attachment_id: &str,
    ) -> ClientRpcResult<SessionAttachmentRead> {
        match self
            .call_folded(
                "session.attachment",
                json!({
                    "sessionId":self.session_id.as_str(),
                    "attachmentId":attachment_id
                }),
            )
            .await
        {
            ClientRpcResult::Success(Some(value)) => {
                let attachment = value.get("attachment").cloned().unwrap_or(Value::Null);
                let Some(encoded) = value.get("data").and_then(Value::as_str) else {
                    return ClientRpcResult::Failure(internal_error(
                        "attachment response omitted base64 data",
                    ));
                };
                match STANDARD.decode(encoded) {
                    Ok(data) => {
                        ClientRpcResult::Success(Some(SessionAttachmentRead { attachment, data }))
                    }
                    Err(error) => ClientRpcResult::Failure(internal_error(error.to_string())),
                }
            }
            ClientRpcResult::Success(None) => {
                ClientRpcResult::Failure(internal_error("attachment response omitted value"))
            }
            ClientRpcResult::Failure(error) => ClientRpcResult::Failure(error),
        }
    }

    /// Applies one operation to a still-pending queue occurrence without local optimism.
    pub async fn update_queue(&self, item_id: &MessageId, action: Value) -> ClientRpcResult<Value> {
        self.call_folded(
            "session.updateQueue",
            json!({
                "sessionId":self.session_id.as_str(),
                "itemId":item_id.as_str(),
                "action":action
            }),
        )
        .await
    }

    /// Renames the Session and settles the title projection from the unary response.
    pub async fn rename(&self, title: &str) -> ClientRpcResult<Value> {
        let result = self
            .call_folded(
                "session.rename",
                json!({"sessionId":self.session_id.as_str(),"title":title}),
            )
            .await;
        if let ClientRpcResult::Success(Some(value)) = &result
            && let (Some(title), Some(seq)) = (
                value.get("title").and_then(Value::as_str),
                value.get("seq").and_then(Value::as_i64),
            )
        {
            self.projections
                .apply("title", Rc::new(Value::String(title.to_owned())), seq);
        }
        result
    }

    /// Executes one slash command and reports only whether a Host command matched.
    pub async fn command(&self, line: &str) -> ClientRpcResult<Value> {
        match self
            .call_folded(
                "commands.execute",
                json!({"sessionId":self.session_id.as_str(),"line":line}),
            )
            .await
        {
            ClientRpcResult::Success(value) => ClientRpcResult::Success(Some(json!({
                "matched":value.is_some()
            }))),
            ClientRpcResult::Failure(error) => ClientRpcResult::Failure(error),
        }
    }

    /// Installs or clears the addressed-child transport fact.
    pub fn configure_subagent(
        self: &Rc<Self>,
        address: Option<SubagentAddress>,
        parent_available: bool,
    ) {
        let same = self.state.borrow().address == address;
        {
            let mut state = self.state.borrow_mut();
            state.address = address;
            state.parent_available = parent_available;
        }
        if !same && self.state.borrow().open_state != SessionOpenState::Cold {
            let session = self.clone();
            self.options
                .spawner
                .spawn(async move { session.resync().await }.boxed_local());
        } else {
            self.notifier.mark_dirty();
        }
    }

    /// Updates only the direct-parent availability hint.
    pub fn handle_subagent_parent_available(&self, available: bool) {
        let changed = {
            let mut state = self.state.borrow_mut();
            if state.parent_available == available {
                false
            } else {
                state.parent_available = available;
                true
            }
        };
        if changed {
            self.notifier.mark_dirty();
        }
    }

    /// Reserved no-op: Session instances remain resident.
    pub fn dispose(&self) {}

    /// Loads one immediately preceding history page when eligible.
    pub async fn load_older(self: &Rc<Self>) {
        let request = {
            let mut state = self.state.borrow_mut();
            if state.open_state != SessionOpenState::Open || !state.has_more || state.loading_older
            {
                return;
            }
            state.loading_older = true;
            SessionHistoryRequest {
                session_id: self.session_id.clone(),
                address: state.address.clone(),
                before_seq: Some(state.base_seq),
                max_messages: Some(PAGE_MESSAGES),
            }
        };
        self.notifier.mark_dirty();
        let response = self.transport.history(request).await;
        match response {
            Ok(ClientRpcResult::Success(Some(page))) => {
                if page.entries.is_empty() {
                    self.state.borrow_mut().has_more = page.has_more;
                    if let Err(error) = self.conversation.borrow_mut().prepend(&[], page.has_more) {
                        (self.options.report)(error.to_string());
                    }
                } else {
                    let base_seq = self.state.borrow().base_seq;
                    let tail = page.entries.last().map(|entry| entry.event.seq);
                    if tail.and_then(|tail| tail.checked_add(1)) == Some(base_seq) {
                        let inputs = page
                            .entries
                            .iter()
                            .map(SessionHistoryEntry::conversation_input)
                            .collect::<Vec<_>>();
                        {
                            let mut state = self.state.borrow_mut();
                            let mut events = page
                                .entries
                                .iter()
                                .map(|entry| entry.event.clone())
                                .collect::<Vec<_>>();
                            events.append(&mut state.events);
                            state.events = events;
                            let mut views = page
                                .entries
                                .iter()
                                .map(|entry| entry.view.clone())
                                .collect::<Vec<_>>();
                            views.append(&mut state.views);
                            state.views = views;
                            state.base_seq = page.entries[0].event.seq;
                            state.has_more = page.has_more;
                        }
                        if let Err(error) = self
                            .conversation
                            .borrow_mut()
                            .prepend(&inputs, page.has_more)
                        {
                            (self.options.report)(error.to_string());
                        }
                    } else {
                        (self.options.report)(format!(
                            "[web-runtime] history page discontinuous: tail seq {tail:?} vs baseSeq {base_seq}"
                        ));
                        self.state.borrow_mut().has_more = false;
                        let _ = self.conversation.borrow_mut().prepend(&[], false);
                    }
                }
            }
            Ok(ClientRpcResult::Success(None) | ClientRpcResult::Failure(_)) => {}
            Err(error) => (self.options.report)(format!("[web-runtime] loadOlder failed: {error}")),
        }
        self.state.borrow_mut().loading_older = false;
        self.notifier.mark_dirty();
    }

    /// Routes one mux envelope into the Session state machine.
    pub fn handle_mux_envelope(self: &Rc<Self>, rpc_id: RpcId, frame: SessionMuxFrame) {
        match frame {
            SessionMuxFrame::Event(entry) => self.accept_live_event(entry),
            SessionMuxFrame::Queue(items) => {
                self.queue.borrow_mut().replace(&items);
                self.notifier.mark_dirty();
            }
            SessionMuxFrame::Subscribed { last_seq } => {
                self.state.borrow_mut().subscribed_last_seq = Some(last_seq);
                if self.queue.borrow_mut().reset() {
                    self.notifier.mark_dirty();
                }
            }
            SessionMuxFrame::ApprovalRequested { payload } => {
                self.mint_wait(PendingKind::Approval, rpc_id, payload);
                self.notifier.mark_dirty();
            }
            SessionMuxFrame::ApprovalResolved { approval_id } => {
                let waits = self
                    .state
                    .borrow()
                    .pending
                    .values()
                    .filter(|wait| {
                        wait.kind == PendingKind::Approval
                            && wait.payload.get("approvalId").and_then(Value::as_str)
                                == Some(approval_id.as_str())
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                for wait in waits {
                    self.settle_wait(&wait);
                }
                self.notifier.mark_dirty();
            }
            SessionMuxFrame::QuestionRequested { payload } => {
                self.mint_wait(PendingKind::Question, rpc_id, payload);
                self.notifier.mark_dirty();
            }
            SessionMuxFrame::QuestionResolved { question_rpc_id } => {
                let key = format!("q:{question_rpc_id}");
                let wait = self.state.borrow().pending.get(&key).cloned();
                if let Some(wait) = wait {
                    self.settle_wait(&wait);
                }
                self.notifier.mark_dirty();
            }
            SessionMuxFrame::Unknown => {}
        }
    }

    /// Relays the Host running bit and authoritative first-Turn engagement.
    pub fn handle_running(&self, running: bool) {
        let mut dirty = false;
        {
            let mut state = self.state.borrow_mut();
            if running && state.blank {
                state.blank = false;
                dirty = true;
            }
            if running {
                state.first_prompt_pending_turn = false;
            }
            if state.running != running {
                state.running = running;
                dirty = true;
            }
        }
        if dirty {
            self.notifier.mark_dirty();
        }
    }

    /// Monotonically reconciles the authoritative summary blank bit.
    pub fn handle_blank(&self, blank: bool) {
        let changed = {
            let mut state = self.state.borrow_mut();
            if blank == state.blank || blank && (state.prompt_attempted || state.running) {
                false
            } else {
                state.blank = blank;
                true
            }
        };
        if changed {
            self.notifier.mark_dirty();
        }
    }

    /// Flags a Host-removed resident Session.
    pub fn handle_removed(&self) {
        self.state.borrow_mut().removed = true;
        self.notifier.mark_dirty();
    }

    /// Publishes one unpositioned live Agent error.
    pub fn handle_agent_error(&self, message: impl Into<String>) {
        self.state.borrow_mut().last_agent_error = Some(message.into());
        self.notifier.mark_dirty();
    }

    /// Rebuilds the current window after a low-frequency Registry change.
    pub fn rebuild_conversation_registry(&self) {
        match self.conversation.borrow_mut().rebuild_registry() {
            Ok(publication) => self.schedule_conversation(publication),
            Err(error) => (self.options.report)(error.to_string()),
        }
    }

    fn mint_wait(self: &Rc<Self>, kind: PendingKind, rpc_id: RpcId, payload: Value) {
        let weak = Rc::downgrade(self);
        let responder: PendingResponder = Rc::new(move |response: PendingClientResponse| {
            let weak = weak.clone();
            async move {
                let Some(session) = weak.upgrade() else {
                    return Err("Session response owner was disposed".to_owned());
                };
                match session
                    .transport
                    .call(SessionTransportRequest {
                        method: "respond".to_owned(),
                        payload: json!({
                            "type":"client-response",
                            "rpcId":response.rpc_id.as_str(),
                            "result":response.result
                        }),
                    })
                    .await?
                {
                    ClientRpcResult::Success(value) => Ok(value.unwrap_or(Value::Null)),
                    ClientRpcResult::Failure(error) => Err(error.message),
                }
            }
            .boxed_local()
        });
        let wait = Rc::new(PendingWait::new(
            kind,
            rpc_id,
            self.session_id.clone(),
            payload,
            responder,
        ));
        let mut state = self.state.borrow_mut();
        state.pending.insert(wait.key.clone(), wait);
        state.pending_revision = state.pending_revision.wrapping_add(1);
    }

    fn settle_wait(&self, wait: &Rc<PendingWait>) {
        wait.mark_settled();
        let mut state = self.state.borrow_mut();
        state.pending.shift_remove(&wait.key);
        state.pending_revision = state.pending_revision.wrapping_add(1);
    }

    async fn do_open(self: &Rc<Self>, generation: u64) {
        let mut response = self
            .transport
            .history(self.history_request(None, Some(PAGE_MESSAGES)))
            .await;
        if generation != self.state.borrow().open_generation {
            return;
        }
        match response {
            Ok(ClientRpcResult::Failure(error)) => {
                let mut state = self.state.borrow_mut();
                state.open_state = SessionOpenState::Error;
                state.open_error = Some(error);
            }
            Err(error) => {
                let mut state = self.state.borrow_mut();
                state.open_state = SessionOpenState::Error;
                state.open_error = Some(internal_error(error));
            }
            Ok(ClientRpcResult::Success(None)) => {
                let mut state = self.state.borrow_mut();
                state.open_state = SessionOpenState::Error;
                state.open_error = Some(internal_error("history response omitted value"));
            }
            Ok(ClientRpcResult::Success(Some(page))) => {
                if let Err(error) = self.install_window(&page) {
                    let mut state = self.state.borrow_mut();
                    state.open_state = SessionOpenState::Error;
                    state.open_error = Some(internal_error(error));
                } else {
                    let (tail, subscribed) = {
                        let state = self.state.borrow();
                        (
                            state.events.last().map(|event| event.seq),
                            state.subscribed_last_seq,
                        )
                    };
                    if subscribed
                        .zip(tail)
                        .is_some_and(|(baseline, tail)| baseline > tail)
                    {
                        response = self
                            .transport
                            .history(self.history_request(None, Some(PAGE_MESSAGES)))
                            .await;
                        if generation != self.state.borrow().open_generation {
                            return;
                        }
                        if let Ok(ClientRpcResult::Success(Some(page))) = response {
                            let _ = self.install_window(&page);
                        }
                    }
                    self.state.borrow_mut().open_state = SessionOpenState::Open;
                }
            }
        }
        if generation == self.state.borrow().open_generation {
            self.notifier.mark_dirty();
        }
    }

    fn install_window(self: &Rc<Self>, page: &SessionHistoryPage) -> Result<(), String> {
        let inputs = page
            .entries
            .iter()
            .map(SessionHistoryEntry::conversation_input)
            .collect::<Vec<_>>();
        self.conversation
            .borrow_mut()
            .replace_window(&inputs, page.has_more)
            .map_err(|error| error.to_string())?;
        if let Some(projections) = &page.projections {
            self.projections.seed(projections);
        }
        let buffered = {
            let mut state = self.state.borrow_mut();
            state.events = page
                .entries
                .iter()
                .map(|entry| entry.event.clone())
                .collect();
            state.views = page
                .entries
                .iter()
                .map(|entry| entry.view.clone())
                .collect();
            state.base_seq = state.events.first().map_or(0, |event| event.seq);
            state.has_more = page.has_more;
            if state
                .events
                .iter()
                .any(|event| event.event_type == "turn/start")
            {
                state.first_prompt_pending_turn = false;
            }
            std::mem::take(&mut state.live_buffer)
        };
        for entry in buffered {
            let _ = self.append_live(&entry)?;
        }
        self.notifier.mark_dirty();
        Ok(())
    }

    fn append_live(&self, entry: &SessionHistoryEntry) -> Result<ConversationPublication, String> {
        let tail = self.state.borrow().events.last().map(|event| event.seq);
        if tail.is_some_and(|tail| entry.event.seq <= tail) {
            return Ok(ConversationPublication::None);
        }
        {
            let mut state = self.state.borrow_mut();
            state.events.push(entry.event.clone());
            state.views.push(entry.view.clone());
            if entry.event.event_type == "turn/start" {
                state.first_prompt_pending_turn = false;
            }
        }
        let queue_changed = if entry.event.event_type == "user/message" {
            entry
                .event
                .data
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| {
                    self.queue
                        .borrow_mut()
                        .accept_durable_user_message(&MessageId::new(id))
                })
        } else {
            false
        };
        let publication = self
            .conversation
            .borrow_mut()
            .append(&entry.conversation_input())
            .map_err(|error| error.to_string())?;
        Ok(if queue_changed {
            ConversationPublication::Immediate
        } else {
            publication
        })
    }

    fn accept_live_event(self: &Rc<Self>, entry: SessionHistoryEntry) {
        let mut repair = false;
        {
            let mut state = self.state.borrow_mut();
            if state.open_state == SessionOpenState::Loading || state.stitching {
                state.live_buffer.push(entry);
                return;
            }
            if state.open_state != SessionOpenState::Open {
                return;
            }
            let tail = state.events.last().map(|event| event.seq);
            if tail
                .and_then(|tail| tail.checked_add(1))
                .is_some_and(|next| entry.event.seq > next)
            {
                state.live_buffer.push(entry.clone());
                state.stitching = true;
                repair = true;
            }
        }
        if repair {
            let weak = Rc::downgrade(self);
            self.options.spawner.spawn(
                async move {
                    if let Some(session) = weak.upgrade() {
                        session.repair_gap().await;
                    }
                }
                .boxed_local(),
            );
            return;
        }
        match self.append_live(&entry) {
            Ok(publication) => self.schedule_conversation(publication),
            Err(error) => (self.options.report)(error),
        }
    }

    async fn repair_gap(self: &Rc<Self>) {
        let generation = self.state.borrow().open_generation;
        let response = self
            .transport
            .history(self.history_request(None, Some(PAGE_MESSAGES)))
            .await;
        if let Ok(ClientRpcResult::Success(Some(page))) = response
            && generation == self.state.borrow().open_generation
            && self.state.borrow().open_state == SessionOpenState::Open
        {
            let _ = self.install_window(&page);
        }
        self.state.borrow_mut().stitching = false;
    }

    fn schedule_conversation(&self, publication: ConversationPublication) {
        match publication {
            ConversationPublication::Immediate => self.notifier.mark_dirty(),
            ConversationPublication::AnimationFrame => self.notifier.mark_frame_dirty(),
            ConversationPublication::None => {}
        }
    }

    fn finish_prompt(&self, result: ClientRpcResult<Value>) -> ClientRpcResult<Value> {
        match &result {
            ClientRpcResult::Failure(error) => {
                self.state.borrow_mut().prompt_error = Some(SessionPromptError {
                    operation: PromptOperation::Send,
                    error: error.clone(),
                });
                self.notifier.mark_dirty();
            }
            ClientRpcResult::Success(_) => {
                let engaged = {
                    let mut state = self.state.borrow_mut();
                    if state.blank {
                        state.blank = false;
                        true
                    } else {
                        false
                    }
                };
                if engaged {
                    if let Some(on_engaged) = &self.options.on_engaged {
                        on_engaged(self.session_id.clone());
                    }
                    self.notifier.mark_dirty();
                }
            }
        }
        result
    }

    async fn call_folded(&self, method: &str, payload: Value) -> ClientRpcResult<Value> {
        match self
            .transport
            .call(SessionTransportRequest {
                method: method.to_owned(),
                payload,
            })
            .await
        {
            Ok(result) => result,
            Err(error) => ClientRpcResult::Failure(internal_error(error)),
        }
    }

    fn history_request(
        &self,
        before_seq: Option<u64>,
        max_messages: Option<u64>,
    ) -> SessionHistoryRequest {
        SessionHistoryRequest {
            session_id: self.session_id.clone(),
            address: self.state.borrow().address.clone(),
            before_seq,
            max_messages,
        }
    }

    fn rebuild_snapshot(&self) {
        if let Err(error) = self.conversation.borrow_mut().flush() {
            panic!("{error}");
        }
        let mut state = self.state.borrow_mut();
        let pending = if let Some((revision, pending)) = &state.pending_cache
            && *revision == state.pending_revision
        {
            pending.clone()
        } else {
            let pending = Rc::new(state.pending.values().cloned().collect::<Vec<_>>());
            state.pending_cache = Some((state.pending_revision, pending.clone()));
            pending
        };
        let chat = self.conversation.borrow().snapshot("chat");
        let has_content = chat
            .as_ref()
            .is_some_and(|chat| chat_has_visible_content(chat))
            || !state.blank && !state.first_prompt_pending_turn
            || state.running
            || !pending.is_empty();
        let composer_phase = if has_content {
            ComposerPhase::Active
        } else if state.prompt_attempted {
            ComposerPhase::Engaging
        } else {
            ComposerPhase::Blank
        };
        state.snapshot = Rc::new(SessionSnapshot {
            session_id: self.session_id.clone(),
            chat,
            pending,
            queue: self.queue.borrow().snapshot(),
            running: state.running,
            subagent: state.address.clone().map(|address| SessionSubagentState {
                address,
                parent_available: state.parent_available,
            }),
            composer_phase,
            removed: state.removed,
            open_state: state.open_state,
            open_error: state.open_error.clone(),
            has_more: state.has_more,
            loading_older: state.loading_older,
            prompt_error: state.prompt_error.clone(),
            blank: state.blank,
            last_agent_error: state.last_agent_error.clone(),
        });
    }
}

fn internal_error(message: impl Into<String>) -> ClientRpcError {
    ClientRpcError {
        code: "internal".to_owned(),
        message: message.into(),
        details: Map::new(),
    }
}

fn chat_has_visible_content(chat: &Value) -> bool {
    chat.get("order")
        .and_then(Value::as_array)
        .is_some_and(|order| {
            order.iter().filter_map(Value::as_str).any(|key| {
                chat.get("nodes")
                    .and_then(|nodes| nodes.get(key))
                    .and_then(|node| node.get("kind"))
                    .and_then(Value::as_str)
                    != Some("command")
            })
        })
}
