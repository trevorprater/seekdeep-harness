//! Provider-bound model-facing subagent delegation.

use std::sync::Arc;

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions};
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin, fiber::EffectHandle};
use seekdeep_jobs::{JOBS, JobHooks, JobOutcome, JobStart, JobTerminalStatus};
use seekdeep_llm::{ContentBlock, ModelId, ProviderId};
use seekdeep_subagent::{
    ContinuableStartRequest, ContinuableStartSpec, SUBAGENTS, SubagentProvider, SubagentResult,
    SubagentRun, SubagentRuntime, SubagentStopReason,
};
use seekdeep_system_prompt::{PromptSection, PromptText, SYSTEM_PROMPT};
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, TOOLS, ToolDefinition, ToolRestriction, ToolRuntime,
    define_tool,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Loader plugin name.
pub const NAME: &str = "tool-subagent";
/// Required services.
pub const INJECT: &[&str] = &["tools", "subagents", "systemPrompt"];
/// Prompt order after delegation policy and before child reporting.
pub const SUBAGENT_SECTION_ORDER: f64 = 116.5;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Background execution policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackgroundMode {
    /// A foreground result or a collectable one-shot job.
    #[default]
    OneShot,
    /// A durable conversation that defaults to background execution.
    Continuable,
}

/// Literal branch of [`MaxDepth`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderManaged {
    /// The provider owns its recursion budget.
    #[serde(rename = "provider-managed")]
    Value,
}

/// Numeric Harness depth policy or a provider-owned policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MaxDepth {
    /// Absolute non-negative delegation depth.
    Numeric(u64),
    /// No Harness depth cap is sent.
    ProviderManaged(ProviderManaged),
}

/// Child model defaults accepted by this tool configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigAgentOptions {
    /// Child provider override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Child model override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Per-request output token ceiling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
}

impl From<ConfigAgentOptions> for AgentOptions {
    fn from(value: ConfigAgentOptions) -> Self {
        Self {
            provider: value.provider.map(ProviderId::new),
            model: value.model.map(ModelId::new),
            max_tokens: value.max_tokens,
            subagent_depth: None,
        }
    }
}

/// Provider selection and child defaults for one delegation tool instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Registered subagent provider name.
    pub provider: String,
    /// Model-facing tool name; direct application defaults to `subagent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Whether the model may request background execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_run_in_background: Option<bool>,
    /// One-shot or continuable background policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_mode: Option<BackgroundMode>,
    /// Child model defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_options: Option<ConfigAgentOptions>,
    /// Per-child persona override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// Child tool visibility restriction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_filter: Option<ToolRestriction>,
    /// Harness depth limit or provider-owned policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<MaxDepth>,
}

#[derive(Debug, Deserialize)]
struct DelegationArgs {
    description: String,
    prompt: String,
    #[serde(default)]
    run_in_background: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
enum DelegationValue {
    Background {
        job_id: seekdeep_jobs::JobId,
    },
    Continuable {
        subagent_id: seekdeep_core::session::SessionId,
    },
    Foreground {
        run_id: seekdeep_core::session::SessionId,
        output: Vec<Value>,
    },
}

struct Wording {
    description: &'static str,
    prompt_description: &'static str,
}

fn provider_wording(inherits_conversation: bool) -> Wording {
    if inherits_conversation {
        return Wording {
            description: "Delegate a task to a subagent that inherits this conversation: a child agent seeded with all completed turns so far (it does not see the current in-flight turn). Use this when the subtask builds on this conversation's context — a follow-up analysis, a review, a continuation — without consuming this conversation's context for the work itself. You receive its result, not its intermediate steps.",
            prompt_description: "The task for the subagent. It already sees this conversation's completed turns, so build on them freely and state only what is new.",
        };
    }
    Wording {
        description: "Delegate a self-contained task to a subagent (a separate agent that works in its own context) to offload focused, independent work — research, a scoped implementation, an analysis — so it does not consume this conversation's context. The subagent returns its result, not its intermediate steps. Give it a complete, standalone prompt: it does not see this conversation.",
        prompt_description: "The complete, self-contained task for the subagent. It does not share this conversation's context, so include everything it needs.",
    }
}

fn output_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "required": true, "const": "background" },
                    "jobId": { "type": "string", "required": true }
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "required": true, "const": "continuable" },
                    "subagentId": { "type": "string", "required": true }
                }
            },
            {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "kind": { "type": "string", "required": true, "const": "foreground" },
                    "runId": { "type": "string", "required": true },
                    "output": { "type": "array", "required": true, "items": { "type": "json" } }
                }
            }
        ]
    })
}

