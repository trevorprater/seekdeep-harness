//! Codex dialect payloads and interception-point bridge.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use seekdeep_agent::{Agent, AgentEvent, PreStepDecision};
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
use seekdeep_tools::{PostToolDecision, PreToolDecision, ToolExecution, ToolExecutionResult};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{CodexHookConfig, parse_codex_config};

/// Cordis plugin name.
pub const NAME: &str = "hooks-codex";
/// Required capability.
pub const INJECT: &[&str] = &["shell"];
static HANDLER_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Codex hook bridge configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Process-level hooks JSON path.
    pub config_path: String,
    /// Model name stamped on each payload.
    #[serde(default)]
    pub model: String,
    /// Default per-hook timeout in milliseconds.
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
        ("model", Schema::string().with_default("")),
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
    hooks: CodexHookConfig,
    detached: DetachedRuns,
    stderr_summary_max_chars: usize,
}

struct RunPointOptions<'a> {
    agent: Option<&'a Arc<Agent>>,
    turn: Option<u64>,
    signal: AbortSignal,
    plain_stdout_as_context: bool,
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
        for group in groups {
            if !matches_matcher(group.matcher.as_deref(), match_query, MatcherMode::Codex) {
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
                            dialect: HookDialect::Codex,
                            handler_id: handler_id.clone(),
                            matcher: group.matcher.clone(),
                        },
                    )?;
                }
                let clock = elapsed_clock();
                let mut result = run_hook(
                    &**self.shell,
                    &hook,
                    &RunHookOptions {
                        payload: payload.clone(),
                        env: None,
                        cwd: workdir.clone(),
                        signal: options.signal.clone(),
                        trailing_newline: false,
                        default_timeout_ms: self.config.default_timeout_ms,
                        expected_event_name: Some(point.to_owned()),
                    },
                    clock,
                )
                .await;
                if options.plain_stdout_as_context
                    && result.output.exit_code == Some(0)
                    && result.output.additional_context.is_none()
                    && !result.output.stdout.is_empty()
                    && !result.output.stdout.starts_with('{')
                {
                    result.output.additional_context = Some(result.output.stdout.clone());
                }
                if result.output.system_message.is_some() {
                    warn(
                        &self.context,
                        format!(
                            "hooks-codex: {point} hook emitted a systemMessage, which is not yet surfaced (ignored)"
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
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .to_string_lossy()
                    .into_owned()
            });
        let transcript_path = agent.and_then(|agent| {
            self.context
                .get(SESSION_PERSISTENCE)
                .and_then(|service| service.persistence().locate(agent.session().header()))
                .map(|location| location.path.to_string_lossy().into_owned())
        });
        json!({
            "session_id": agent.map_or("", |agent| agent.session().id().as_str()),
            "transcript_path": transcript_path,
            "cwd": cwd,
            "hook_event_name": event,
            "model": self.config.model,
            "permission_mode": "default",
        })
    }

    fn turn_base(&self, agent: Option<&Arc<Agent>>, event: &str) -> Value {
        let mut base = self.base(agent, event);
        base["turn_id"] = Value::String(last_turn(agent).to_string());
        base
    }
}

