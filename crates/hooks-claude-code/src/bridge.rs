//! Claude Code dialect payloads and seven-point interception bridge.

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use parking_lot::Mutex;
use seekdeep_agent::{AGENTS, Agent, AgentEvent, PreStepDecision};
use seekdeep_agent_loop::{AgentPreStepEvent, AgentTurnStoppingEvent, SessionStartEvent};
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin, fiber::EffectHandle};
use seekdeep_hook_protocol::{
    DetachedRuns, HookDialect, HookInvocation, HookResultRecord, MatcherMode, MergedDecision,
    MergedHookOutcome, RunHookOptions, append_hook_invoked, append_hook_result,
    create_detached_runs, matches_matcher, merge_hook_outputs, run_hook,
};
use seekdeep_llm::{AbortSignal, ContentBlock, MessageSource, UserMessage};
use seekdeep_schemastery::Schema;
use seekdeep_session_persistence::SESSION_PERSISTENCE;
use seekdeep_shell::{SHELL, ShellService};
use seekdeep_subagent::{SubagentRunEndInfo, SubagentRunId, SubagentRunInfo};
use seekdeep_tools::{PostToolDecision, PreToolDecision, ToolExecution, ToolExecutionResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{ClaudeCodeHookConfig, SubstitutionVars, parse_claude_code_config};

/// Cordis plugin name.
pub const NAME: &str = "hooks-claude-code";
/// Required provider-neutral shell capability.
pub const INJECT: &[&str] = &["shell"];
const SUBAGENT_TYPE: &str = "general-purpose";
static HANDLER_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Claude Code hook bridge configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Process-level hooks JSON/settings path.
    pub config_path: String,
    /// Command substitution root.
    #[serde(default)]
    pub plugin_root: Option<String>,
    /// Explicit command substitution and environment project root.
    #[serde(default)]
    pub project_dir: Option<String>,
    /// Default timeout in milliseconds.
    #[serde(default = "default_timeout_ms")]
    pub default_timeout_ms: f64,
    /// Persisted stderr summary character cap.
    #[serde(default = "default_stderr_cap")]
    pub stderr_summary_max_chars: f64,
}

const fn default_timeout_ms() -> f64 {
    600_000.0
}

const fn default_stderr_cap() -> f64 {
    500.0
}

/// Source-compatible Loader configuration schema.
#[must_use]
pub fn config_schema() -> Schema {
    Schema::object([
        ("configPath", Schema::string().required()),
        ("pluginRoot", Schema::string()),
        ("projectDir", Schema::string()),
        ("defaultTimeoutMs", Schema::number().with_default(600_000.0)),
        (
            "stderrSummaryMaxChars",
            Schema::number().with_default(500.0),
        ),
    ])
}

/// Loader-facing Cordis plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, value| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(value)?;
            apply(&context, &config)
        })
    })
    .with_config_validator(|value| {
        config_schema()
            .resolve(value)
            .map_err(|error| anyhow::anyhow!("{error}"))
    })
}

struct Bridge {
    context: Context,
    shell: Arc<ShellService>,
    config: Config,
    hooks: ClaudeCodeHookConfig,
    detached: DetachedRuns,
    stderr_summary_max_chars: usize,
    subagent_children: Mutex<HashMap<SubagentRunId, Arc<Agent>>>,
}

struct RunPointOptions<'a> {
    agent: Option<&'a Arc<Agent>>,
    turn: Option<u64>,
    signal: AbortSignal,
}