fn output_value_text(values: &[Value]) -> String {
    values
        .iter()
        .filter_map(|value| {
            let object = value.as_object()?;
            (object.get("type")?.as_str()? == "text")
                .then(|| object.get("text")?.as_str().map(str::to_owned))?
        })
        .collect()
}

fn stop_reason_error(result: &SubagentResult) -> Option<&'static str> {
    match result.stop_reason {
        SubagentStopReason::Completed => None,
        SubagentStopReason::Aborted => Some("subagent run was cancelled"),
        SubagentStopReason::Error => Some("subagent run failed"),
        SubagentStopReason::MaxTokens => Some("subagent run hit its token limit before finishing"),
        SubagentStopReason::Refusal => Some("subagent declined the task"),
    }
}

fn partial_text_error(headline: &str, output: &[ContentBlock]) -> String {
    let text = output
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    if text.is_empty() {
        headline.to_owned()
    } else {
        format!("{headline}\nPartial output before the run ended:\n{text}")
    }
}

async fn settle_foreground(run: Arc<dyn SubagentRun>) -> anyhow::Result<DelegationValue> {
    let result = run.result().await;
    let execution = if let Some(error) = stop_reason_error(&result) {
        Err(anyhow::anyhow!(partial_text_error(error, &result.output)))
    } else {
        let output = result
            .output
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(DelegationValue::Foreground {
            run_id: run.id().clone(),
            output,
        })
    };
    let disposal = run.dispose().await;
    match (execution, disposal) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(disposal)) => Err(disposal),
        (Err(error), Err(disposal)) => Err(anyhow::anyhow!(
            "subagent run failed: {error}; dispose failed: {disposal}"
        )),
    }
}

fn map_job_outcome(outcome: seekdeep_subagent::JobOutcome) -> JobOutcome {
    match outcome {
        seekdeep_subagent::JobOutcome::Completed { output } => JobOutcome {
            status: JobTerminalStatus::Completed,
            detail: None,
            output: Some(output),
        },
        seekdeep_subagent::JobOutcome::Killed => JobOutcome {
            status: JobTerminalStatus::Killed,
            detail: None,
            output: None,
        },
        seekdeep_subagent::JobOutcome::Failed { detail } => JobOutcome {
            status: JobTerminalStatus::Failed,
            detail,
            output: None,
        },
    }
}

struct BackgroundSubagentHooks {
    signal: seekdeep_llm::AbortSignal,
    done: Mutex<Option<BoxFuture<'static, anyhow::Result<JobOutcome>>>>,
}

impl JobHooks for BackgroundSubagentHooks {
    fn cancel(&self, reason: Option<&str>) {
        self.signal.abort_with_reason(Value::String(
            reason
                .unwrap_or("background subagent task killed")
                .to_owned(),
        ));
    }

    fn done(&self) -> BoxFuture<'static, anyhow::Result<JobOutcome>> {
        self.done.lock().take().unwrap_or_else(|| {
            Box::pin(async { Err(anyhow::anyhow!("subagent job done was already consumed")) })
        })
    }
}

