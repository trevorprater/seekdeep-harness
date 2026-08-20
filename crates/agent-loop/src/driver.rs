//! Durable turn/step state machine whose requests reconstruct from the log.

use std::sync::{
    Arc, OnceLock, Weak,
    atomic::{AtomicBool, Ordering},
};

use futures::{StreamExt, future::BoxFuture};
use seekdeep_agent::{
    Agent, AgentEvents, InboxTarget, PreStepDecision, RequestErrorAction, assemble_context_for,
};
use seekdeep_core::{
    request_header::{AdapterDefaults, EpochHeader, canonical_header, header_equals},
    session::{AppendOptions, RequestContext, Session, SurfaceOp},
};
use seekdeep_llm::{
    AbortSignal, BlockAssembler, FinishReason, GenerateOptions, LlmCallConfig, LlmError,
    LlmFailure, LlmRuntime, Message, MessageSource, PreparedLlmCall, ProviderId,
    ResolvedRetryPolicy, ToolSchema,
};
use seekdeep_system_prompt::{
    PromptAssembly, SystemPrompt, join_context_sections, render_context_sections, render_prompt,
};
use seekdeep_tools::ToolRuntime;
use serde_json::{Value, json};

use crate::{
    DriverTask, LoopAgent, LoopController, RuntimeContextProjection, ToolCall, ToolCallBatch,
    execute_tool_calls,
};

/// Services and scheduler policy shared by all agents in one loop runtime.
#[derive(Clone)]
pub struct AgentLoopServices {
    /// Exact-model adapter registry and stream middleware.
    pub llm: Arc<LlmRuntime>,
    /// Scoped prompt assembly registry.
    pub system_prompt: Arc<SystemPrompt>,
    /// Scoped staged tool runtime.
    pub tools: Arc<ToolRuntime>,
    /// Maximum in-flight parallel-safe calls per step.
    pub max_parallel_tool_calls: usize,
}

impl std::fmt::Debug for AgentLoopServices {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentLoopServices")
            .field("max_parallel_tool_calls", &self.max_parallel_tool_calls)
            .finish_non_exhaustive()
    }
}

/// `agent/pre-step` payload fields.
#[derive(Clone, Debug)]
pub struct AgentPreStepEvent {
    /// Claimed messages proposed for the step.
    pub messages: Vec<seekdeep_llm::UserMessage>,
    /// Owning turn.
    pub turn: u64,
    /// Proposed step.
    pub step: u64,
    /// Current activity signal.
    pub signal: AbortSignal,
}

/// `agent/request` payload fields.
#[derive(Clone, Debug)]
pub struct AgentRequestEvent {
    /// Owning turn.
    pub turn: u64,
    /// Owning step.
    pub step: u64,
    /// Current activity signal.
    pub signal: AbortSignal,
}

/// `agent/request-error` payload fields.
#[derive(Clone, Debug)]
pub struct AgentRequestErrorEvent {
    /// Owning turn.
    pub turn: u64,
    /// Owning step.
    pub step: u64,
    /// Provider route used by the failed attempt.
    pub provider: ProviderId,
    /// Normalized terminal failure.
    pub failure: LlmFailure,
    /// Captured adapter retry policy when a prepared call existed.
    pub retry_policy: Option<ResolvedRetryPolicy>,
    /// Current activity signal.
    pub signal: AbortSignal,
}

/// `agent/turn-stopping` payload fields.
#[derive(Clone, Debug)]
pub struct AgentTurnStoppingEvent {
    /// Turn about to close.
    pub turn: u64,
    /// Current activity signal.
    pub signal: AbortSignal,
}

/// `agent/error` payload fields.
#[derive(Clone, Debug)]
pub struct AgentErrorEvent {
    /// Turn position at failure.
    pub turn: u64,
    /// Step position at failure.
    pub step: u64,
    /// Flattened error chain for cross-observer stability.
    pub error: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StepEndReason {
    Completed,
    MaxTokens,
}

struct PreparedStep {
    messages: Vec<seekdeep_llm::UserMessage>,
    assembly: PromptAssembly,
}

struct BuiltRequest {
    request: GenerateOptions,
    prepared: Option<PreparedLlmCall>,
}

/// Per-agent durable machine used by [`LoopAgent::new_default`].
pub struct DefaultAgentDriver {
    agent: Weak<Agent>,
    services: AgentLoopServices,
    events: AgentEvents,
    runtime_context: RuntimeContextProjection,
    request_header_logged: AtomicBool,
}

impl std::fmt::Debug for DefaultAgentDriver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DefaultAgentDriver")
            .field(
                "request_header_logged",
                &self.request_header_logged.load(Ordering::Acquire),
            )
            .finish_non_exhaustive()
    }
}