impl Bridge {
    async fn run_point(
        &self,
        point: &str,
        match_query: &str,
        payload: Value,
        options: RunPointOptions<'_>,
    ) -> anyhow::Result<MergedHookOutcome> {
        let groups = self.hooks.get(point).cloned().unwrap_or_default();
        let mut outputs = Vec::new();
        let workdir = options
            .agent
            .and_then(|agent| agent.session().header().cwd.clone());
        let project_dir = self.config.project_dir.clone().or_else(|| workdir.clone());
        let env =
            project_dir.map(|project| BTreeMap::from([("CLAUDE_PROJECT_DIR".to_owned(), project)]));
        for group in groups {
            if !matches_matcher(
                group.matcher.as_deref(),
                match_query,
                MatcherMode::ClaudeCode,
            ) {
                continue;
            }
            for hook in group.hooks {
                let handler_id = next_handler_id(point);
                if let (Some(agent), Some(turn)) = (options.agent, options.turn) {
                    append_hook_invoked(
                        agent.session(),
                        &HookInvocation {
                            turn,
                            point: point.to_owned(),
                            dialect: HookDialect::ClaudeCode,
                            handler_id: handler_id.clone(),
                            matcher: group.matcher.clone(),
                        },
                    )?;
                }
                let result = run_hook(
                    &**self.shell,
                    &hook,
                    &RunHookOptions {
                        payload: payload.clone(),
                        env: env.clone(),
                        cwd: workdir.clone(),
                        signal: options.signal.clone(),
                        trailing_newline: true,
                        default_timeout_ms: self.config.default_timeout_ms,
                        expected_event_name: Some(point.to_owned()),
                    },
                    elapsed_clock(),
                )
                .await;
                if result.output.updated_input.is_some() {
                    warn(
                        &self.context,
                        format!(
                            "hooks-claude-code: {point} hook requested updatedInput, which is not yet honored (ignored)"
                        ),
                    );
                }
                if result.output.system_message.is_some() {
                    warn(
                        &self.context,
                        format!(
                            "hooks-claude-code: {point} hook emitted a systemMessage, which is not yet surfaced (ignored)"
                        ),
                    );
                }
                if let (Some(agent), Some(turn)) = (options.agent, options.turn) {
                    append_hook_result(
                        agent.session(),
                        &HookResultRecord {
                            turn,
                            point: point.to_owned(),
                            handler_id,
                            output: result.output.clone(),
                            stderr_summary_max_chars: self.stderr_summary_max_chars,
                            duration_ms: result.duration_ms,
                        },
                    )?;
                }
                outputs.push(result.output);
            }
        }
        Ok(merge_hook_outputs(&outputs))
    }

    fn base(&self, agent: Option<&Arc<Agent>>, event: &str) -> Value {
        let cwd = agent
            .and_then(|agent| agent.session().header().cwd.clone())
            .unwrap_or_else(process_cwd);
        let transcript_path = agent
            .and_then(|agent| {
                self.context
                    .get(SESSION_PERSISTENCE)
                    .and_then(|service| service.persistence().locate(agent.session().header()))
                    .map(|location| location.path.to_string_lossy().into_owned())
            })
            .unwrap_or_default();
        json!({
            "session_id": agent.map_or("", |agent| agent.session().id().as_str()),
            "transcript_path": transcript_path,
            "cwd": cwd,
            "hook_event_name": event,
        })
    }
}

/// Installs the seven-point Claude Code compatibility bridge.
///
/// # Errors
///
/// Returns invalid summary-cap, missing shell, listener, or lifecycle failures.
#[allow(clippy::too_many_lines)]
pub fn apply(context: &Context, config: &Config) -> anyhow::Result<()> {
    anyhow::ensure!(
        config.stderr_summary_max_chars.is_finite()
            && config.stderr_summary_max_chars.fract() == 0.0
            && config.stderr_summary_max_chars >= 1.0,
        "hooks-claude-code: stderrSummaryMaxChars must be a positive integer"
    );
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let stderr_summary_max_chars = config.stderr_summary_max_chars as usize;
    let parsed = match std::fs::read_to_string(&config.config_path)
        .and_then(|content| {
            serde_json::from_str::<Value>(&content)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })
        .and_then(|value| {
            parse_claude_code_config(
                &value,
                &SubstitutionVars {
                    plugin_root: config.plugin_root.clone(),
                    project_dir: config.project_dir.clone(),
                },
            )
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        }) {
        Ok(parsed) => parsed,
        Err(error) => {
            warn(
                context,
                format!(
                    "hooks-claude-code: could not load hook config {:?}: {error} — no hooks registered",
                    config.config_path
                ),
            );
            return Ok(());
        }
    };
    for skipped in &parsed.skipped {
        warn(
            context,
            format!(
                "hooks-claude-code: skipping unsupported \"{}\" hook on {} (only command hooks run)",
                skipped.hook_type, skipped.event
            ),
        );
    }
    let shell = context
        .get(SHELL)
        .ok_or_else(|| anyhow::anyhow!("hooks-claude-code requires shell"))?;
    let bridge = Arc::new(Bridge {
        context: context.clone(),
        shell,
        config: config.clone(),
        hooks: parsed.config,
        detached: create_detached_runs(),
        stderr_summary_max_chars,
        subagent_children: Mutex::new(HashMap::new()),
    });
    let draining = bridge.detached.clone();
    context.own(EffectHandle::new("hooks-claude-code.detached", move || {
        Box::pin(async move {
            draining.drain().await;
            Ok(())
        })
    }))?;
    register_session_start(context, &bridge)?;
    register_pre_step(context, &bridge)?;
    register_pre_tool(context, &bridge)?;
    register_post_tool(context, &bridge)?;
    register_stop(context, &bridge)?;
    register_subagents(context, &bridge)?;
    Ok(())
}

