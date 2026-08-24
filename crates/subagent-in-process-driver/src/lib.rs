//! Shared one-shot in-process child creation, execution, and structured capture.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use futures::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use parking_lot::Mutex;
use seekdeep_agent::{
    AGENT, AGENTS, Agent, AgentEvent, AgentHandle, CancelOptions, CreateAgentOptions,
    PreStepDecision,
};
use seekdeep_agent_loop::AgentPreStepEvent;
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_core::session::{AgentCancelCause, AppendOptions, SessionEvent, SessionId};
use seekdeep_llm::{AbortSignal, ContentBlock, MessageSource, UserMessage};
use seekdeep_subagent::{
    ChildComposition, ResolvedSubagentStartRequest, SubagentDescriptorData, SubagentResult,
    SubagentRun, SubagentStopReason, append_delegated_policy_overrides, apply_child_composition,
    capture_delegated_policy_overrides, child_session_meta, final_assistant_output,
    resolve_child_agent_options, resolve_child_depth,
};
use seekdeep_system_prompt::{PromptSection, SYSTEM_PROMPT};
use seekdeep_tools::{
    TOOLS, ToolArgsError, ToolDefinition, ToolExecutionToken, ToolOutputDefinition, ToolRuntime,
    assert_object_json_schema, assert_supported_json_schema, validate_json_schema_value,
};
use serde_json::{Value, json};
use tokio::sync::OnceCell;
use uuid::Uuid;

/// Child-local tool required to complete a structured run.
pub const STRUCTURED_OUTPUT_TOOL: &str = "structured_output";
/// Child-local instruction paired with [`STRUCTURED_OUTPUT_TOOL`].
pub const STRUCTURED_OUTPUT_INSTRUCTION: &str = "When you have your final answer, you MUST report it by calling the `structured_output` tool with arguments matching its parameter schema exactly. Do not finish with a plain text answer: only the tool call counts as your result.";

/// Completed-turn seed for fork, or none for a fresh spawn.
#[derive(Clone, Debug, Default)]
pub struct InProcessRunOptions {
    /// Prefix copied into the child before its own activation.
    pub seed: Option<Vec<SessionEvent>>,
}

#[derive(Default)]
struct StructuredState {
    staged: std::collections::HashMap<ToolExecutionToken, Value>,
    pending: Option<(ToolExecutionToken, Value)>,
    captured: Option<Value>,
}

/// One structured run's live capture view.
#[derive(Clone)]
pub struct StructuredAttachment {
    state: Arc<Mutex<StructuredState>>,
}

impl StructuredAttachment {
    /// Returns the committed structured value, when one was accepted.
    #[must_use]
    pub fn captured(&self) -> Option<Value> {
        self.state.lock().captured.clone()
    }
}

/// Installs the child-scoped structured tool, instruction, guard, and commit observer.
///
/// # Errors
///
/// Returns unsupported-schema, missing-service, duplicate-tool, prompt, guard,
/// or observer-registration failures.
pub fn attach_structured_runtime(
    child_context: &Context,
    schema: Value,
) -> anyhow::Result<StructuredAttachment> {
    let schema = Arc::new(assert_object_json_schema(schema)?);
    let parameters = schema
        .as_value()
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("structured output schema lost its object root"))?;
    let tools = child_context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("structured child requires tools"))?;
    let prompt = child_context
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("structured child requires systemPrompt"))?;
    let state = Arc::new(Mutex::new(StructuredState::default()));
    tools.register(
        child_context,
        structured_definition(parameters, schema, &state)?,
    )?;
    prompt.section(
        child_context,
        PromptSection::new(
            format!("tool:{STRUCTURED_OUTPUT_TOOL}"),
            190.0,
            STRUCTURED_OUTPUT_INSTRUCTION,
        ),
    )?;
    install_structured_guard_and_commit(child_context, &tools, &state)?;
    Ok(StructuredAttachment { state })
}