async fn settle_background_start(
    start: BoxFuture<'static, anyhow::Result<Arc<dyn SubagentRun>>>,
    signal: seekdeep_llm::AbortSignal,
) -> anyhow::Result<JobOutcome> {
    match start.await {
        Ok(run) => Ok(map_job_outcome(seekdeep_subagent::settle_run(&run).await)),
        Err(error) if signal.is_aborted() => Ok(JobOutcome {
            status: JobTerminalStatus::Killed,
            detail: None,
            output: None,
        }),
        Err(error) => Ok(JobOutcome {
            status: JobTerminalStatus::Failed,
            detail: Some(error.to_string()),
            output: None,
        }),
    }
}

struct DelegationMount {
    context: Context,
    config: Config,
    tools: Arc<ToolRuntime>,
    subagents: Arc<SubagentRuntime>,
    tool_effect: Mutex<Option<EffectHandle>>,
}

impl DelegationMount {
    fn tool_name(&self) -> &str {
        self.config.tool_name.as_deref().unwrap_or("subagent")
    }

    fn background_enabled(&self) -> bool {
        self.config.enable_run_in_background != Some(false)
    }

    fn continuable(&self) -> bool {
        self.config.background_mode == Some(BackgroundMode::Continuable)
    }

    fn is_mounted(&self) -> bool {
        self.tool_effect.lock().is_some()
    }

    fn mount(self: &Arc<Self>, provider: &Arc<dyn SubagentProvider>) -> anyhow::Result<()> {
        if self.tool_effect.lock().is_some() {
            return Ok(());
        }
        if matches!(self.config.max_depth, Some(MaxDepth::Numeric(_)))
            && !provider.capabilities().depth_limit
        {
            anyhow::bail!(
                "tool-subagent: provider \"{}\" cannot enforce maxDepth (no depthLimit capability) — set maxDepth: 'provider-managed' to leave the recursion budget to the provider",
                provider.name()
            );
        }
        if self.continuable() && !provider.supports_continuable() {
            anyhow::bail!(
                "tool-subagent: provider \"{}\" does not support `backgroundMode: continuable`",
                provider.name()
            );
        }
        let definition = self.definition(provider.inherits_parent_context())?;
        let effect = self.tools.register(&self.context, definition)?;
        *self.tool_effect.lock() = Some(effect);
        Ok(())
    }

    fn unmount(&self) -> anyhow::Result<()> {
        let Some(effect) = self.tool_effect.lock().take() else {
            return Ok(());
        };
        futures::executor::block_on(effect.dispose())
    }

    fn definition(self: &Arc<Self>, inherits: bool) -> anyhow::Result<ToolDefinition> {
        let wording = provider_wording(inherits);
        let description = format!(
            "{}{}",
            wording.description,
            if self.background_enabled() {
                if self.continuable() {
                    " This tool runs in the background by default, immediately returns a durable subagent id, and keeps the child conversation available for later turns. When that run settles, the runtime sends the parent a notice containing its outcome and any final assistant message; `send_message` starts a later turn in the same child conversation. Set `run_in_background: false` only when your next action depends on receiving the result."
                } else {
                    " This call waits for the result by default. Set `run_in_background: true` to return a job id; collect with `job_output` and stop with `job_kill`."
                }
            } else {
                " This call waits for the subagent and returns its result."
            }
        );
        let mut parameters = serde_json::Map::from_iter([
            (
                "description".to_owned(),
                json!({
                    "type": "string",
                    "required": true,
                    "description": "A short (3-5 word) description of the delegated task, for display."
                }),
            ),
            (
                "prompt".to_owned(),
                json!({
                    "type": "string",
                    "required": true,
                    "description": wording.prompt_description
                }),
            ),
        ]);
        if self.background_enabled() {
            parameters.insert(
                "run_in_background".to_owned(),
                json!({
                    "type": "boolean",
                    "description": if self.continuable() {
                        "Whether to run in the background and return a durable subagent id immediately. Defaults to true. Set false to wait for the result when your next action depends on it."
                    } else {
                        "Whether to run as a background job and return its id. Defaults to false; collect with job_output or stop with job_kill."
                    }
                }),
            );
        }
        let state = Arc::clone(self);
        let mut options = DefineToolOptions::new(
            self.tool_name(),
            description,
            Value::Object(parameters),
            DefineToolOutput::new(
                output_schema(),
                Arc::new(|_args: &DelegationArgs, value: &DelegationValue| {
                    let text = match value {
                        DelegationValue::Background { job_id } => {
                            format!("started background subagent task {job_id}")
                        }
                        DelegationValue::Continuable { subagent_id } => {
                            format!("started subagent {subagent_id}")
                        }
                        DelegationValue::Foreground { output, .. } => output_value_text(output),
                    };
                    Ok(vec![ContentBlock::Text { text }])
                }),
            ),
            Arc::new(move |args: DelegationArgs, run| {
                let state = Arc::clone(&state);
                Box::pin(async move { state.execute(args, run).await })
            }),
        );
        options.is_concurrency_safe = Some(Arc::new(|_args| true));
        define_tool(options)
    }