fn register_session_start(context: &Context, bridge: &Arc<Bridge>) -> anyhow::Result<()> {
    let bridge = bridge.clone();
    context.events().on_sync(
        context,
        "agent/session-start",
        move |_, args| {
            let event = args
                .get::<AgentEvent<SessionStartEvent>>(0)
                .ok_or_else(|| anyhow::anyhow!("agent/session-start lacks event"))?;
            let agent = event.agent.clone();
            let source = serde_json::to_value(event.payload.source)?;
            let mut payload = bridge.base(Some(&agent), "SessionStart");
            payload["source"] = source.clone();
            let tracked = bridge.clone();
            let signal = bridge.detached.signal.clone();
            bridge.detached.track(async move {
                let outcome = tracked
                    .run_point(
                        "SessionStart",
                        source.as_str().unwrap_or_default(),
                        payload,
                        RunPointOptions {
                            agent: Some(&agent),
                            turn: None,
                            signal,
                        },
                    )
                    .await;
                match outcome {
                    Ok(merged) => {
                        if let Some(message) = context_from(&merged)
                            && let Err(error) = agent.inject(message)
                        {
                            warn(
                                &tracked.context,
                                format!("hooks-claude-code: SessionStart hook failed: {error}"),
                            );
                        }
                    }
                    Err(error) => warn(
                        &tracked.context,
                        format!("hooks-claude-code: SessionStart hook failed: {error}"),
                    ),
                }
            });
            Ok(EventReply::Undefined)
        },
        global_events(),
    )?;
    Ok(())
}

fn register_pre_step(context: &Context, bridge: &Arc<Bridge>) -> anyhow::Result<()> {
    let bridge = bridge.clone();
    context.events().on_waterfall(
        context,
        "agent/pre-step",
        move |_, args, next| {
            let Some(event) = args.get::<AgentEvent<AgentPreStepEvent>>(0) else {
                return Box::pin(async { Err(anyhow::anyhow!("agent/pre-step lacks payload")) });
            };
            let bridge = bridge.clone();
            Box::pin(async move {
                if event.payload.messages.is_empty() {
                    return next.run().await;
                }
                let prompt = blocks_to_text(
                    &event
                        .payload
                        .messages
                        .iter()
                        .flat_map(|message| message.content().iter().cloned())
                        .collect::<Vec<_>>(),
                );
                let mut payload = bridge.base(Some(&event.agent), "UserPromptSubmit");
                payload["prompt"] = Value::String(prompt);
                let merged = bridge
                    .run_point(
                        "UserPromptSubmit",
                        "",
                        payload,
                        RunPointOptions {
                            agent: Some(&event.agent),
                            turn: Some(event.payload.turn),
                            signal: event.payload.signal.clone(),
                        },
                    )
                    .await?;
                if merged.decision == MergedDecision::Deny {
                    return Ok(EventReply::Value(Arc::new(PreStepDecision::Reject)));
                }
                let reply = next.run().await?;
                let decision = reply
                    .downcast::<PreStepDecision>()
                    .map(|decision| (*decision).clone())
                    .ok_or_else(|| anyhow::anyhow!("agent/pre-step returned invalid decision"))?;
                let Some(ours) = context_from(&merged) else {
                    return Ok(EventReply::Value(Arc::new(decision)));
                };
                Ok(EventReply::Value(Arc::new(match decision {
                    PreStepDecision::Enter { mut messages } => {
                        messages.push(ours);
                        PreStepDecision::Enter { messages }
                    }
                    PreStepDecision::Reject => PreStepDecision::Reject,
                })))
            })
        },
        global_events(),
    )?;
    Ok(())
}