impl DefaultAgentDriver {
    /// Builds the driver after the agent scope exists but before publication.
    ///
    /// # Errors
    ///
    /// Returns runtime-context listener registration failures.
    pub fn new(agent: &Arc<Agent>, services: AgentLoopServices) -> anyhow::Result<Arc<Self>> {
        anyhow::ensure!(
            services.max_parallel_tool_calls > 0,
            "maxParallelToolCalls must be a positive integer"
        );
        Ok(Arc::new(Self {
            agent: Arc::downgrade(agent),
            services,
            events: AgentEvents::new(agent.context().clone(), agent.clone()),
            runtime_context: RuntimeContextProjection::new(agent.context(), agent.session())?,
            request_header_logged: AtomicBool::new(false),
        }))
    }

    /// Runs turns until the inbox drains or one boundary closes the activity.
    pub async fn run(self: &Arc<Self>, controller: &Arc<LoopController>) {
        loop {
            match self.turn(controller).await {
                Ok(true) => {
                    if controller.advance_turn().is_err() {
                        return;
                    }
                }
                Ok(false) | Err(_) => return,
            }
        }
    }

    async fn pre_step(
        &self,
        target: InboxTarget,
        turn: u64,
        step: u64,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<PreparedStep>> {
        let agent = self.agent()?;
        let claimed = agent.inbox().claim(target, turn)?;
        let assembly = self
            .services
            .system_prompt
            .assemble(assemble_context_for(&agent, Some(signal.clone())))
            .await?;
        ensure_not_aborted(signal)?;
        let sections = render_context_sections(&assembly)?;
        let context = self
            .runtime_context
            .project(&join_context_sections(&sections), &sections);
        let decision_messages = context.map_or_else(
            || claimed.clone(),
            |context| claimed.iter().cloned().chain([context]).collect(),
        );
        let decision = self
            .events
            .waterfall(
                "agent/pre-step",
                AgentPreStepEvent {
                    messages: claimed,
                    turn,
                    step,
                    signal: signal.clone(),
                },
                move || async move {
                    Ok(PreStepDecision::Enter {
                        messages: decision_messages,
                    })
                },
            )
            .await?;
        ensure_not_aborted(signal)?;
        match decision {
            PreStepDecision::Reject => Ok(None),
            PreStepDecision::Enter { messages } => Ok(Some(PreparedStep { messages, assembly })),
        }
    }

    async fn turn(&self, controller: &Arc<LoopController>) -> anyhow::Result<bool> {
        let agent = self.agent()?;
        let session = agent.session();
        let (last_turn, _) = controller.position().ok_or_else(|| {
            anyhow::anyhow!("agent {:?}: turn without driver reservation", agent.id())
        })?;
        let signal = controller
            .signal()
            .ok_or_else(|| anyhow::anyhow!("agent {:?}: running signal is absent", agent.id()))?;
        ensure_not_aborted(&signal)?;
        let turn = last_turn + 1;
        session.append(
            "turn/start",
            json!({"turn": turn}),
            AppendOptions::default(),
        )?;
        controller.set_position(turn, 0)?;

        let outcome = self.turn_body(controller, turn, &signal).await;
        let reason = match &outcome {
            Ok(TurnOutcome::Stopped(reason) | TurnOutcome::Continue(reason)) => reason.clone(),
            Err(error) if signal.is_aborted() => json!({
                "kind": "aborted",
                "reason": signal.reason().unwrap_or_else(|| json!({"kind": "legacy"})),
            }),
            Err(error) => {
                self.emit_error(controller, error);
                let failure = error.downcast_ref::<LlmError>().map_or_else(
                    || LlmFailure {
                        message: format!("{error:#}"),
                        code: "UNKNOWN".to_owned(),
                        status: None,
                        provider_retry_after_ms: None,
                        request_id: None,
                    },
                    |error| error.failure().clone(),
                );
                json!({"kind": "error", "error": failure})
            }
        };
        if let Err(error) = session.append(
            "turn/end",
            json!({"turn": turn, "reason": reason}),
            AppendOptions::default(),
        ) {
            let error = anyhow::Error::new(error);
            self.emit_error(controller, &error);
            return Err(error);
        }
        match outcome {
            Ok(TurnOutcome::Continue(_)) => Ok(true),
            Ok(TurnOutcome::Stopped(_)) | Err(_) => Ok(false),
        }
    }

    async fn turn_body(
        &self,
        controller: &Arc<LoopController>,
        turn: u64,
        signal: &AbortSignal,
    ) -> anyhow::Result<TurnOutcome> {
        let agent = self.agent()?;
        let session = agent.session();
        let mut target = InboxTarget::NextTurn;
        let mut turn_end: Option<StepEndReason> = None;
        loop {
            ensure_not_aborted(signal)?;
            let step = controller.position().map_or(1, |(_, step)| step + 1);
            let Some(prepared) = self.pre_step(target, turn, step, signal).await? else {
                return Ok(TurnOutcome::Stopped(json!({"kind": "blocked"})));
            };
            if turn_end.is_some() && prepared.messages.is_empty() {
                break;
            }
            if controller.position().is_some_and(|(_, step)| step == 0)
                && prepared.messages.is_empty()
            {
                return Ok(TurnOutcome::Stopped(json!({"kind": "completed"})));
            }
            ensure_not_aborted(signal)?;
            session.append(
                "step/start",
                json!({"turn": turn, "step": step}),
                AppendOptions::default(),
            )?;
            controller.set_position(turn, step)?;
            let step_result = async {
                for message in prepared.messages {
                    session.append(
                        "user/message",
                        serde_json::to_value(message)?,
                        AppendOptions {
                            surface_op: Some(SurfaceOp::append()),
                            ..AppendOptions::default()
                        },
                    )?;
                }
                self.step(controller, prepared.assembly).await
            }
            .await;
            let close_result = session.append(
                "step/end",
                json!({"turn": turn, "step": step}),
                AppendOptions::default(),
            );
            close_result?;
            let step_end = step_result?;
            if turn_end != Some(StepEndReason::MaxTokens) {
                turn_end = step_end;
            }
            ensure_not_aborted(signal)?;
            if turn_end.is_some() && agent.inbox().next_step().is_empty() {
                self.events
                    .serial(
                        "agent/turn-stopping",
                        AgentTurnStoppingEvent {
                            turn,
                            signal: signal.clone(),
                        },
                    )
                    .await?;
                ensure_not_aborted(signal)?;
            }
            if turn_end.is_some() && agent.inbox().next_step().is_empty() {
                break;
            }
            target = InboxTarget::NextStep;
        }
        let reason = match turn_end.unwrap_or(StepEndReason::Completed) {
            StepEndReason::Completed => json!({"kind": "completed"}),
            StepEndReason::MaxTokens => json!({"kind": "max-tokens"}),
        };
        if agent.inbox().has_pending() {
            Ok(TurnOutcome::Continue(reason))
        } else {
            Ok(TurnOutcome::Stopped(reason))
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn step(
        &self,
        controller: &Arc<LoopController>,
        assembly: PromptAssembly,
    ) -> anyhow::Result<Option<StepEndReason>> {
        let agent = self.agent()?;
        let (turn, step) = controller
            .position()
            .ok_or_else(|| anyhow::anyhow!("step outside running phase"))?;
        let signal = controller
            .signal()
            .ok_or_else(|| anyhow::anyhow!("step signal is absent"))?;
        ensure_not_aborted(&signal)?;
        let system = render_prompt(&assembly)?;
        loop {
            let BuiltRequest { request, prepared } = self
                .build_request(
                    turn,
                    step,
                    &assembly.tools,
                    &system,
                    agent.session().derive_messages(),
                    &signal,
                )
                .await?;
            let mut assembler = BlockAssembler::new();
            let mut chunk_seqs = Vec::new();
            let mut stream = match &prepared {
                Some(prepared) => prepared.stream(request.clone_preserving_agent_loop_request())?,
                None => self
                    .services
                    .llm
                    .stream(request.clone_preserving_agent_loop_request()),
            };
            ensure_not_aborted(&signal)?;
            while let Some(chunk) = stream.next().await {
                ensure_not_aborted(&signal)?;
                let chunk = chunk?;
                let event = agent.session().append(
                    "assistant/chunk",
                    json!({"turn": turn, "step": step, "chunk": chunk}),
                    AppendOptions::default(),
                )?;
                chunk_seqs.push(event.seq);
                assembler.push(chunk);
            }
            ensure_not_aborted(&signal)?;
            let finish = assembler.finish();
            if let FinishReason::Error { failure } | FinishReason::Aborted { failure } = finish {
                let retry_policy = prepared.as_ref().map(|call| call.retry_policy().clone());
                let provider = request.provider.clone();
                let failure_for_default = failure.clone();
                let action = self
                    .events
                    .waterfall(
                        "agent/request-error",
                        AgentRequestErrorEvent {
                            turn,
                            step,
                            provider,
                            failure: failure.clone(),
                            retry_policy,
                            signal: signal.clone(),
                        },
                        || async move { Ok(RequestErrorAction::Terminal) },
                    )
                    .await?;
                ensure_not_aborted(&signal)?;
                if action == RequestErrorAction::Retry {
                    continue;
                }
                return Err(anyhow::Error::new(LlmError::new(
                    failure_for_default.message,
                    failure_for_default.code,
                    failure_for_default.status,
                    failure_for_default.provider_retry_after_ms,
                    failure_for_default.request_id,
                )?));
            }

            let mut source =
                MessageSource::model(request.provider.as_str(), request.model.as_str());
            if let Some(replay_state) = assembler.replay_state() {
                source
                    .fields
                    .insert("replayState".to_owned(), replay_state.clone());
            }
            let message = Message::new(
                seekdeep_llm::MessageRole::Assistant,
                assembler.blocks()?,
                source,
            );
            let mut data = serde_json::Map::new();
            data.insert("turn".to_owned(), Value::from(turn));
            data.insert("step".to_owned(), Value::from(step));
            data.insert("message".to_owned(), serde_json::to_value(&message)?);
            if let Some(usage) = assembler.usage() {
                data.insert("usage".to_owned(), serde_json::to_value(usage)?);
            }
            agent.session().append(
                "assistant/message",
                Value::Object(data),
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    source_event_seqs: Some(chunk_seqs),
                    ..AppendOptions::default()
                },
            )?;
            if finish == FinishReason::MaxTokens {
                return Ok(Some(StepEndReason::MaxTokens));
            }
            let tool_calls = message
                .content()
                .iter()
                .filter_map(ToolCall::from_content)
                .collect::<Vec<_>>();
            if tool_calls.is_empty() {
                return Ok(Some(StepEndReason::Completed));
            }
            let outcome = execute_tool_calls(
                ToolCallBatch {
                    runtime: &self.services.tools,
                    session: agent.session(),
                    agent: Some(&agent),
                    agent_scope: Some(agent.scope_key()),
                    turn,
                    step,
                    tool_calls: &tool_calls,
                    signal: &signal,
                    max_parallel_tool_calls: self.services.max_parallel_tool_calls,
                },
                |context| {
                    agent
                        .inbox()
                        .splice(InboxTarget::NextStep, f64::INFINITY, 0.0, vec![context])
                        .map(|_| ())
                        .map_err(Into::into)
                },
            )
            .await?;
            return Ok(outcome.concluded.then_some(StepEndReason::Completed));
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_lines)]
    async fn build_request(
        &self,
        turn: u64,
        step: u64,
        tools: &[ToolSchema],
        system: &str,
        boundary_messages: Vec<Message>,
        signal: &AbortSignal,
    ) -> anyhow::Result<BuiltRequest> {
        let agent = self.agent()?;
        let persisted = agent.session().request_header();
        let route_provider = agent
            .options()
            .provider
            .clone()
            .unwrap_or_else(|| ProviderId::new(""));
        let route_model = agent
            .options()
            .model
            .clone()
            .unwrap_or_else(|| seekdeep_llm::ModelId::new(""));
        let reasoning_effort = persisted.as_ref().and_then(|header| {
            (header.config.provider == route_provider
                && header.config.model == route_model
                && header
                    .adapter_defaults
                    .as_ref()
                    .and_then(|defaults| defaults.reasoning_effort)
                    != Some(true))
            .then(|| header.config.reasoning_effort.clone())
            .flatten()
        });
        let seed = if self.request_header_logged.load(Ordering::Acquire) {
            request_proposal(
                persisted
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("logged request header disappeared"))?,
            )
        } else {
            LlmCallConfig {
                provider: route_provider,
                model: route_model,
                reasoning_effort,
                temperature: None,
                max_tokens: agent.options().max_tokens,
                stop: None,
            }
        };
        let proposed = self
            .events
            .waterfall(
                "agent/request",
                AgentRequestEvent {
                    turn,
                    step,
                    signal: signal.clone(),
                },
                move || async move { Ok(seed) },
            )
            .await?;
        ensure_not_aborted(signal)?;
        anyhow::ensure!(
            !proposed.provider.is_empty() && !proposed.model.is_empty(),
            "agent {:?} has no provider/model: set AgentOptions.provider and AgentOptions.model or supply both via the agent/request waterfall",
            agent.id()
        );

        let (config, prepared) = match self
            .services
            .llm
            .prepare_call(&proposed, Some(signal))
            .await
        {
            Ok(prepared) => (prepared.config().clone(), Some(prepared)),
            Err(error)
                if error
                    .downcast_ref::<LlmError>()
                    .is_some_and(|error| error.code() == "NO_ADAPTER") =>
            {
                (proposed, None)
            }
            Err(error) => return Err(error),
        };
        ensure_not_aborted(signal)?;
        let header = canonical_header(EpochHeader {
            config: config.clone(),
            adapter_defaults: prepared.as_ref().map(|call| AdapterDefaults {
                reasoning_effort: call.adapter_defaults().reasoning_effort,
                max_tokens: call.adapter_defaults().max_tokens,
            }),
            system: (!system.is_empty()).then(|| system.to_owned()),
            tools: (!tools.is_empty()).then(|| tools.to_vec()),
        });
        let baseline = agent.session().request_header();
        if !self.request_header_logged.swap(true, Ordering::AcqRel) {
            agent.session().append(
                "request/header",
                json!({
                    "header": header,
                    "reason": if baseline.is_none() { "initial" } else { "resume" },
                }),
                AppendOptions::default(),
            )?;
        } else if baseline
            .as_ref()
            .is_none_or(|baseline| !header_equals(baseline, &header))
        {
            agent.session().append(
                "request/header",
                json!({"header": header, "reason": "change"}),
                AppendOptions::default(),
            )?;
        }

        let request_context = RequestContext {
            provider: config.provider.clone(),
            model: config.model.clone(),
            context_window: prepared
                .as_ref()
                .and_then(PreparedLlmCall::context)
                .map(|context| context.context_window),
        };
        if agent.session().request_context().as_ref() != Some(&request_context) {
            agent.session().append(
                "request/context",
                serde_json::to_value(&request_context)?,
                AppendOptions::default(),
            )?;
        }
        ensure_not_aborted(signal)?;
        let mut request = GenerateOptions::new(
            header.config.provider,
            header.config.model,
            boundary_messages,
        );
        request.reasoning_effort = header.config.reasoning_effort;
        request.system = header.system;
        request.tools = header.tools;
        request.temperature = header.config.temperature;
        request.max_tokens = header.config.max_tokens;
        request.stop = header.config.stop;
        request.signal = Some(signal.clone());
        request.session_id = Some(agent.id().clone());
        Ok(BuiltRequest {
            request: request.mark_agent_loop_request(),
            prepared,
        })
    }

    fn emit_error(&self, controller: &LoopController, error: &anyhow::Error) {
        let (turn, step) = controller.position().unwrap_or_default();
        self.events.emit(
            "agent/error",
            AgentErrorEvent {
                turn,
                step,
                error: format!("{error:#}"),
            },
        );
    }

    fn agent(&self) -> anyhow::Result<Arc<Agent>> {
        self.agent
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("agent was disposed"))
    }
}