    async fn execute(
        self: Arc<Self>,
        args: DelegationArgs,
        run: seekdeep_tools::ToolRunContext,
    ) -> anyhow::Result<DelegationValue> {
        let parent = run.execution().agent.clone().ok_or_else(|| {
            anyhow::anyhow!("subagent tool requires a calling agent (exec.agent was undefined)")
        })?;
        let background = if self.background_enabled() {
            args.run_in_background.unwrap_or_else(|| self.continuable())
        } else {
            if args.run_in_background == Some(true) {
                anyhow::bail!(
                    "run_in_background is disabled for this tool instance (enableRunInBackground: false)"
                );
            }
            false
        };
        if background && self.continuable() {
            let started = self
                .subagents
                .start_continuable(ContinuableStartSpec {
                    provider: self.config.provider.clone(),
                    label: args.description,
                    request: self.continuable_request(args.prompt, parent),
                    signal: run.signal(),
                })
                .await?;
            return Ok(DelegationValue::Continuable {
                subagent_id: started.child_id,
            });
        }
        if background {
            return self.start_background(args, parent);
        }
        let request = self.one_shot_request(args, parent, run.signal());
        let child = self.subagents.start(&self.config.provider, request).await?;
        settle_foreground(child).await
    }

    fn one_shot_request(
        &self,
        args: DelegationArgs,
        parent: Arc<Agent>,
        signal: seekdeep_llm::AbortSignal,
    ) -> seekdeep_subagent::SubagentStartRequest {
        seekdeep_subagent::SubagentStartRequest {
            label: Some(args.description),
            prompt: vec![ContentBlock::Text { text: args.prompt }],
            parent,
            signal,
            agent_options: self.config.agent_options.clone().map(AgentOptions::from),
            output_schema: None,
            max_depth: self.numeric_max_depth(),
            tool_filter: self.config.tool_filter.clone(),
            persona: self.config.persona.clone(),
        }
    }

    fn continuable_request(&self, prompt: String, parent: Arc<Agent>) -> ContinuableStartRequest {
        ContinuableStartRequest {
            prompt: vec![ContentBlock::Text { text: prompt }],
            parent,
            agent_options: self.config.agent_options.clone().map(AgentOptions::from),
            max_depth: self.numeric_max_depth(),
            tool_filter: self.config.tool_filter.clone(),
            persona: self.config.persona.clone(),
        }
    }

    fn numeric_max_depth(&self) -> Option<u64> {
        match self.config.max_depth {
            Some(MaxDepth::Numeric(value)) => Some(value),
            None | Some(MaxDepth::ProviderManaged(_)) => None,
        }
    }