/// Installs the five-point Codex hook bridge.
///
/// Config read/parse failures are warned and intentionally register no hooks.
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
        "hooks-codex: stderrSummaryMaxChars must be a positive integer"
    );
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let stderr_summary_max_chars = config.stderr_summary_max_chars as usize;
    let raw = match std::fs::read_to_string(&config.config_path)
        .and_then(|content| {
            serde_json::from_str::<Value>(&content)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })
        .and_then(|value| {
            parse_codex_config(&value)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        }) {
        Ok(parsed) => parsed,
        Err(error) => {
            warn(
                context,
                format!(
                    "hooks-codex: could not load hook config {:?}: {error} — no hooks registered",
                    config.config_path
                ),
            );
            return Ok(());
        }
    };
    for skipped in &raw.skipped {
        warn(
            context,
            format!(
                "hooks-codex: skipping {} on {} (only sync command hooks run)",
                skipped.reason, skipped.event
            ),
        );
    }
    let shell = context
        .get(SHELL)
        .ok_or_else(|| anyhow::anyhow!("hooks-codex requires shell"))?;
    let bridge = Arc::new(Bridge {
        context: context.clone(),
        shell,
        config: config.clone(),
        hooks: raw.config,
        detached: create_detached_runs(),
        stderr_summary_max_chars,
    });
    let draining = bridge.detached.clone();
    context.own(EffectHandle::new("hooks-codex.detached", move || {
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
                .ok_or_else(|| anyhow::anyhow!("agent/session-start lacks its event"))?;
            let agent = event.agent.clone();
            let source = serde_json::to_value(event.payload.source)?;
            let mut payload = bridge.base(Some(&agent), "SessionStart");
            payload["source"] = source.clone();
            let tracked = bridge.clone();
            let signal = bridge.detached.signal.clone();
            bridge.detached.track(async move {
                match tracked
                    .run_point(
                        "SessionStart",
                        source.as_str().unwrap_or_default(),
                        payload,
                        RunPointOptions {
                            agent: Some(&agent),
                            turn: None,
                            signal,
                            plain_stdout_as_context: true,
                        },
                    )
                    .await
                {
                    Ok(merged) => {
                        if let Some(context) = context_from(&merged)
                            && let Err(error) = agent.inject(context)
                        {
                            warn(
                                &tracked.context,
                                format!("hooks-codex: SessionStart hook failed: {error}"),
                            );
                        }
                    }
                    Err(error) => warn(
                        &tracked.context,
                        format!("hooks-codex: SessionStart hook failed: {error}"),
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
                return Box::pin(async {
                    Err(anyhow::anyhow!("agent/pre-step lacks its payload"))
                });
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
                let mut payload = bridge.turn_base(Some(&event.agent), "UserPromptSubmit");
                payload["turn_id"] = Value::String(event.payload.turn.to_string());
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
                            plain_stdout_as_context: true,
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
                    .ok_or_else(|| {
                        anyhow::anyhow!("agent/pre-step returned an invalid decision")
                    })?;
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
                let turn = last_turn(execution.agent.as_ref());
                let merged = bridge
                    .run_point(
                        "PreToolUse",
                        &execution.name,
                        payload,
                        RunPointOptions {
                            agent: execution.agent.as_ref(),
                            turn: Some(turn),
                            signal: execution.signal(),
                            plain_stdout_as_context: false,
                        },
                    )
                    .await?;
                if merged.decision == MergedDecision::Deny {
                    return Ok(EventReply::Value(Arc::new(PreToolDecision::Deny {
                        reason: merged
                            .reason
                            .unwrap_or_else(|| "blocked by PreToolUse hook".to_owned()),
                    })));
                }
                next.run().await
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
                let turn = last_turn(execution.agent.as_ref());
                let merged = bridge
                    .run_point(
                        "PostToolUse",
                        &execution.name,
                        payload,
                        RunPointOptions {
                            agent: execution.agent.as_ref(),
                            turn: Some(turn),
                            signal: execution.signal(),
                            plain_stdout_as_context: false,
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
                let mut payload = bridge.turn_base(Some(&event.agent), "Stop");
                payload["turn_id"] = Value::String(event.payload.turn.to_string());
                payload["stop_hook_active"] = Value::Bool(false);
                payload["last_assistant_message"] = Value::Null;
                let merged = bridge
                    .run_point(
                        "Stop",
                        "",
                        payload,
                        RunPointOptions {
                            agent: Some(&event.agent),
                            turn: Some(event.payload.turn),
                            signal: event.payload.signal.clone(),
                            plain_stdout_as_context: false,
                        },
                    )
                    .await?;
                if merged.decision == MergedDecision::Deny {
                    let text = merged
                        .reason
                        .unwrap_or_else(|| "continue: blocked by Stop hook".to_owned());
                    if let Err(error) = event.agent.steer(UserMessage::new(
                        vec![ContentBlock::Text { text }],
                        MessageSource::plugin("hooks-codex"),
                    )) {
                        warn(
                            &bridge.context,
                            format!("hooks-codex: Stop steering failed: {error}"),
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

fn context_from(merged: &MergedHookOutcome) -> Option<UserMessage> {
    (!merged.additional_context.is_empty()).then(|| {
        UserMessage::new(
            merged
                .additional_context
                .iter()
                .cloned()
                .map(|text| ContentBlock::Text { text })
                .collect(),
            MessageSource::plugin("hooks-codex"),
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

fn pre_tool_payload(bridge: &Bridge, execution: &ToolExecution) -> Value {
    let mut payload = bridge.turn_base(execution.agent.as_ref(), "PreToolUse");
    payload["tool_name"] = Value::String(execution.name.clone());
    payload["tool_input"] = json!({"command": command_of(&execution.arguments)});
    payload["tool_use_id"] = Value::String(execution.call_id.as_str().to_owned());
    payload
}

fn post_tool_payload(
    bridge: &Bridge,
    execution: &ToolExecution,
    result: &ToolExecutionResult,
) -> Value {
    let mut payload = bridge.turn_base(execution.agent.as_ref(), "PostToolUse");
    payload["tool_name"] = Value::String(execution.name.clone());
    payload["tool_input"] = json!({"command": command_of(&execution.arguments)});
    payload["tool_use_id"] = Value::String(execution.call_id.as_str().to_owned());
    payload["tool_response"] = Value::String(blocks_to_text(result.content()));
    payload
}

fn command_of(arguments: &Value) -> &str {
    arguments
        .as_object()
        .and_then(|object| object.get("command"))
        .and_then(Value::as_str)
        .unwrap_or_default()
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
        "codex:{point}:{}",
        HANDLER_COUNTER.fetch_add(1, Ordering::AcqRel) + 1
    )
}

fn elapsed_clock() -> impl FnMut() -> i64 {
    let started = Instant::now();
    move || i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX)
}

fn warn(context: &Context, message: String) {
    context
        .logger(Some("hooks-codex"))
        .warn([Value::String(message)]);
}

fn global_events() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}