fn register_pre_tool(context: &Context, bridge: &Arc<Bridge>) -> anyhow::Result<()> {
    let bridge = bridge.clone();
    context.events().on_waterfall(
        context,
        "tools/pre-execute",
        move |_, args, next| {
            let Some(execution) = args.get::<ToolExecution>(0) else {
                return Box::pin(async {
                    Err(anyhow::anyhow!("tools/pre-execute lacks execution"))
                });
            };
            let bridge = bridge.clone();
            Box::pin(async move {
                let payload = pre_tool_payload(&bridge, &execution);
                let merged = bridge
                    .run_point(
                        "PreToolUse",
                        &execution.name,
                        payload,
                        RunPointOptions {
                            agent: execution.agent.as_ref(),
                            turn: Some(last_turn(execution.agent.as_ref())),
                            signal: execution.signal(),
                        },
                    )
                    .await?;
                match merged.decision {
                    MergedDecision::Deny => {
                        Ok(EventReply::Value(Arc::new(PreToolDecision::Deny {
                            reason: merged
                                .reason
                                .unwrap_or_else(|| "blocked by PreToolUse hook".to_owned()),
                        })))
                    }
                    MergedDecision::Ask => Ok(EventReply::Value(Arc::new(PreToolDecision::Ask {
                        reason: merged.reason,
                    }))),
                    MergedDecision::None | MergedDecision::Allow => next.run().await,
                }
            })
        },
        global_events(),
    )?;
    Ok(())
}

fn register_post_tool(context: &Context, bridge: &Arc<Bridge>) -> anyhow::Result<()> {
    let bridge = bridge.clone();
    context.events().on_waterfall(
        context,
        "tools/post-execute",
        move |_, args, next| {
            let Some(execution) = args.get::<ToolExecution>(0) else {
                return Box::pin(async {
                    Err(anyhow::anyhow!("tools/post-execute lacks execution"))
                });
            };
            let Some(result) = args.get::<ToolExecutionResult>(1) else {
                return Box::pin(async { Err(anyhow::anyhow!("tools/post-execute lacks result")) });
            };
            let bridge = bridge.clone();
            Box::pin(async move {
                let payload = post_tool_payload(&bridge, &execution, &result);
                let merged = bridge
                    .run_point(
                        "PostToolUse",
                        &execution.name,
                        payload,
                        RunPointOptions {
                            agent: execution.agent.as_ref(),
                            turn: Some(last_turn(execution.agent.as_ref())),
                            signal: execution.signal(),
                        },
                    )
                    .await?;
                let ours = context_from(&merged);
                if merged.decision == MergedDecision::Deny {
                    return Ok(EventReply::Value(Arc::new(PostToolDecision::Block {
                        feedback: vec![ContentBlock::Text {
                            text: merged
                                .reason
                                .unwrap_or_else(|| "blocked by PostToolUse hook".to_owned()),
                        }],
                        additional_contexts: ours.into_iter().collect(),
                    })));
                }
                let reply = next.run().await?;
                let decision = reply
                    .downcast::<PostToolDecision>()
                    .map(|decision| (*decision).clone())
                    .ok_or_else(|| {
                        anyhow::anyhow!("tools/post-execute returned invalid decision")
                    })?;
                Ok(EventReply::Value(Arc::new(fold_post_context(
                    decision, ours,
                ))))
            })
        },
        global_events(),
    )?;
    Ok(())
}

