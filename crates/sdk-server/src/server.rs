//! Typed SDK request dispatch, session ownership, and lifecycle notifications.

use std::{collections::HashMap, path::Path, sync::Arc};

use futures::future::join_all;
use parking_lot::Mutex;
use path_clean::PathClean as _;
use seekdeep_agent::{
    AGENTS, AgentEvent, AgentHandle, AgentRegistry, AgentStatus, AgentStatusChanged,
    CreateAgentOptions,
};
use seekdeep_cordis::{
    Context, EventArgs, EventOptions, EventReply, PluginFiber, fiber::EffectHandle,
};
use seekdeep_core::session::{Session, SessionEvent, SessionId};
use seekdeep_llm::{LLM, MessageSource, UserMessage};
use seekdeep_scope::carrier_key_of;
use seekdeep_sdk_protocol::{
    InitializeParams, InitializeResult, JsonRpcLineTransport, SdkRunStatus, ServerInfo,
    SessionEventNotification, SessionPromptParams, SessionPromptResult, SessionStatus,
    SessionStatusNotification, SubagentFinishedNotification, SubagentStartedNotification,
};
use seekdeep_subagent::{SubagentRunEndInfo, SubagentStopReason};
use serde_json::{Map, Value};
use tokio::sync::Notify;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Deployment-specific outcome mapping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HarnessSdkJsonRpcServerOptions {
    /// Treat max-token terminal facts as accepted SDK outcomes.
    pub max_tokens_as_success: bool,
}

/// Maps one shared stop reason to the SDK result status.
#[must_use]
pub fn success_status(
    reason: SubagentStopReason,
    options: HarnessSdkJsonRpcServerOptions,
) -> SdkRunStatus {
    match reason {
        SubagentStopReason::Completed => SdkRunStatus::Ok,
        SubagentStopReason::MaxTokens if options.max_tokens_as_success => SdkRunStatus::Ok,
        SubagentStopReason::MaxTokens
        | SubagentStopReason::Aborted
        | SubagentStopReason::Error
        | SubagentStopReason::Refusal => SdkRunStatus::Error,
    }
}

#[derive(Default)]
struct CreationSlot {
    result: Mutex<Option<Result<Arc<AgentHandle>, String>>>,
    notify: Notify,
}

impl CreationSlot {
    fn set(&self, result: anyhow::Result<Arc<AgentHandle>>) {
        *self.result.lock() = Some(result.map_err(|error| error.to_string()));
        self.notify.notify_waiters();
    }

    async fn get(&self) -> anyhow::Result<Arc<AgentHandle>> {
        loop {
            let notified = self.notify.notified();
            if let Some(result) = self.result.lock().clone() {
                return result.map_err(anyhow::Error::msg);
            }
            notified.await;
        }
    }
}

struct ServerState {
    cwd: String,
    provider: String,
    model: String,
    max_tokens: Option<u64>,
    llm_fiber: Option<Arc<PluginFiber>>,
    sessions: HashMap<SessionId, Arc<AgentHandle>>,
    creations: HashMap<SessionId, Arc<CreationSlot>>,
    shutting_down: bool,
}

impl ServerState {
    fn new() -> Self {
        Self {
            cwd: std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            provider: "deepseek-official".to_owned(),
            model: "deepseek-official".to_owned(),
            max_tokens: None,
            llm_fiber: None,
            sessions: HashMap::new(),
            creations: HashMap::new(),
            shutting_down: false,
        }
    }
}

#[derive(Default)]
struct ShutdownState {
    started: std::sync::atomic::AtomicBool,
    result: Mutex<Option<Result<Map<String, Value>, String>>>,
    notify: Notify,
}

/// Server over one booted Harness context and one transport peer.
pub struct HarnessSdkJsonRpcServer {
    context: Context,
    options: HarnessSdkJsonRpcServerOptions,
    agents: Arc<AgentRegistry>,
    state: Mutex<ServerState>,
    subscriptions: Mutex<Vec<EffectHandle>>,
    notifications: tokio::sync::mpsc::UnboundedSender<(String, Map<String, Value>)>,
    notification_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    shutdown: Arc<ShutdownState>,
}

