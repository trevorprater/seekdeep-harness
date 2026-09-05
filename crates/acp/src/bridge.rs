//! Connection-owned ACP session bridge over live Harness agents.

use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use futures::future::{BoxFuture, join_all};
use parking_lot::Mutex;
use seekdeep_agent::{
    AGENTS, AgentCancelCause, AgentEvent, AgentHandle, AgentOptions, AgentRegistry, CancelOptions,
    CreateAgentOptions, ModelSelection, ModelSelectionRef, install_model_selection,
};
use seekdeep_agent_loop::{AgentErrorEvent, AgentInboxClaimed};
use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply, fiber::EffectHandle};
use seekdeep_core::session::{Session, SessionEvent, SessionId};
use seekdeep_llm::{MessageSource, ModelId, ProviderId, UserMessage};
use seekdeep_sdk_protocol::{JsonRpcLineTransport, JsonRpcResponseError};
use seekdeep_subagent::SUBAGENTS;
use seekdeep_system_prompt::SYSTEM_PROMPT;
use seekdeep_user_approval::{
    ApprovalAnswer, ApprovalOutcome, ApprovalRequest, register_approval_answerer,
};
use serde_json::{Map, Value, json};
use tokio::{
    sync::{OnceCell, mpsc, oneshot},
    task::JoinHandle,
};

use crate::{
    codec::{acp_prompt_to_text, prompt_has_unsupported_content, turn_end_to_stop_reason},
    types::{AcpSessionId, AcpStopReason, PROTOCOL_VERSION, agent_methods, client_methods},
};

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

/// Deterministic test seam for the connection-scoped continuable-descendant drain.
#[doc(hidden)]
pub type AcpContinuableDrainHook = Arc<
    dyn Fn(Vec<Arc<seekdeep_agent::Agent>>) -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync,
>;

/// Per-created-agent provider and model selection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AcpBridgeConfig {
    /// Provider route.
    pub provider: Option<String>,
    /// Model id.
    pub model: Option<String>,
}

struct InflightPrompt {
    sender: Option<oneshot::Sender<anyhow::Result<AcpStopReason>>>,
    message_id: String,
    turn: Option<u64>,
    end_kind: Option<String>,
}

struct SessionRecord {
    handle: Arc<AgentHandle>,
    inflight: Option<InflightPrompt>,
}

enum NotificationCommand {
    Send(Map<String, Value>),
    Flush(oneshot::Sender<()>),
    Shutdown(oneshot::Sender<()>),
}