fn register_stop(context: &Context, bridge: &Arc<Bridge>) -> anyhow::Result<()> {
    let bridge = bridge.clone();
    context.events().on(
        context,
        "agent/turn-stopping",
        move |_, args| {
            let bridge = bridge.clone();
            let event = args.get::<AgentEvent<AgentTurnStoppingEvent>>(0);
            Box::pin(async move {
                let event =
                    event.ok_or_else(|| anyhow::anyhow!("agent/turn-stopping lacks event"))?;
                let mut payload = bridge.base(Some(&event.agent), "Stop");
                payload["stop_hook_active"] = Value::Bool(false);
                let merged = bridge
                    .run_point(
                        "Stop",
                        "",
                        payload,
                        RunPointOptions {
                            agent: Some(&event.agent),
                            turn: Some(event.payload.turn),
                            signal: event.payload.signal.clone(),
                        },
                    )
                    .await?;
                if merged.decision == MergedDecision::Deny {
                    let text = merged
                        .reason
                        .unwrap_or_else(|| "continue: blocked by Stop hook".to_owned());
                    if let Err(error) = event.agent.steer(UserMessage::new(
                        vec![ContentBlock::Text { text }],
                        MessageSource::plugin("hooks-claude-code"),
                    )) {
                        warn(
                            &bridge.context,
                            format!("hooks-claude-code: Stop steering failed: {error}"),
                        );
                    }
                }
                Ok(EventReply::Undefined)
            })
        },
        global_events(),
    )?;
    Ok(())
}

fn register_subagents(context: &Context, bridge: &Arc<Bridge>) -> anyhow::Result<()> {
    register_subagent_start(context, bridge)?;
    register_subagent_end(context, bridge)
}

fn register_subagent_start(context: &Context, bridge: &Arc<Bridge>) -> anyhow::Result<()> {
    let start_bridge = bridge.clone();
    context.events().on_sync(
        context,
        "subagent/start",
        move |_, args| {
            let info = args
                .get::<SubagentRunInfo>(0)
                .ok_or_else(|| anyhow::anyhow!("subagent/start lacks info"))?;
            let child = start_bridge
                .context
                .get(AGENTS)
                .and_then(|agents| agents.get(&info.id));
            if let Some(child) = &child {
                start_bridge
                    .subagent_children
                    .lock()
                    .insert(info.run_id.clone(), child.clone());
            }
            let payload = subagent_payload(
                &start_bridge,
                "SubagentStart",
                info.id.as_str(),
                child.as_ref(),
            );
            let tracked = start_bridge.clone();
            let signal = start_bridge.detached.signal.clone();
            start_bridge.detached.track(async move {
                match tracked
                    .run_point(
                        "SubagentStart",
                        SUBAGENT_TYPE,
                        payload,
                        RunPointOptions {
                            agent: child.as_ref(),
                            turn: None,
                            signal,
                        },
                    )
                    .await
                {
                    Ok(merged) => {
                        if let (Some(child), Some(message)) = (child, context_from(&merged))
                            && let Err(error) = child.inject(message)
                        {
                            warn(
                                &tracked.context,
                                format!("hooks-claude-code: SubagentStart hook failed: {error}"),
                            );
                        }
                    }
                    Err(error) => warn(
                        &tracked.context,
                        format!("hooks-claude-code: SubagentStart hook failed: {error}"),
                    ),
                }
            });
            Ok(EventReply::Undefined)
        },
        global_events(),
    )?;
    Ok(())
}

fn register_subagent_end(context: &Context, bridge: &Arc<Bridge>) -> anyhow::Result<()> {
    let end_bridge = bridge.clone();
    context.events().on_sync(
        context,
        "subagent/end",
        move |_, args| {
            let info = args
                .get::<SubagentRunEndInfo>(0)
                .ok_or_else(|| anyhow::anyhow!("subagent/end lacks info"))?;
            let child = end_bridge
                .subagent_children
                .lock()
                .remove(&info.run_id)
                .or_else(|| {
                    end_bridge
                        .context
                        .get(AGENTS)
                        .and_then(|agents| agents.get(&info.id))
                });
            let mut payload = subagent_payload(
                &end_bridge,
                "SubagentStop",
                info.id.as_str(),
                child.as_ref(),
            );
            payload["stop_hook_active"] = Value::Bool(false);
            let tracked = end_bridge.clone();
            let signal = end_bridge.detached.signal.clone();
            end_bridge.detached.track(async move {
                let _ = tracked
                    .run_point(
                        "SubagentStop",
                        SUBAGENT_TYPE,
                        payload,
                        RunPointOptions {
                            agent: child.as_ref(),
                            turn: None,
                            signal,
                        },
                    )
                    .await;
            });
            Ok(EventReply::Undefined)
        },
        global_events(),
    )?;
    Ok(())
}