fn structured_definition(
    parameters: serde_json::Map<String, Value>,
    schema: Arc<seekdeep_tools::ObjectJsonSchema>,
    state: &Arc<Mutex<StructuredState>>,
) -> anyhow::Result<ToolDefinition> {
    let execute_state = Arc::clone(state);
    let output = ToolOutputDefinition::new(
        Arc::new(assert_supported_json_schema(json!({
            "type": "object",
            "properties": { "recorded": { "type": "boolean", "const": true } },
            "required": ["recorded"],
            "additionalProperties": false
        }))?),
        Arc::new(|_arguments, _value| {
            Ok(vec![ContentBlock::Text {
                text: "Structured output recorded.".to_owned(),
            }])
        }),
    );
    let definition = ToolDefinition::new(
        STRUCTURED_OUTPUT_TOOL,
        "Report your final structured result. Call this exactly once, when your answer is complete; the arguments must match this tool's parameter schema exactly.",
        parameters,
        output,
        Arc::new(move |arguments, run| {
            let violations = validate_json_schema_value(schema.as_schema(), &arguments);
            if !violations.is_empty() {
                return Box::pin(
                    async move { Err(anyhow::Error::new(ToolArgsError::new(violations))) },
                );
            }
            execute_state
                .lock()
                .staged
                .insert(run.execution().token, arguments);
            run.conclude_turn();
            Box::pin(async { Ok(json!({ "recorded": true })) })
        }),
    );
    Ok(definition)
}

fn install_structured_guard_and_commit(
    child_context: &Context,
    tools: &ToolRuntime,
    state: &Arc<Mutex<StructuredState>>,
) -> anyhow::Result<()> {
    let guard_state = Arc::clone(state);
    tools.guard(
        child_context,
        Arc::new(move |execution| {
            let state = guard_state.lock();
            (state.captured.is_some() || state.pending.is_some()).then(|| {
                format!(
                    "structured output already recorded: the run is complete, so `{}` is not executed",
                    execution.name
                )
            })
        }),
    )?;
    let result_state = Arc::clone(state);
    tools.on_result(
        child_context,
        move |execution, result| {
            let mut state = result_state.lock();
            if execution.name == STRUCTURED_OUTPUT_TOOL {
                let Some(value) = state.staged.remove(&execution.token) else {
                    return Ok(());
                };
                if result.is_error() {
                    return Ok(());
                }
                if let Some(parent) = execution.parent {
                    if state.captured.is_none() && state.pending.is_none() {
                        state.pending = Some((parent, value));
                    }
                } else if state.captured.is_none() {
                    state.captured = Some(value);
                }
                return Ok(());
            }
            let Some((parent, _)) = state.pending.as_ref() else {
                return Ok(());
            };
            if *parent != execution.token {
                return Ok(());
            }
            let (_, value) = state.pending.take().expect("matched pending capture");
            if !result.is_error() && state.captured.is_none() {
                state.captured = Some(value);
            }
            Ok(())
        },
        EventOptions::default(),
    )?;
    Ok(())
}

fn attach_descriptor_append(
    child_context: &Context,
    descriptor: SubagentDescriptorData,
) -> anyhow::Result<()> {
    let appended = Arc::new(AtomicBool::new(false));
    child_context.events().on_waterfall(
        child_context,
        "agent/pre-step",
        move |_, args, next| {
            let appended = Arc::clone(&appended);
            let descriptor = descriptor.clone();
            let event = args.get::<AgentEvent<AgentPreStepEvent>>(0);
            Box::pin(async move {
                let event =
                    event.ok_or_else(|| anyhow::anyhow!("agent/pre-step is missing its event"))?;
                let reply = next.run().await?;
                let decision = reply
                    .downcast::<PreStepDecision>()
                    .map(|decision| (*decision).clone())
                    .ok_or_else(|| {
                        anyhow::anyhow!("agent/pre-step returned an invalid decision")
                    })?;
                if matches!(decision, PreStepDecision::Enter { .. })
                    && !appended.swap(true, Ordering::AcqRel)
                {
                    event.agent.session().append(
                        "subagent/descriptor",
                        serde_json::to_value(descriptor)?,
                        AppendOptions::default(),
                    )?;
                }
                Ok(EventReply::Value(Arc::new(decision)))
            })
        },
        EventOptions::default(),
    )?;
    Ok(())
}