    fn start_background(
        self: &Arc<Self>,
        args: DelegationArgs,
        parent: Arc<Agent>,
    ) -> anyhow::Result<DelegationValue> {
        let jobs = self.context.get(JOBS).ok_or_else(|| {
            anyhow::anyhow!(
                "background jobs unavailable: load @seekdeep-ai/seekdeep-jobs and @seekdeep-ai/seekdeep-tool-jobs"
            )
        })?;
        let state = Arc::clone(self);
        let label = args.description.clone();
        let id = jobs.start(JobStart {
            kind: "subagent".to_owned(),
            label,
            output_limit_bytes: None,
            owner: Some(parent.clone()),
            run: Box::new(move || {
                let signal = seekdeep_llm::AbortSignal::default();
                let request = state.one_shot_request(args, parent, signal.clone());
                let subagents = Arc::clone(&state.subagents);
                let provider = state.config.provider.clone();
                let start_signal = signal.clone();
                let start: BoxFuture<'static, anyhow::Result<Arc<dyn SubagentRun>>> =
                    Box::pin(async move { subagents.start(&provider, request).await });
                Box::new(BackgroundSubagentHooks {
                    signal,
                    done: Mutex::new(Some(Box::pin(settle_background_start(start, start_signal)))),
                })
            }),
        });
        Ok(DelegationValue::Background { job_id: id })
    }
}

fn validate_direct_config(config: &Config) -> anyhow::Result<()> {
    if let Some(MaxDepth::Numeric(depth)) = config.max_depth {
        anyhow::ensure!(
            depth <= MAX_SAFE_INTEGER,
            "maxDepth must be a non-negative safe integer"
        );
    }
    if let Some(filter) = &config.tool_filter {
        anyhow::ensure!(
            filter.allow.is_some() || filter.deny.is_some(),
            "tool-subagent: `toolFilter` is configured but names neither `allow` nor `deny` — remove the key or fill the filter"
        );
    }
    Ok(())
}

fn normalized_config(value: &Value) -> anyhow::Result<Value> {
    let mut config: Config = serde_json::from_value(value.clone())?;
    if config.tool_name.is_none() {
        config.tool_name = Some("subagent".to_owned());
    }
    if config.enable_run_in_background.is_none() {
        config.enable_run_in_background = Some(true);
    }
    if config.background_mode.is_none() {
        config.background_mode = Some(BackgroundMode::OneShot);
    }
    if config.max_depth.is_none() {
        config.max_depth = Some(MaxDepth::Numeric(3));
    }
    if let Some(max_tokens) = config
        .agent_options
        .as_ref()
        .and_then(|options| options.max_tokens)
    {
        anyhow::ensure!(
            (1..=MAX_SAFE_INTEGER).contains(&max_tokens),
            "agentOptions.maxTokens must be a positive safe integer"
        );
    }
    validate_direct_config(&config)?;
    Ok(serde_json::to_value(config)?)
}

fn rollback_effects(effects: Vec<EffectHandle>, primary: anyhow::Error) -> anyhow::Error {
    let mut failures = Vec::new();
    for effect in effects.into_iter().rev() {
        if let Err(error) = futures::executor::block_on(effect.dispose()) {
            failures.push(error.to_string());
        }
    }
    if failures.is_empty() {
        primary
    } else {
        anyhow::anyhow!("{primary:#}; rollback failed: {}", failures.join("; "))
    }
}