fn subagent_payload(bridge: &Bridge, event: &str, id: &str, child: Option<&Arc<Agent>>) -> Value {
    let mut payload = bridge.base(child, event);
    payload["agent_id"] = Value::String(id.to_owned());
    payload["agent_type"] = Value::String(SUBAGENT_TYPE.to_owned());
    payload
}

fn pre_tool_payload(bridge: &Bridge, execution: &ToolExecution) -> Value {
    let mut payload = bridge.base(execution.agent.as_ref(), "PreToolUse");
    payload["tool_name"] = Value::String(execution.name.clone());
    payload["tool_input"] = execution.arguments.clone();
    payload["tool_use_id"] = Value::String(execution.call_id.as_str().to_owned());
    payload
}

fn post_tool_payload(
    bridge: &Bridge,
    execution: &ToolExecution,
    result: &ToolExecutionResult,
) -> Value {
    let mut payload = bridge.base(execution.agent.as_ref(), "PostToolUse");
    payload["tool_name"] = Value::String(execution.name.clone());
    payload["tool_input"] = execution.arguments.clone();
    payload["tool_use_id"] = Value::String(execution.call_id.as_str().to_owned());
    payload["tool_response"] = Value::String(blocks_to_text(result.content()));
    payload
}

fn context_from(merged: &MergedHookOutcome) -> Option<UserMessage> {
    (!merged.additional_context.is_empty()).then(|| {
        UserMessage::new(
            merged
                .additional_context
                .iter()
                .cloned()
                .map(|text| ContentBlock::Text { text })
                .collect(),
            MessageSource::plugin("hooks-claude-code"),
        )
    })
}

fn fold_post_context(decision: PostToolDecision, ours: Option<UserMessage>) -> PostToolDecision {
    let Some(ours) = ours else {
        return decision;
    };
    match decision {
        PostToolDecision::Accept {
            content,
            mut additional_contexts,
        } => {
            additional_contexts.insert(0, ours);
            PostToolDecision::Accept {
                content,
                additional_contexts,
            }
        }
        PostToolDecision::ReplaceValue {
            value,
            mut additional_contexts,
        } => {
            additional_contexts.insert(0, ours);
            PostToolDecision::ReplaceValue {
                value,
                additional_contexts,
            }
        }
        PostToolDecision::Block {
            feedback,
            mut additional_contexts,
        } => {
            additional_contexts.insert(0, ours);
            PostToolDecision::Block {
                feedback,
                additional_contexts,
            }
        }
    }
}

fn blocks_to_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn last_turn(agent: Option<&Arc<Agent>>) -> u64 {
    agent
        .and_then(|agent| {
            agent
                .session()
                .events()
                .into_iter()
                .rev()
                .find(|event| event.event_type == "turn/start")
        })
        .and_then(|event| event.data.get("turn").and_then(Value::as_u64))
        .unwrap_or(0)
}

fn next_handler_id(point: &str) -> String {
    format!(
        "claude-code:{point}:{}",
        HANDLER_COUNTER.fetch_add(1, Ordering::AcqRel) + 1
    )
}

fn process_cwd() -> String {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .to_string_lossy()
        .into_owned()
}

fn elapsed_clock() -> impl FnMut() -> i64 {
    let started = Instant::now();
    move || i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)
}

fn warn(context: &Context, message: String) {
    context
        .logger(Some("hooks-claude-code"))
        .warn([Value::String(message)]);
}

fn global_events() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}