#[derive(Debug)]
enum TurnOutcome {
    Stopped(Value),
    Continue(Value),
}

impl LoopAgent {
    /// Composes an unpublished agent with the default durable driver.
    ///
    /// # Errors
    ///
    /// Returns scope, inbox, runtime-context, controller, or config failures.
    pub fn new_default(
        context: &seekdeep_cordis::Context,
        session: &Arc<Session>,
        options: seekdeep_agent::AgentOptions,
        parent_scope: Option<seekdeep_scope::ScopeKey>,
        services: AgentLoopServices,
    ) -> anyhow::Result<(Self, Arc<DefaultAgentDriver>)> {
        Self::new_default_with_registry(context, session, options, parent_scope, services, None)
    }

    /// Composes the default durable driver and attributes its entire returned
    /// foreground lifetime through the supplied agent registry.
    ///
    /// # Errors
    ///
    /// Returns scope, inbox, runtime-context, controller, or config failures.
    pub fn new_default_with_registry(
        context: &seekdeep_cordis::Context,
        session: &Arc<Session>,
        options: seekdeep_agent::AgentOptions,
        parent_scope: Option<seekdeep_scope::ScopeKey>,
        services: AgentLoopServices,
        registry: Option<seekdeep_agent::AgentRegistry>,
    ) -> anyhow::Result<(Self, Arc<DefaultAgentDriver>)> {
        let slot = Arc::new(OnceLock::<Arc<DefaultAgentDriver>>::new());
        let task_slot = slot.clone();
        let task: DriverTask = Arc::new(move |agent, controller| {
            let driver = task_slot.get().cloned();
            let registry = registry.clone();
            Box::pin(async move {
                if let Some(driver) = driver {
                    if let Some(registry) = registry {
                        let _ = registry
                            .scope_initiator(agent, driver.run(&controller))
                            .await;
                    } else {
                        driver.run(&controller).await;
                    }
                }
            }) as BoxFuture<'static, ()>
        });
        let loop_agent = Self::new(context, session, options, parent_scope, task)?;
        let driver = DefaultAgentDriver::new(&loop_agent.agent, services)?;
        slot.set(driver.clone())
            .map_err(|_| anyhow::anyhow!("default agent driver was already installed"))?;
        Ok((loop_agent, driver))
    }
}