fn install_provider_listeners(
    context: &Context,
    state: &Arc<DelegationMount>,
) -> anyhow::Result<Vec<EffectHandle>> {
    let mut effects = Vec::new();
    let added_state = Arc::clone(state);
    let added = context.events().on(
        context,
        "subagent/provider-added",
        move |_dispatch, args| {
            let state = Arc::clone(&added_state);
            Box::pin(async move {
                let provider = args
                    .get::<Arc<dyn SubagentProvider>>(0)
                    .ok_or_else(|| anyhow::anyhow!("subagent/provider-added lacks its provider"))?;
                if provider.name() == state.config.provider && !state.is_mounted() {
                    state.mount(provider.as_ref())?;
                }
                Ok(EventReply::Undefined)
            })
        },
        EventOptions::default(),
    )?;
    effects.push(added);
    let removed_state = Arc::clone(state);
    match context.events().on(
        context,
        "subagent/provider-removed",
        move |_dispatch, args| {
            let state = Arc::clone(&removed_state);
            Box::pin(async move {
                let name = args
                    .get::<String>(0)
                    .ok_or_else(|| anyhow::anyhow!("subagent/provider-removed lacks its name"))?;
                if name.as_str() == state.config.provider && state.is_mounted() {
                    state.unmount()?;
                }
                Ok(EventReply::Undefined)
            })
        },
        EventOptions::default(),
    ) {
        Ok(effect) => effects.push(effect),
        Err(error) => return Err(rollback_effects(effects, error.into())),
    }
    Ok(effects)
}

fn install_continuable_guidance(
    context: &Context,
    prompt: &seekdeep_system_prompt::SystemPrompt,
    state: &Arc<DelegationMount>,
) -> anyhow::Result<Option<EffectHandle>> {
    if !state.background_enabled() || !state.continuable() {
        return Ok(None);
    }
    let prompt_state = Arc::clone(state);
    let tool_name = state.tool_name().to_owned();
    let text_tool_name = tool_name.clone();
    let section = PromptSection::new(
        format!("tool:{tool_name}"),
        SUBAGENT_SECTION_ORDER,
        PromptText::Dynamic(Arc::new(move |assembly| {
            if !prompt_state.is_mounted()
                || prompt_state
                    .tools
                    .get(&text_tool_name, assembly.scope)
                    .is_none()
            {
                return Ok(String::new());
            }
            Ok(format!(
                "Use {text_tool_name} in the background by default. Start independent delegations together in one assistant message and continue useful work while they run. Set `run_in_background: false` only when your next action depends on that subagent's result. When a background run settles, the runtime sends you a notice containing its outcome and any final assistant message."
            ))
        })),
    );
    prompt.section(context, section).map(Some)
}

/// Applies one provider-bound delegation tool instance.
///
/// Direct application preserves omitted `max_depth` as capless; the loader
/// validator supplies its source default of three.
///
/// # Errors
///
/// Returns configuration, missing-service, provider-capability, schema, or
/// duplicate-registration failures.
pub fn apply(context: &Context, config: Config) -> anyhow::Result<()> {
    validate_direct_config(&config)?;
    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-subagent requires tools"))?;
    let subagents = context
        .get(SUBAGENTS)
        .ok_or_else(|| anyhow::anyhow!("tool-subagent requires subagents"))?;
    let prompt = context
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("tool-subagent requires systemPrompt"))?;
    let state = Arc::new(DelegationMount {
        context: context.clone(),
        config,
        tools,
        subagents,
        tool_effect: Mutex::new(None),
    });
    let mut effects = install_provider_listeners(context, &state)?;
    if let Some(provider) = state.subagents.get_provider(&state.config.provider) {
        if let Err(error) = state.mount(&provider) {
            return Err(rollback_effects(effects, error));
        }
    } else {
        tracing::info!(
            provider = %state.config.provider,
            tool = state.tool_name(),
            "subagent provider is not registered yet; the tool will register when it appears"
        );
    }
    match install_continuable_guidance(context, &prompt, &state) {
        Ok(Some(effect)) => effects.push(effect),
        Ok(None) => {}
        Err(error) => {
            let primary = match state.unmount() {
                Ok(()) => error,
                Err(cleanup) => {
                    anyhow::anyhow!("{error:#}; tool rollback failed: {cleanup:#}")
                }
            };
            return Err(rollback_effects(effects, primary));
        }
    }
    Ok(())
}

/// Builds the Loader-compatible delegation plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(config)?;
            apply(&context, config)
        })
    })
    .with_config_validator(normalized_config)
}