type SharedResult = Shared<BoxFuture<'static, SubagentResult>>;

struct InProcessRun {
    id: SessionId,
    agent: Arc<Agent>,
    result: SharedResult,
    handle: Arc<Mutex<Option<AgentHandle>>>,
    cancelled: Arc<AtomicBool>,
    disposal: Arc<OnceCell<Result<(), String>>>,
}

impl SubagentRun for InProcessRun {
    fn id(&self) -> &SessionId {
        &self.id
    }

    fn local_agent(&self) -> Option<&Arc<Agent>> {
        Some(&self.agent)
    }

    fn result(&self) -> BoxFuture<'static, anyhow::Result<SubagentResult>> {
        Box::pin(self.result.clone().map(Ok))
    }

    fn dispose(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        let result = self.result.clone();
        let handle = Arc::clone(&self.handle);
        let cancelled = Arc::clone(&self.cancelled);
        let disposal = Arc::clone(&self.disposal);
        Box::pin(async move {
            let outcome = disposal
                .get_or_init(|| async move {
                    cancelled.store(true, Ordering::Release);
                    let Some(handle) = handle.lock().take() else {
                        let _ = result.await;
                        return Ok(());
                    };
                    let (released, _) = futures::join!(handle.dispose(), result);
                    released.map_err(|error| format!("{error:#}"))
                })
                .await
                .clone();
            outcome.map_err(|error| anyhow::anyhow!(error))
        })
    }
}

/// Creates, publishes, drives, and returns one holder-owned one-shot child run.
///
/// # Errors
///
/// Returns depth, cancellation, setup, schema, factory, or publication failures.
pub async fn start_in_process_run(
    request: ResolvedSubagentStartRequest,
    options: InProcessRunOptions,
) -> anyhow::Result<Arc<dyn SubagentRun>> {
    if request.request.signal.is_aborted() {
        anyhow::bail!("subagent request was aborted before child publication");
    }
    let parent = request.request.parent.clone();
    let child_depth = resolve_child_depth(&parent, request.request.max_depth)?;
    let child_id = SessionId::new(Uuid::new_v4().to_string());
    let seed = options.seed;
    let boundary = seed.as_ref().map_or(0, Vec::len);
    let inherited = capture_delegated_policy_overrides(&parent);
    let composition = ChildComposition {
        persona: request.request.persona.clone(),
        tool_filter: request.request.tool_filter.clone(),
    };
    let descriptor = request.descriptor.clone();
    let output_schema = request.request.output_schema.clone();
    let structured_slot = Arc::new(Mutex::new(None));
    let setup_slot = Arc::clone(&structured_slot);
    let setup = Arc::new(move |child_context: Context| {
        let inherited = inherited.clone();
        let composition = composition.clone();
        let descriptor = descriptor.clone();
        let output_schema = output_schema.clone();
        let setup_slot = Arc::clone(&setup_slot);
        Box::pin(async move {
            let child = child_context
                .get(AGENT)
                .ok_or_else(|| anyhow::anyhow!("subagent setup requires its child agent"))?;
            append_delegated_policy_overrides(child.session(), &inherited)?;
            apply_child_composition(&child_context, &composition)?;
            if let Some(schema) = output_schema {
                *setup_slot.lock() = Some(attach_structured_runtime(&child_context, schema)?);
            }
            attach_descriptor_append(&child_context, descriptor)?;
            Ok(None)
        })
            as BoxFuture<'static, anyhow::Result<Option<Arc<dyn seekdeep_agent::AgentSetupCommit>>>>
    });
    let agents = parent
        .context()
        .get(AGENTS)
        .ok_or_else(|| anyhow::anyhow!("in-process subagent requires agents"))?;
    let mut create = CreateAgentOptions::new(child_id.clone());
    create.meta = child_session_meta(&parent, child_depth, boundary as u64);
    create.seed = seed;
    create.agent_options =
        resolve_child_agent_options(&parent, request.request.agent_options.clone(), child_depth);
    create.signal = Some(request.request.signal.clone());
    create.setup = Some(setup);
    create.owner_agent = Some(parent);
    let handle = agents.create(create).await?;
    let structured = structured_slot.lock().clone();
    Ok(drive_published_run(
        handle,
        request.request.signal,
        request.request.prompt,
        child_id,
        boundary,
        structured,
    ))
}