fn request_proposal(header: &EpochHeader) -> LlmCallConfig {
    let mut proposal = header.config.clone();
    if let Some(defaults) = &header.adapter_defaults {
        if defaults.reasoning_effort == Some(true) {
            proposal.reasoning_effort = None;
        }
        if defaults.max_tokens == Some(true) {
            proposal.max_tokens = None;
        }
    }
    proposal
}

fn ensure_not_aborted(signal: &AbortSignal) -> anyhow::Result<()> {
    if signal.is_aborted() {
        anyhow::bail!("agent activity aborted")
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use async_trait::async_trait;
    use futures::stream;
    use parking_lot::Mutex;
    use seekdeep_llm::{AdapterStream, CallId, LlmAdapter, StreamChunk};
    use seekdeep_llm::{ContentBlock, FinishReason, MessageSource, UserMessage};
    use seekdeep_system_prompt::SystemPromptConfig;
    use seekdeep_tools::{
        ToolDefinition, ToolOutputDefinition, ToolRuntimeConfig, assert_supported_json_schema,
    };
    use serde_json::Map;
    use tokio::sync::Semaphore;

    use super::*;

    #[derive(Debug)]
    struct ScriptedAdapter {
        requests: Arc<Mutex<Vec<GenerateOptions>>>,
        calls: AtomicBool,
        tool_first: bool,
    }

    #[async_trait]
    impl LlmAdapter for ScriptedAdapter {
        fn stream(&self, options: GenerateOptions) -> AdapterStream {
            self.requests.lock().push(options);
            let first = !self.calls.swap(true, Ordering::AcqRel);
            let chunks = if self.tool_first && first {
                vec![
                    Ok(StreamChunk::BlockEnd {
                        index: 0,
                        block: ContentBlock::ToolCall {
                            id: CallId::new("call-echo"),
                            name: "echo".to_owned(),
                            arguments: "{\"text\":\"hello\"}".to_owned(),
                        },
                    }),
                    Ok(StreamChunk::Finish {
                        reason: FinishReason::ToolCalls,
                        replay_state: None,
                    }),
                ]
            } else {
                vec![
                    Ok(StreamChunk::TextDelta {
                        index: 0,
                        text: "done".to_owned(),
                    }),
                    Ok(StreamChunk::Finish {
                        reason: FinishReason::Stop,
                        replay_state: None,
                    }),
                ]
            };
            AdapterStream::new(stream::iter(chunks))
        }
    }

    #[derive(Debug)]
    struct BlockingThenStopAdapter {
        requests: Arc<Mutex<Vec<GenerateOptions>>>,
        calls: AtomicBool,
        first_stream_parked: Arc<Semaphore>,
    }

    #[async_trait]
    impl LlmAdapter for BlockingThenStopAdapter {
        fn stream(&self, options: GenerateOptions) -> AdapterStream {
            self.requests.lock().push(options.clone());
            if self.calls.swap(true, Ordering::AcqRel) {
                return AdapterStream::new(stream::iter([Ok(StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                })]));
            }
            let signal = options.signal.expect("loop signal");
            let parked = self.first_stream_parked.clone();
            AdapterStream::new(async_stream::stream! {
                yield Ok(StreamChunk::TextDelta {
                    index: 0,
                    text: "partial".to_owned(),
                });
                parked.add_permits(1);
                signal.cancelled().await;
            })
        }
    }

    fn user(text: &str) -> UserMessage {
        UserMessage::new(
            vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            MessageSource::user(),
        )
    }

    fn composition(
        context: &seekdeep_cordis::Context,
        tool_first: bool,
    ) -> (
        AgentLoopServices,
        Arc<Mutex<Vec<GenerateOptions>>>,
        Arc<ToolRuntime>,
    ) {
        let llm = LlmRuntime::install(context).expect("llm");
        let requests = Arc::new(Mutex::new(Vec::new()));
        llm.register_adapter(
            &["mock".to_owned()],
            Arc::new(ScriptedAdapter {
                requests: requests.clone(),
                calls: AtomicBool::new(false),
                tool_first,
            }),
        )
        .expect("adapter");
        let prompt = SystemPrompt::new(context, SystemPromptConfig::default()).expect("prompt");
        let tools =
            ToolRuntime::new_with_system_prompt(context, &prompt, ToolRuntimeConfig::default())
                .expect("tools");
        (
            AgentLoopServices {
                llm,
                system_prompt: prompt,
                tools: tools.clone(),
                max_parallel_tool_calls: 10,
            },
            requests,
            tools,
        )
    }

    fn blocking_composition(
        context: &seekdeep_cordis::Context,
    ) -> (
        AgentLoopServices,
        Arc<Mutex<Vec<GenerateOptions>>>,
        Arc<Semaphore>,
    ) {
        let llm = LlmRuntime::install(context).expect("llm");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let parked = Arc::new(Semaphore::new(0));
        llm.register_adapter(
            &["mock".to_owned()],
            Arc::new(BlockingThenStopAdapter {
                requests: requests.clone(),
                calls: AtomicBool::new(false),
                first_stream_parked: parked.clone(),
            }),
        )
        .expect("adapter");
        let prompt = SystemPrompt::new(context, SystemPromptConfig::default()).expect("prompt");
        let tools =
            ToolRuntime::new_with_system_prompt(context, &prompt, ToolRuntimeConfig::default())
                .expect("tools");
        (
            AgentLoopServices {
                llm,
                system_prompt: prompt,
                tools,
                max_parallel_tool_calls: 10,
            },
            requests,
            parked,
        )
    }

    #[tokio::test]
    async fn drives_one_turn_from_durable_input_to_reconstructed_assistant() {
        let context = seekdeep_cordis::Context::new();
        let (services, requests, _tools) = composition(&context, false);
        let session = Session::create(
            &seekdeep_core::session::SessionId::new("simple"),
            None,
            None,
        )
        .expect("session");
        let (loop_agent, _driver) = LoopAgent::new_default(
            &context,
            &session,
            seekdeep_agent::AgentOptions {
                provider: Some("mock".into()),
                model: Some("model".into()),
                max_tokens: None,
                subagent_depth: None,
            },
            None,
            services,
        )
        .expect("agent");
        loop_agent.agent.followup(user("hello")).expect("followup");
        loop_agent.agent.when_idle().expect("idle").await;

        let event_types = session
            .events()
            .into_iter()
            .map(|event| event.event_type)
            .collect::<Vec<_>>();
        assert_eq!(
            event_types,
            [
                "agent/inbox/spliced",
                "turn/start",
                "agent/inbox/spliced",
                "step/start",
                "user/message",
                "request/header",
                "request/context",
                "assistant/chunk",
                "assistant/chunk",
                "assistant/message",
                "step/end",
                "turn/end",
            ]
        );
        let surface = session.derive_messages();
        assert_eq!(surface.len(), 2);
        assert_eq!(surface[0].role(), seekdeep_llm::MessageRole::User);
        assert_eq!(surface[1].role(), seekdeep_llm::MessageRole::Assistant);
        let requests = requests.lock();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].messages.len(), 1);
        assert_eq!(
            requests[0]
                .session_id
                .as_ref()
                .map(seekdeep_llm::SessionId::as_str),
            Some("simple")
        );
        assert!(
            requests[0]
                .system
                .as_deref()
                .is_some_and(|system| system.contains("SeekDeep Harness"))
        );
        assert_eq!(
            session.events().last().expect("end").data["reason"]["kind"],
            "completed"
        );
    }

    #[tokio::test]
    async fn executes_tool_and_reconstructs_result_into_next_step_request() {
        let context = seekdeep_cordis::Context::new();
        let (services, requests, tools) = composition(&context, true);
        let output = ToolOutputDefinition::new(
            Arc::new(assert_supported_json_schema(json!({"type": "string"})).expect("schema")),
            Arc::new(|_, value| {
                Ok(vec![ContentBlock::Text {
                    text: value.as_str().unwrap_or_default().to_owned(),
                }])
            }),
        );
        tools
            .register(
                &context,
                ToolDefinition::new(
                    "echo",
                    "echo input",
                    Map::from_iter([(
                        "type".to_owned(),
                        serde_json::Value::String("object".to_owned()),
                    )]),
                    output,
                    Arc::new(|arguments, _execution| {
                        Box::pin(async move {
                            Ok(arguments
                                .get("text")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null))
                        })
                    }),
                ),
            )
            .expect("tool");
        let session = Session::create(&seekdeep_core::session::SessionId::new("tool"), None, None)
            .expect("session");
        let (loop_agent, _driver) = LoopAgent::new_default(
            &context,
            &session,
            seekdeep_agent::AgentOptions {
                provider: Some("mock".into()),
                model: Some("model".into()),
                max_tokens: None,
                subagent_depth: None,
            },
            None,
            services,
        )
        .expect("agent");
        loop_agent
            .agent
            .followup(user("use tool"))
            .expect("followup");
        loop_agent.agent.when_idle().expect("idle").await;

        let requests = requests.lock();
        assert_eq!(requests.len(), 2);
        assert!(
            requests[0]
                .messages
                .iter()
                .all(|message| message.source().kind != "tool")
        );
        assert!(
            requests[1]
                .messages
                .iter()
                .any(|message| message.source().kind == "tool")
        );
        let events = session.events();
        let call = events
            .iter()
            .find(|event| event.event_type == "tool/call")
            .expect("call");
        let result = events
            .iter()
            .find(|event| event.event_type == "tool/result")
            .expect("result");
        assert_eq!(result.source_event_seqs.as_deref(), Some(&[call.seq][..]));
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == "step/start")
                .count(),
            2
        );
        assert_eq!(
            events.last().expect("end").data["reason"]["kind"],
            "completed"
        );
    }

    #[tokio::test]
    async fn cancel_mid_stream_balances_the_turn_and_a_later_prompt_runs() {
        let context = seekdeep_cordis::Context::new();
        let (services, requests, parked) = blocking_composition(&context);
        let session = Session::create(
            &seekdeep_core::session::SessionId::new("cancel-stream"),
            None,
            None,
        )
        .expect("session");
        let (loop_agent, _driver) = LoopAgent::new_default(
            &context,
            &session,
            seekdeep_agent::AgentOptions {
                provider: Some("mock".into()),
                model: Some("model".into()),
                max_tokens: None,
                subagent_depth: None,
            },
            None,
            services,
        )
        .expect("agent");
        loop_agent.agent.followup(user("first")).expect("followup");
        parked.acquire().await.expect("parked").forget();
        loop_agent
            .agent
            .cancel(
                seekdeep_agent::AgentCancelCause::User,
                seekdeep_agent::CancelOptions::default(),
            )
            .expect("cancel");
        tokio::time::timeout(
            Duration::from_secs(1),
            loop_agent.agent.when_idle().expect("idle"),
        )
        .await
        .expect("cancel converged");
        let first_events = session.events();
        assert_eq!(
            first_events
                .iter()
                .filter(|event| event.event_type == "turn/start")
                .count(),
            1
        );
        assert_eq!(
            first_events
                .iter()
                .filter(|event| event.event_type == "turn/end")
                .count(),
            1
        );
        assert_eq!(
            first_events
                .iter()
                .filter(|event| event.event_type == "step/start")
                .count(),
            first_events
                .iter()
                .filter(|event| event.event_type == "step/end")
                .count()
        );
        assert_eq!(
            first_events.last().expect("turn end").data["reason"],
            json!({"kind": "aborted", "reason": {"kind": "user"}})
        );
        assert!(
            first_events
                .iter()
                .all(|event| event.event_type != "assistant/message")
        );

        loop_agent
            .agent
            .followup(user("after cancellation"))
            .expect("second prompt");
        loop_agent.agent.when_idle().expect("second idle").await;
        assert_eq!(requests.lock().len(), 2);
        assert_eq!(
            session
                .events()
                .iter()
                .filter(|event| event.event_type == "turn/end")
                .count(),
            2
        );
        assert_eq!(
            session.events().last().expect("second end").data["reason"]["kind"],
            "completed"
        );
    }

    #[tokio::test]
    async fn keep_inbox_parks_the_queued_tail_until_an_explicit_wake() {
        let context = seekdeep_cordis::Context::new();
        let (services, _requests, parked) = blocking_composition(&context);
        let session = Session::create(
            &seekdeep_core::session::SessionId::new("cancel-keep"),
            None,
            None,
        )
        .expect("session");
        let (loop_agent, _driver) = LoopAgent::new_default(
            &context,
            &session,
            seekdeep_agent::AgentOptions {
                provider: Some("mock".into()),
                model: Some("model".into()),
                max_tokens: None,
                subagent_depth: None,
            },
            None,
            services,
        )
        .expect("agent");
        loop_agent.agent.followup(user("active")).expect("active");
        parked.acquire().await.expect("parked").forget();
        loop_agent.agent.followup(user("queued")).expect("queued");
        loop_agent
            .agent
            .cancel(
                seekdeep_agent::AgentCancelCause::User,
                seekdeep_agent::CancelOptions { keep_inbox: true },
            )
            .expect("cancel");
        loop_agent.agent.when_idle().expect("idle").await;
        assert!(loop_agent.agent.inbox().has_pending());
        assert_eq!(
            session
                .events()
                .iter()
                .filter(|event| event.event_type == "turn/start")
                .count(),
            1
        );
        loop_agent.agent.followup(user("wake")).expect("wake");
        loop_agent.agent.when_idle().expect("replayed").await;
        assert!(!loop_agent.agent.inbox().has_pending());
        let events = session.events();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == "turn/start")
                .count(),
            3
        );
        let second_request_messages = events
            .iter()
            .filter(|event| event.event_type == "user/message")
            .filter_map(|event| event.data["content"][0]["text"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(second_request_messages, ["active", "queued", "wake"]);
    }

    #[tokio::test]
    async fn pre_step_can_rewrite_or_reject_before_opening_the_step() {
        use seekdeep_agent::{AgentEvent, PreStepDecision};
        use seekdeep_cordis::{EventOptions, EventReply};

        let rewrite_context = seekdeep_cordis::Context::new();
        let (services, requests, _tools) = composition(&rewrite_context, false);
        rewrite_context
            .events()
            .on_waterfall(
                &rewrite_context,
                "agent/pre-step",
                |_, args, _next| {
                    Box::pin(async move {
                        let event = args
                            .get::<AgentEvent<AgentPreStepEvent>>(0)
                            .ok_or_else(|| anyhow::anyhow!("missing pre-step"))?;
                        assert_eq!(event.payload.turn, 1);
                        assert_eq!(event.payload.step, 1);
                        Ok(EventReply::Value(Arc::new(PreStepDecision::Enter {
                            messages: vec![user("rewritten")],
                        })))
                    })
                },
                EventOptions::default(),
            )
            .expect("rewrite listener");
        let session = Session::create(
            &seekdeep_core::session::SessionId::new("rewrite"),
            None,
            None,
        )
        .expect("session");
        let (loop_agent, _driver) = LoopAgent::new_default(
            &rewrite_context,
            &session,
            seekdeep_agent::AgentOptions {
                provider: Some("mock".into()),
                model: Some("model".into()),
                max_tokens: None,
                subagent_depth: None,
            },
            None,
            services,
        )
        .expect("agent");
        loop_agent.agent.followup(user("original")).expect("prompt");
        loop_agent.agent.when_idle().expect("idle").await;
        {
            let requests = requests.lock();
            assert_eq!(requests.len(), 1);
            assert!(matches!(
                &requests[0].messages[0].content()[0],
                ContentBlock::Text { text } if text == "rewritten"
            ));
        }

        let reject_context = seekdeep_cordis::Context::new();
        let (services, requests, _tools) = composition(&reject_context, false);
        reject_context
            .events()
            .on_waterfall(
                &reject_context,
                "agent/pre-step",
                |_, _, _| {
                    Box::pin(async { Ok(EventReply::Value(Arc::new(PreStepDecision::Reject))) })
                },
                EventOptions::default(),
            )
            .expect("reject listener");
        let session = Session::create(
            &seekdeep_core::session::SessionId::new("reject"),
            None,
            None,
        )
        .expect("session");
        let (loop_agent, _driver) = LoopAgent::new_default(
            &reject_context,
            &session,
            seekdeep_agent::AgentOptions {
                provider: Some("mock".into()),
                model: Some("model".into()),
                max_tokens: None,
                subagent_depth: None,
            },
            None,
            services,
        )
        .expect("agent");
        loop_agent.agent.followup(user("blocked")).expect("prompt");
        loop_agent.agent.when_idle().expect("idle").await;
        assert!(requests.lock().is_empty());
        assert!(
            session
                .events()
                .iter()
                .all(|event| event.event_type != "step/start")
        );
        assert_eq!(
            session.events().last().expect("end").data["reason"]["kind"],
            "blocked"
        );
    }
}