struct NotificationQueue {
    sender: Mutex<Option<mpsc::UnboundedSender<NotificationCommand>>>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl NotificationQueue {
    fn new(transport: Arc<JsonRpcLineTransport>) -> Self {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            while let Some(command) = receiver.recv().await {
                match command {
                    NotificationCommand::Send(params) => {
                        if let Err(error) = transport
                            .notify(client_methods::SESSION_UPDATE, Some(params))
                            .await
                        {
                            tracing::warn!(%error, "ACP session/update failed");
                        }
                    }
                    NotificationCommand::Flush(done) => {
                        let _ = done.send(());
                    }
                    NotificationCommand::Shutdown(done) => {
                        let _ = done.send(());
                        break;
                    }
                }
            }
        });
        Self {
            sender: Mutex::new(Some(sender)),
            task: Mutex::new(Some(task)),
        }
    }

    fn enqueue(&self, params: Map<String, Value>) {
        if let Some(sender) = self.sender.lock().as_ref() {
            let _ = sender.send(NotificationCommand::Send(params));
        }
    }

    async fn flush(&self) {
        let sender = self.sender.lock().clone();
        let Some(sender) = sender else {
            return;
        };
        let (done, waiting) = oneshot::channel();
        if sender.send(NotificationCommand::Flush(done)).is_ok() {
            let _ = waiting.await;
        }
    }

    async fn shutdown(&self) {
        let sender = self.sender.lock().take();
        if let Some(sender) = sender {
            let (done, waiting) = oneshot::channel();
            if sender.send(NotificationCommand::Shutdown(done)).is_ok() {
                let _ = waiting.await;
            }
        }
        let task = self.task.lock().take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

/// One automation-only ACP connection and the exact agents it owns.
pub struct AcpBridge {
    context: Context,
    config: AcpBridgeConfig,
    agents: Arc<AgentRegistry>,
    transport: Arc<JsonRpcLineTransport>,
    notifications: NotificationQueue,
    sessions: Mutex<HashMap<AcpSessionId, SessionRecord>>,
    effects: Mutex<Vec<EffectHandle>>,
    continuable_drain_hook: Option<AcpContinuableDrainHook>,
    connection_closed: seekdeep_llm::AbortSignal,
    closed: AtomicBool,
    shutdown: OnceCell<Result<(), String>>,
}

impl AcpBridge {
    /// Constructs and wires the bridge without starting the transport reader.
    ///
    /// # Errors
    ///
    /// Returns missing-agent, listener, or approval-answerer registration failures.
    pub fn new(
        context: &Context,
        transport: &Arc<JsonRpcLineTransport>,
        config: AcpBridgeConfig,
    ) -> anyhow::Result<Arc<Self>> {
        Self::new_inner(context, transport, config, None, None)
    }

    /// Constructs the bridge with the exact assembled Agent registry.
    #[doc(hidden)]
    pub fn new_with_agents(
        context: &Context,
        transport: &Arc<JsonRpcLineTransport>,
        config: AcpBridgeConfig,
        agents: Arc<AgentRegistry>,
    ) -> anyhow::Result<Arc<Self>> {
        Self::new_inner(context, transport, config, Some(agents), None)
    }

    /// Constructs the bridge with an explicit continuable-drain seam.
    ///
    /// Production callers use [`Self::new`]; conformance tests use this seam to
    /// prove ordering and failure containment without fabricating descendant state.
    #[doc(hidden)]
    pub fn new_with_continuable_drain(
        context: &Context,
        transport: &Arc<JsonRpcLineTransport>,
        config: AcpBridgeConfig,
        drain: AcpContinuableDrainHook,
    ) -> anyhow::Result<Arc<Self>> {
        Self::new_inner(context, transport, config, None, Some(drain))
    }

    fn new_inner(
        context: &Context,
        transport: &Arc<JsonRpcLineTransport>,
        config: AcpBridgeConfig,
        explicit_agents: Option<Arc<AgentRegistry>>,
        continuable_drain_hook: Option<AcpContinuableDrainHook>,
    ) -> anyhow::Result<Arc<Self>> {
        let agents = explicit_agents
            .or_else(|| context.get(AGENTS))
            .ok_or_else(|| anyhow::anyhow!("acp requires agents"))?;
        let bridge = Arc::new(Self {
            context: context.clone(),
            config,
            agents,
            transport: Arc::clone(transport),
            notifications: NotificationQueue::new(Arc::clone(transport)),
            sessions: Mutex::new(HashMap::new()),
            effects: Mutex::new(Vec::new()),
            continuable_drain_hook,
            connection_closed: seekdeep_llm::AbortSignal::default(),
            closed: AtomicBool::new(false),
            shutdown: OnceCell::new(),
        });
        bridge.register_events()?;
        let weak = Arc::downgrade(&bridge);
        transport.on_request(Arc::new(move |method, params| {
            let weak = weak.clone();
            Box::pin(async move {
                let Some(bridge) = weak.upgrade() else {
                    return Err(internal_error("the ACP bridge has been disposed"));
                };
                bridge.handle_request(&method, params).await
            })
        }));
        let weak = Arc::downgrade(&bridge);
        transport.on_notification(Arc::new(move |method, params| {
            if let Some(bridge) = weak.upgrade()
                && method == agent_methods::SESSION_CANCEL
            {
                bridge.cancel_raw(&params);
            }
        }));
        let weak = Arc::downgrade(&bridge);
        transport.on_input_failure(Arc::new(move |error| {
            tracing::warn!(%error, "ACP connection closed with an error");
            if let Some(bridge) = weak.upgrade() {
                bridge
                    .connection_closed
                    .abort_with_reason(Value::String("ACP connection input closed".to_owned()));
                tokio::spawn(async move {
                    if let Err(error) = bridge.shutdown().await {
                        tracing::warn!(%error, "ACP connection-close teardown failed");
                    }
                });
            }
        }));
        Ok(bridge)
    }

    /// Starts reading protocol frames.
    pub fn start(&self) {
        self.transport.start();
    }

    /// Signal that aborts when the peer input ends or fails.
    #[must_use]
    pub fn connection_closed_signal(&self) -> seekdeep_llm::AbortSignal {
        self.connection_closed.clone()
    }

    /// Dispatches one ACP request.
    ///
    /// # Errors
    ///
    /// Returns protocol validation, session, creation, prompt, or lifecycle failures.
    pub async fn handle_request(
        self: &Arc<Self>,
        method: &str,
        params: Map<String, Value>,
    ) -> anyhow::Result<Value> {
        match method {
            agent_methods::INITIALIZE => Ok(json!({
                "protocolVersion":PROTOCOL_VERSION,
                "agentInfo":{"name":"seekdeep-harness-acp","version":"0.0.1"},
                "agentCapabilities":{"promptCapabilities":{"image":false,"audio":false,"embeddedContext":false}},
                "authMethods":[]
            })),
            agent_methods::AUTHENTICATE => Ok(json!({})),
            agent_methods::SESSION_NEW => self.new_session(params).await,
            agent_methods::SESSION_PROMPT => self.prompt(params).await,
            _ => Err(anyhow::anyhow!("method not found: {method}")),
        }
    }

    async fn new_session(&self, params: Map<String, Value>) -> anyhow::Result<Value> {
        self.assert_open()?;
        let cwd = required_string(&params, "cwd")?;
        if !Path::new(&cwd).is_absolute() {
            return Err(invalid_params(format!(
                "cwd must be an absolute path: {cwd}"
            )));
        }
        if params
            .get("additionalDirectories")
            .and_then(Value::as_array)
            .is_some_and(|directories| !directories.is_empty())
        {
            return Err(invalid_params("additionalDirectories is not supported"));
        }
        let mcp_servers = params
            .get("mcpServers")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_params("mcpServers must be an array"))?;
        if !mcp_servers.is_empty() {
            return Err(invalid_params("mcpServers is not supported"));
        }
        let id = AcpSessionId::new(format!(
            "acp-{:016x}",
            NEXT_SESSION.fetch_add(1, Ordering::AcqRel)
        ));
        let mut options = CreateAgentOptions::new(SessionId::new(id.as_str()));
        options.meta.cwd = Some(cwd);
        options.agent_options = AgentOptions {
            provider: self.config.provider.clone().map(ProviderId::new),
            model: self.config.model.clone().map(ModelId::new),
            max_tokens: None,
            subagent_depth: None,
        };
        if let Some(system_prompt) = self.context.get(SYSTEM_PROMPT) {
            let provider = self.config.provider.clone();
            let model = self.config.model.clone();
            let session_cwd = options.meta.cwd.clone().unwrap_or_default();
            options.setup = Some(Arc::new(move |agent_context| {
                let system_prompt = system_prompt.clone();
                let provider = provider.clone();
                let model = model.clone();
                let session_cwd = session_cwd.clone();
                Box::pin(async move {
                    if let (Some(provider), Some(model)) = (provider, model) {
                        let selection = Arc::new(parking_lot::RwLock::new(ModelSelectionRef {
                            current: Some(ModelSelection {
                                provider: ProviderId::new(provider),
                                model: ModelId::new(model),
                                reasoning_effort: None,
                            }),
                            assembled: None,
                        }));
                        let _ = install_model_selection(&agent_context, &system_prompt, selection)?;
                    }
                    system_prompt.variable(
                        &agent_context,
                        "cwd",
                        Arc::new(move |_| Ok(Some(session_cwd.clone()))),
                    )?;
                    Ok(None)
                })
            }));
        }
        let handle = Arc::new(self.agents.create(options).await?);
        if self.closed.load(Ordering::Acquire) {
            handle.dispose().await?;
            return Err(internal_error("connection closed during session/new"));
        }
        self.sessions.lock().insert(
            id.clone(),
            SessionRecord {
                handle,
                inflight: None,
            },
        );
        Ok(json!({"sessionId":id.as_str()}))
    }

    async fn prompt(self: &Arc<Self>, params: Map<String, Value>) -> anyhow::Result<Value> {
        self.assert_open()?;
        let id = AcpSessionId::new(required_string(&params, "sessionId")?);
        let prompt = params
            .get("prompt")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid_params("prompt must be an array"))?;
        if prompt_has_unsupported_content(prompt) {
            return Err(invalid_params(
                "only text and resource_link prompt content is supported",
            ));
        }
        let text = acp_prompt_to_text(prompt);
        if text.trim().is_empty() {
            return Err(invalid_params("empty prompt"));
        }
        let (receiver, agent, message_id, message) = {
            let mut sessions = self.sessions.lock();
            let record = sessions
                .get_mut(&id)
                .ok_or_else(|| invalid_params(format!("unknown session: {}", id.as_str())))?;
            if record.inflight.is_some() {
                return Err(invalid_params(
                    "a prompt is already in flight for this session",
                ));
            }
            if !self
                .agents
                .get(record.handle.agent.id())
                .is_some_and(|agent| Arc::ptr_eq(&agent, &record.handle.agent))
            {
                return Err(internal_error(
                    "prompt was not queued: the agent was disposed outside the bridge",
                ));
            }
            let message = UserMessage::new(
                vec![seekdeep_llm::ContentBlock::Text { text }],
                MessageSource {
                    kind: "user".to_owned(),
                    fields: Map::new(),
                },
            );
            let message_id = message.id().as_str().to_owned();
            let (sender, receiver) = oneshot::channel();
            record.inflight = Some(InflightPrompt {
                sender: Some(sender),
                message_id: message_id.clone(),
                turn: None,
                end_kind: None,
            });
            (
                receiver,
                Arc::clone(&record.handle.agent),
                message_id,
                message,
            )
        };
        if let Err(error) = agent.followup(message) {
            let mut sessions = self.sessions.lock();
            if let Some(record) = sessions.get_mut(&id)
                && record
                    .inflight
                    .as_ref()
                    .is_some_and(|inflight| inflight.message_id == message_id)
            {
                record.inflight = None;
            }
            return Err(internal_error(format!("prompt was not queued: {error}")));
        }
        let weak = Arc::downgrade(self);
        let idle_id = id.clone();
        tokio::spawn(async move {
            let result = match agent.when_idle() {
                Ok(wait) => wait.await,
                Err(error) => Err(error.into()),
            };
            if let Some(bridge) = weak.upgrade() {
                bridge.settle_idle(&idle_id, &message_id, result);
            }
        });
        let reason = receiver
            .await
            .map_err(|_| internal_error("ACP prompt settlement channel closed"))??;
        self.notifications.flush().await;
        Ok(json!({"stopReason":reason.as_str()}))
    }

    fn cancel_raw(&self, params: &Map<String, Value>) {
        let Some(id) = params.get("sessionId").and_then(Value::as_str) else {
            return;
        };
        self.cancel(&AcpSessionId::new(id));
    }

    /// Cancels one owned session; unknown ids are no-ops.
    pub fn cancel(&self, id: &AcpSessionId) {
        let Some((agent, sender)) = ({
            let mut sessions = self.sessions.lock();
            let Some(record) = sessions.get_mut(id) else {
                return;
            };
            let agent = Arc::clone(&record.handle.agent);
            let sender = record
                .inflight
                .take()
                .and_then(|mut inflight| inflight.sender.take());
            Some((agent, sender))
        }) else {
            return;
        };
        let _ = agent.cancel(AgentCancelCause::User, CancelOptions::default());
        if let Some(sender) = sender {
            let _ = sender.send(Ok(AcpStopReason::Cancelled));
        }
    }

    fn settle_idle(&self, id: &AcpSessionId, message_id: &str, idle: anyhow::Result<()>) {
        let settlement = {
            let mut sessions = self.sessions.lock();
            let Some(record) = sessions.get_mut(id) else {
                return;
            };
            if record
                .inflight
                .as_ref()
                .is_none_or(|inflight| inflight.message_id != message_id)
            {
                return;
            }
            let mut inflight = record.inflight.take().expect("checked inflight");
            inflight.sender.take().map(|sender| {
                let result = idle.map(|()| {
                    inflight
                        .end_kind
                        .as_deref()
                        .map_or(AcpStopReason::Cancelled, |kind| {
                            if kind == "max-tokens" {
                                AcpStopReason::EndTurn
                            } else {
                                turn_end_to_stop_reason(kind)
                            }
                        })
                });
                (sender, result)
            })
        };
        if let Some((sender, result)) = settlement {
            let _ = sender.send(result);
        }
    }

    /// Cancels and disposes all exact connection-owned agents once.
    ///
    /// # Errors
    ///
    /// Returns aggregate owned-session teardown failures.
    pub async fn shutdown(self: &Arc<Self>) -> anyhow::Result<()> {
        let result = self
            .shutdown
            .get_or_init(|| async {
                self.perform_shutdown()
                    .await
                    .map_err(|error| error.to_string())
            })
            .await;
        result.clone().map_err(anyhow::Error::msg)
    }

    async fn perform_shutdown(&self) -> anyhow::Result<()> {
        self.closed.store(true, Ordering::Release);
        let mut records = {
            let mut sessions = self.sessions.lock();
            std::mem::take(&mut *sessions)
                .into_values()
                .collect::<Vec<_>>()
        };
        let mut prompt_senders = Vec::new();
        for record in &mut records {
            let _ = record
                .handle
                .agent
                .cancel(AgentCancelCause::User, CancelOptions::default());
            if let Some(sender) = record
                .inflight
                .take()
                .and_then(|mut inflight| inflight.sender.take())
            {
                prompt_senders.push(sender);
            }
        }
        for sender in prompt_senders {
            let _ = sender.send(Ok(AcpStopReason::Cancelled));
        }
        let parents = records
            .iter()
            .map(|record| Arc::clone(&record.handle.agent))
            .collect::<Vec<_>>();
        let drain = if let Some(hook) = &self.continuable_drain_hook {
            hook(parents).await
        } else if let Some(subagents) = self.context.get(SUBAGENTS) {
            subagents.drain_continuable_descendants(&parents).await
        } else {
            Ok(())
        };
        if let Err(error) = drain {
            tracing::warn!(%error, "ACP continuable subagent teardown failed");
        }
        let effects = std::mem::take(&mut *self.effects.lock());
        for effect in effects.into_iter().rev() {
            if let Err(error) = effect.dispose().await {
                tracing::warn!(%error, "ACP listener teardown failed");
            }
        }
        let failures = join_all(
            records
                .into_iter()
                .map(|record| async move { record.handle.dispose().await }),
        )
        .await
        .into_iter()
        .filter_map(Result::err)
        .map(|error| format!("{error:#}"))
        .collect::<Vec<_>>();
        self.notifications.shutdown().await;
        if failures.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(
                "ACP agent teardown failed for {} session(s): {}",
                failures.len(),
                failures.join("; ")
            )
        }
    }

    fn assert_open(&self) -> anyhow::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            Err(internal_error("the ACP bridge has been disposed"))
        } else {
            Ok(())
        }
    }

    fn register_events(self: &Arc<Self>) -> anyhow::Result<()> {
        let weak = Arc::downgrade(self);
        let session = self.context.events().on_sync(
            &self.context,
            "session/event",
            move |_, args| {
                if let Some(bridge) = weak.upgrade() {
                    bridge.on_session_event(&args)?;
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;
        let weak = Arc::downgrade(self);
        let claimed = self.context.events().on_sync(
            &self.context,
            "agent/inbox/claimed",
            move |_, args| {
                if let Some(bridge) = weak.upgrade() {
                    bridge.on_inbox_claimed(&args)?;
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;
        let weak = Arc::downgrade(self);
        let error = self.context.events().on_sync(
            &self.context,
            "agent/error",
            move |_, args| {
                if let Some(bridge) = weak.upgrade() {
                    bridge.on_agent_error(&args)?;
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;
        self.effects.lock().extend([session, claimed, error]);
        let weak: Weak<Self> = Arc::downgrade(self);
        let approval = register_approval_answerer(
            &self.context,
            move |request, next| {
                let weak = weak.clone();
                async move {
                    let Some(bridge) = weak.upgrade() else {
                        return next.run().await;
                    };
                    bridge.answer_approval(request, next).await
                }
            },
            EventOptions::default(),
        )?;
        self.effects.lock().push(approval);
        Ok(())
    }

    fn on_session_event(&self, args: &EventArgs) -> anyhow::Result<()> {
        let session = required::<Session>(args, 0, "session/event session")?;
        let event = required::<SessionEvent>(args, 1, "session/event event")?;
        let id = AcpSessionId::new(session.id().as_str());
        let (updates, error_sender) = {
            let mut sessions = self.sessions.lock();
            let Some(record) = sessions.get_mut(&id) else {
                return Ok(());
            };
            if !Arc::ptr_eq(record.handle.agent.session(), &session) {
                return Ok(());
            }
            let mut updates = Vec::new();
            if event.event_type == "assistant/message"
                && let Some(content) = event
                    .data
                    .pointer("/message/content")
                    .and_then(Value::as_array)
            {
                for block in content {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(Value::as_str)
                                && !text.is_empty()
                            {
                                updates.push(json!({
                                    "sessionId":id.as_str(),
                                    "update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}
                                }));
                            }
                        }
                        Some("image") => {
                            if let Some(attachment) = block
                                .pointer("/attachment/attachmentId")
                                .and_then(Value::as_str)
                            {
                                updates.push(json!({
                                    "sessionId":id.as_str(),
                                    "update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":format!("[image attachment {attachment}]")}}
                                }));
                            }
                        }
                        Some(_) | None => {}
                    }
                }
            }
            let mut error_sender = None;
            if event.event_type == "turn/end"
                && let Some(inflight) = record.inflight.as_mut()
                && inflight.turn == event.data.get("turn").and_then(Value::as_u64)
            {
                let kind = event
                    .data
                    .pointer("/reason/kind")
                    .and_then(Value::as_str)
                    .unwrap_or("completed");
                if kind == "error" {
                    let message = event
                        .data
                        .pointer("/reason/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown error");
                    let mut taken = record.inflight.take().expect("inflight present");
                    error_sender = taken
                        .sender
                        .take()
                        .map(|sender| (sender, internal_error(format!("turn failed: {message}"))));
                } else {
                    inflight.end_kind = Some(kind.to_owned());
                }
            }
            (updates, error_sender)
        };
        for update in updates {
            if let Value::Object(params) = update {
                self.notifications.enqueue(params);
            }
        }
        if let Some((sender, error)) = error_sender {
            let _ = sender.send(Err(error));
        }
        Ok(())
    }

    fn on_inbox_claimed(&self, args: &EventArgs) -> anyhow::Result<()> {
        let event =
            required::<AgentEvent<AgentInboxClaimed>>(args, 0, "agent/inbox/claimed event")?;
        let id = AcpSessionId::new(event.agent.id().as_str());
        let mut sessions = self.sessions.lock();
        let Some(record) = sessions.get_mut(&id) else {
            return Ok(());
        };
        if !Arc::ptr_eq(&record.handle.agent, &event.agent) {
            return Ok(());
        }
        if let Some(inflight) = record.inflight.as_mut()
            && inflight.message_id == event.payload.message.id().as_str()
        {
            inflight.turn = Some(event.payload.turn);
        }
        Ok(())
    }

    fn on_agent_error(&self, args: &EventArgs) -> anyhow::Result<()> {
        let event = required::<AgentEvent<AgentErrorEvent>>(args, 0, "agent/error event")?;
        let id = AcpSessionId::new(event.agent.id().as_str());
        let sender = {
            let mut sessions = self.sessions.lock();
            let Some(record) = sessions.get_mut(&id) else {
                return Ok(());
            };
            if !Arc::ptr_eq(&record.handle.agent, &event.agent) {
                return Ok(());
            }
            if record
                .inflight
                .as_ref()
                .is_some_and(|inflight| inflight.turn != Some(event.payload.turn))
            {
                record
                    .inflight
                    .take()
                    .and_then(|mut inflight| inflight.sender.take())
            } else {
                None
            }
        };
        if let Some(sender) = sender {
            let _ = sender.send(Err(internal_error(format!(
                "turn failed: {}",
                event.payload.error
            ))));
        }
        Ok(())
    }

    async fn answer_approval(
        &self,
        request: ApprovalRequest,
        next: seekdeep_user_approval::ApprovalNext,
    ) -> anyhow::Result<ApprovalAnswer> {
        let Some(call_id) = &request.call_id else {
            return next.run().await;
        };
        let owned = self
            .sessions
            .lock()
            .get(&AcpSessionId::new(request.agent.id().as_str()))
            .is_some_and(|record| Arc::ptr_eq(&record.handle.agent, &request.agent));
        if !owned {
            return next.run().await;
        }
        let value = self
            .transport
            .request(
                client_methods::SESSION_REQUEST_PERMISSION,
                Map::from_iter([
                    (
                        "sessionId".to_owned(),
                        Value::String(request.agent.id().as_str().to_owned()),
                    ),
                    (
                        "toolCall".to_owned(),
                        json!({"toolCallId":call_id.as_str()}),
                    ),
                    (
                        "options".to_owned(),
                        json!([
                            {"optionId":"allow-once","name":"Allow once","kind":"allow_once"},
                            {"optionId":"reject-once","name":"Reject","kind":"reject_once"}
                        ]),
                    ),
                ]),
                request.signal.clone(),
            )
            .await?;
        let outcome = value.pointer("/outcome/outcome").and_then(Value::as_str);
        let answer = if outcome == Some("cancelled") {
            ApprovalOutcome::Cancelled
        } else if value.pointer("/outcome/optionId").and_then(Value::as_str) == Some("allow-once") {
            ApprovalOutcome::AllowedOnce
        } else {
            ApprovalOutcome::Rejected
        };
        Ok(ApprovalAnswer::Outcome(answer))
    }
}

fn required<T: Send + Sync + 'static>(
    args: &EventArgs,
    index: usize,
    label: &str,
) -> anyhow::Result<Arc<T>> {
    args.get::<T>(index)
        .ok_or_else(|| anyhow::anyhow!("{label} is missing"))
}

fn required_string(params: &Map<String, Value>, name: &str) -> anyhow::Result<String> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| invalid_params(format!("{name} must be a string")))
}

fn invalid_params(detail: impl Into<String>) -> anyhow::Error {
    let detail = detail.into();
    anyhow::Error::new(JsonRpcResponseError {
        code: Some(-32602),
        message: format!("Invalid params: {detail}"),
        data: None,
    })
}

fn internal_error(detail: impl Into<String>) -> anyhow::Error {
    let detail = detail.into();
    anyhow::Error::new(JsonRpcResponseError {
        code: Some(-32603),
        message: format!("Internal error: {detail}"),
        data: None,
    })
}