impl HarnessSdkJsonRpcServer {
    /// Constructs the server and subscribes to lifecycle events.
    ///
    /// # Errors
    ///
    /// Returns missing-agent or listener-registration failures.
    pub fn new(
        context: &Context,
        transport: &Arc<JsonRpcLineTransport>,
        options: HarnessSdkJsonRpcServerOptions,
    ) -> anyhow::Result<Arc<Self>> {
        let agents = context
            .get(AGENTS)
            .ok_or_else(|| anyhow::anyhow!("sdk-jsonrpc-server requires agents"))?;
        let (notifications, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let notification_transport = Arc::clone(transport);
        let notification_task = tokio::spawn(async move {
            while let Some((method, params)) = receiver.recv().await {
                if notification_transport
                    .notify(method, Some(params))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let server = Arc::new(Self {
            context: context.clone(),
            options,
            agents,
            state: Mutex::new(ServerState::new()),
            subscriptions: Mutex::new(Vec::new()),
            notifications,
            notification_task: Mutex::new(Some(notification_task)),
            shutdown: Arc::new(ShutdownState::default()),
        });
        server.register_notifications(context)?;
        Ok(server)
    }

    /// Whether a live LLM adapter owns the provider id.
    #[must_use]
    pub fn has_adapter_for(&self, provider: &str) -> bool {
        self.context.get(LLM).is_some_and(|runtime| {
            runtime
                .list_providers()
                .iter()
                .any(|entry| entry.id.as_str() == provider)
        })
    }

    /// Configures the SDK route and mounts the `DeepSeek` fallback only when unowned.
    ///
    /// # Errors
    ///
    /// Returns invalid token limits, unsupported providers, or fallback activation failures.
    pub async fn initialize(&self, params: InitializeParams) -> anyhow::Result<InitializeResult> {
        if let Some(max_tokens) = params.max_tokens {
            anyhow::ensure!(
                (1..=MAX_SAFE_INTEGER).contains(&max_tokens),
                "initialize maxTokens must be a positive safe integer"
            );
        }
        let cwd = resolve_path(&params.cwd)?;
        let provider = params.provider.as_str().to_owned();
        {
            let mut state = self.state.lock();
            state.cwd = cwd;
            state.provider.clone_from(&provider);
            params.model.as_str().clone_into(&mut state.model);
            state.max_tokens = params.max_tokens;
        }
        if !self.has_adapter_for(&provider) {
            anyhow::ensure!(
                provider == "deepseek-official",
                "no adapter registered for provider {provider:?}"
            );
            let fiber = self
                .context
                .plugin(seekdeep_llm_deepseek::plugin(), Value::Null)?;
            fiber.await_settled().await?;
            self.state.lock().llm_fiber = Some(fiber);
        }
        Ok(InitializeResult {
            server_info: ServerInfo {
                name: "seekdeep-harness-sdk-runtime".to_owned(),
                version: "0.0.1".to_owned(),
            },
        })
    }

    /// Queues one prompt and returns its durable message identity.
    ///
    /// # Errors
    ///
    /// Returns creation, stale-agent, message, or delivery failures.
    pub async fn prompt(&self, params: SessionPromptParams) -> anyhow::Result<SessionPromptResult> {
        let record = self
            .get_or_create_session(params.session_id.clone())
            .await?;
        anyhow::ensure!(
            self.agents
                .get(record.agent.id())
                .is_some_and(|agent| Arc::ptr_eq(&agent, &record.agent)),
            "session agent was disposed outside the server: {}",
            params.session_id
        );
        let message = UserMessage::new(
            params.content_blocks,
            MessageSource {
                kind: "user".to_owned(),
                fields: Map::new(),
            },
        );
        let message_id = message.id().clone();
        record.agent.followup(message)?;
        Ok(SessionPromptResult { message_id })
    }

    /// Dispatches one raw JSON-RPC method.
    ///
    /// # Errors
    ///
    /// Returns typed decoding, method, initialization, prompt, or shutdown failures.
    pub async fn handle_request(
        self: &Arc<Self>,
        method: &str,
        params: Map<String, Value>,
    ) -> anyhow::Result<Value> {
        match method {
            "initialize" => {
                validate_max_tokens(&params)?;
                Ok(serde_json::to_value(
                    self.initialize(serde_json::from_value(Value::Object(params))?)
                        .await?,
                )?)
            }
            "session/prompt" => Ok(serde_json::to_value(
                self.prompt(serde_json::from_value(Value::Object(params))?)
                    .await?,
            )?),
            "shutdown" => Ok(Value::Object(self.shutdown().await?)),
            _ => anyhow::bail!("unknown SeekDeep Harness SDK runtime method: {method}"),
        }
    }

    /// Disposes server-owned agents, fallback adapter, and subscriptions.
    ///
    /// # Errors
    ///
    /// Returns one teardown failure directly or aggregates several.
    pub async fn shutdown(self: &Arc<Self>) -> anyhow::Result<Map<String, Value>> {
        if !self
            .shutdown
            .started
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            let result = self.perform_shutdown().await;
            *self.shutdown.result.lock() = Some(result.map_err(|error| error.to_string()));
            self.shutdown.notify.notify_waiters();
        }
        loop {
            let notified = self.shutdown.notify.notified();
            if let Some(result) = self.shutdown.result.lock().clone() {
                return result.map_err(anyhow::Error::msg);
            }
            notified.await;
        }
    }

    fn register_notifications(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let session_server = Arc::downgrade(self);
        let session = context.events().on_sync(
            context,
            "session/event",
            move |_, args| {
                let Some(server) = session_server.upgrade() else {
                    return Ok(EventReply::Undefined);
                };
                let session = required::<Session>(&args, 0, "session/event session")?;
                let event = required::<SessionEvent>(&args, 1, "session/event event")?;
                server.notify(
                    "session.event",
                    &SessionEventNotification {
                        session_id: session.id().clone(),
                        event: (*event).clone(),
                    },
                );
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;
        let status_server = Arc::downgrade(self);
        let status = context.events().on_sync(
            context,
            "agent/status",
            move |_, args| {
                let Some(server) = status_server.upgrade() else {
                    return Ok(EventReply::Undefined);
                };
                let event =
                    required::<AgentEvent<AgentStatusChanged>>(&args, 0, "agent/status event")?;
                server.notify(
                    "session.status",
                    &SessionStatusNotification {
                        session_id: event.agent.id().clone(),
                        status: match event.payload.status {
                            AgentStatus::Idle => SessionStatus::Idle,
                            AgentStatus::Running => SessionStatus::Running,
                        },
                    },
                );
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;
        let created_server = Arc::downgrade(self);
        let created = context.events().on_sync(
            context,
            "session/created",
            move |_, args| {
                let Some(server) = created_server.upgrade() else {
                    return Ok(EventReply::Undefined);
                };
                let session = required::<Session>(&args, 0, "session/created session")?;
                if let Some(parent) = &session.header().parent_session {
                    server.notify(
                        "subagent.started",
                        &SubagentStartedNotification {
                            parent_session_id: parent.clone(),
                            child_session_id: session.id().clone(),
                        },
                    );
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;
        let end = self.register_subagent_end(context)?;
        self.subscriptions
            .lock()
            .extend([session, status, created, end]);
        Ok(())
    }

    fn register_subagent_end(self: &Arc<Self>, context: &Context) -> anyhow::Result<EffectHandle> {
        let end_server = Arc::downgrade(self);
        Ok(context.events().on_sync(
            context,
            "subagent/end",
            move |event_context, args| {
                let Some(server) = end_server.upgrade() else {
                    return Ok(EventReply::Undefined);
                };
                let info = required::<SubagentRunEndInfo>(&args, 0, "subagent/end info")?;
                if !info.local {
                    return Ok(EventReply::Undefined);
                }
                let Some(scope) = carrier_key_of(&event_context) else {
                    return Ok(EventReply::Undefined);
                };
                let Some(parent) = server
                    .agents
                    .list()
                    .into_iter()
                    .find(|agent| agent.scope_key() == scope)
                else {
                    return Ok(EventReply::Undefined);
                };
                server.notify(
                    "subagent.finished",
                    &SubagentFinishedNotification {
                        provider: info.provider.clone(),
                        agent_id: info.id.clone(),
                        parent_session_id: parent.id().clone(),
                        child_session_id: info.id.clone(),
                        status: success_status(info.stop_reason, server.options),
                        stop_reason: info.stop_reason,
                        last_assistant_message: info.last_assistant_message.clone(),
                    },
                );
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?)
    }

    fn notify(&self, method: &str, payload: &impl serde::Serialize) {
        let Ok(Value::Object(params)) = serde_json::to_value(payload) else {
            return;
        };
        let _ = self.notifications.send((method.to_owned(), params));
    }

    async fn get_or_create_session(&self, id: SessionId) -> anyhow::Result<Arc<AgentHandle>> {
        let (slot, creator) = {
            let mut state = self.state.lock();
            anyhow::ensure!(!state.shutting_down, "SDK server is shutting down");
            if let Some(record) = state.sessions.get(&id) {
                return Ok(Arc::clone(record));
            }
            if let Some(slot) = state.creations.get(&id) {
                (Arc::clone(slot), false)
            } else {
                let slot = Arc::new(CreationSlot::default());
                state.creations.insert(id.clone(), Arc::clone(&slot));
                (slot, true)
            }
        };
        if creator {
            let result = self.create_session(id.clone()).await;
            if let Ok(handle) = &result {
                self.state
                    .lock()
                    .sessions
                    .insert(id.clone(), Arc::clone(handle));
            }
            slot.set(result);
            self.state.lock().creations.remove(&id);
        }
        slot.get().await
    }

    async fn create_session(&self, id: SessionId) -> anyhow::Result<Arc<AgentHandle>> {
        let (cwd, provider, model, max_tokens) = {
            let state = self.state.lock();
            (
                state.cwd.clone(),
                state.provider.clone(),
                state.model.clone(),
                state.max_tokens,
            )
        };
        let mut options = CreateAgentOptions::new(id);
        options.meta.cwd = Some(cwd);
        options.agent_options.provider = Some(seekdeep_llm::ProviderId::new(provider));
        options.agent_options.model = Some(seekdeep_llm::ModelId::new(model));
        options.agent_options.max_tokens = max_tokens;
        Ok(Arc::new(self.agents.create(options).await?))
    }

    async fn perform_shutdown(&self) -> anyhow::Result<Map<String, Value>> {
        let creations = {
            let mut state = self.state.lock();
            state.shutting_down = true;
            state.creations.values().cloned().collect::<Vec<_>>()
        };
        let _ = join_all(creations.iter().map(|slot| slot.get())).await;
        let (records, llm_fiber) = {
            let mut state = self.state.lock();
            state.creations.clear();
            (
                std::mem::take(&mut state.sessions)
                    .into_values()
                    .collect::<Vec<_>>(),
                state.llm_fiber.take(),
            )
        };
        let subscriptions = std::mem::take(&mut *self.subscriptions.lock());
        let mut tasks = subscriptions
            .into_iter()
            .rev()
            .map(|effect| {
                Box::pin(async move { effect.dispose().await })
                    as futures::future::BoxFuture<'static, anyhow::Result<()>>
            })
            .collect::<Vec<_>>();
        tasks.extend(records.into_iter().map(|record| {
            Box::pin(async move { record.dispose().await })
                as futures::future::BoxFuture<'static, anyhow::Result<()>>
        }));
        if let Some(fiber) = llm_fiber {
            tasks.push(Box::pin(async move { fiber.dispose().await }));
        }
        let failures = join_all(tasks)
            .await
            .into_iter()
            .filter_map(Result::err)
            .collect::<Vec<_>>();
        if let Some(task) = self.notification_task.lock().take() {
            task.abort();
        }
        match failures.len() {
            0 => Ok(Map::new()),
            1 => Err(failures.into_iter().next().expect("one failure")),
            _ => Err(anyhow::anyhow!(
                "SDK server teardown failed: {}",
                failures
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            )),
        }
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

fn resolve_path(value: &str) -> anyhow::Result<String> {
    let path = Path::new(value);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(resolved.clean().to_string_lossy().into_owned())
}

fn validate_max_tokens(params: &Map<String, Value>) -> anyhow::Result<()> {
    let Some(value) = params.get("maxTokens") else {
        return Ok(());
    };
    anyhow::ensure!(
        value
            .as_u64()
            .is_some_and(|value| (1..=MAX_SAFE_INTEGER).contains(&value)),
        "initialize maxTokens must be a positive safe integer"
    );
    Ok(())
}