fn drive_published_run(
    handle: AgentHandle,
    signal: AbortSignal,
    prompt: Vec<ContentBlock>,
    child_id: SessionId,
    boundary: usize,
    structured: Option<StructuredAttachment>,
) -> Arc<dyn SubagentRun> {
    let child = handle.agent.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let result_child = child.clone();
    let result_cancelled = Arc::clone(&cancelled);
    let result = async move {
        if signal.is_aborted() {
            cancel_child(&result_child, &result_cancelled).await;
        } else {
            let message = UserMessage::new(prompt, MessageSource::user());
            if result_child.followup(message).is_err() {
                return read_result(
                    &result_child,
                    boundary,
                    result_cancelled.load(Ordering::Acquire),
                    structured.as_ref(),
                );
            }
            let wait = result_child.when_idle();
            tokio::select! {
                () = signal.cancelled() => cancel_child(&result_child, &result_cancelled).await,
                () = async {
                    if let Ok(wait) = wait {
                        let _ = wait.await;
                    }
                } => {}
            }
        }
        read_result(
            &result_child,
            boundary,
            result_cancelled.load(Ordering::Acquire),
            structured.as_ref(),
        )
    }
    .boxed()
    .shared();
    Arc::new(InProcessRun {
        id: child_id,
        agent: child,
        result,
        handle: Arc::new(Mutex::new(Some(handle))),
        cancelled,
        disposal: Arc::new(OnceCell::new()),
    })
}

async fn cancel_child(child: &Arc<Agent>, cancelled: &AtomicBool) {
    cancelled.store(true, Ordering::Release);
    let _ = child.cancel(AgentCancelCause::Parent, CancelOptions::default());
    if let Ok(wait) = child.when_idle() {
        let _ = wait.await;
    }
}

fn read_result(
    child: &Agent,
    boundary: usize,
    cancelled: bool,
    structured: Option<&StructuredAttachment>,
) -> SubagentResult {
    let events = child.session().events();
    let own = &events[boundary.min(events.len())..];
    let consumed = seekdeep_agent::fold_consumed_work(own);
    let recorded = match consumed.end.as_ref().and_then(|event| {
        event
            .data
            .get("reason")
            .and_then(|reason| reason.get("kind"))
            .and_then(Value::as_str)
    }) {
        Some("completed") => SubagentStopReason::Completed,
        Some("max-tokens") => SubagentStopReason::MaxTokens,
        Some("aborted") => SubagentStopReason::Aborted,
        Some("blocked") => SubagentStopReason::Refusal,
        None | Some("error" | "interrupted" | _) => SubagentStopReason::Error,
    };
    let mut stop_reason = if cancelled && recorded != SubagentStopReason::Completed {
        SubagentStopReason::Aborted
    } else {
        recorded
    };
    let output = final_assistant_output(own).unwrap_or_default();
    let captured = structured.and_then(StructuredAttachment::captured);
    if structured.is_some() && captured.is_none() && stop_reason == SubagentStopReason::Completed {
        stop_reason = if cancelled {
            SubagentStopReason::Aborted
        } else {
            SubagentStopReason::Error
        };
    }
    SubagentResult {
        output,
        structured: captured,
        stop_reason,
    }
}
