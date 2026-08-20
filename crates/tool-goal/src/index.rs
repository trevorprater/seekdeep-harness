//! Model-facing `get_goal`, `create_goal`, and `update_goal` tools over the
//! persisted same-session goal domain.

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_goal::{
    CreateGoalRequest, EditGoalRequest, GOAL, GoalActivation, GoalBlockReason, GoalId, GoalPhase,
    GoalRef, GoalView,
};
use seekdeep_llm::{ContentBlock, HarnessError, MessageSource, UserMessage, bound_context_summary};
use seekdeep_schemastery::Schema;
use seekdeep_system_prompt::{PromptSection, PromptText, SYSTEM_PROMPT};
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, GenericCallView, TOOLS, ToolCallKind, ToolCallView,
    define_tool,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::authority::{
    GoalToolAuthority, completion_authority, goal_tool_execution, require_direct_human,
};
use crate::wrapup::render_wrapup_context;

/// Cordis plugin name.
pub const NAME: &str = "tool-goal";

/// Services required by the goal control tools.
pub const INJECT: &[&str] = &["agents", "goals", "tools", "systemPrompt"];

/// Largest value the source accepts as a safe integer.
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Default minimum admitted rounds before a model may self-report `blocked`.
const DEFAULT_BLOCKED_AFTER: f64 = 3.0;

/// Model policy and hard lower bounds for goal-state updates.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// Minimum admitted goal rounds before the model may self-report `blocked`.
    pub blocked_after_consecutive_rounds: Option<f64>,
}

/// The source-compatible admission schema for [`Config`].
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn config_schema() -> Schema {
    Schema::object([(
        "blockedAfterConsecutiveRounds",
        Schema::number()
            .step(1.0)
            .min(1.0)
            .with_default(DEFAULT_BLOCKED_AFTER),
    )])
}

/// Canonical goal field projection, omitting replay-only timestamps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoalFieldValue {
    id: GoalId,
    revision: u64,
    objective: String,
    phase: GoalPhase,
    rounds_started: u64,
    max_goal_rounds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blocked_reason: Option<GoalBlockReason>,
}

/// Canonical goal-tool output, matching the compact Native JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoalToolValue {
    goal: Option<GoalFieldValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    activation: Option<GoalActivation>,
}

fn goal_value(goal: Option<&GoalView>) -> GoalToolValue {
    match goal {
        None => GoalToolValue {
            goal: None,
            activation: None,
        },
        Some(goal) => GoalToolValue {
            goal: Some(GoalFieldValue {
                id: goal.id.clone(),
                revision: goal.revision,
                objective: goal.objective.clone(),
                phase: goal.phase,
                rounds_started: goal.rounds_started,
                max_goal_rounds: goal.max_goal_rounds,
                blocked_reason: goal.blocked_reason.clone(),
            }),
            activation: Some(goal.activation),
        },
    }
}

fn has_text(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

fn has_round_cap(value: Option<u64>) -> Option<u64> {
    value.filter(|value| *value != 0)
}

fn has_non_blank(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.trim().is_empty())
}

/// Builds the exact compare-and-set ref from model arguments.
fn goal_ref(goal_id: &str, revision: f64) -> Result<GoalRef, HarnessError> {
    if goal_id.is_empty()
        || goal_id != goal_id.trim()
        || revision.fract() != 0.0
        || !(1.0..=MAX_SAFE_INTEGER).contains(&revision)
    {
        return Err(HarnessError::new(
            "goal_id must be non-empty and revision must be a positive safe integer",
            "GOAL_TOOL_INVALID_UPDATE",
        ));
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(GoalRef {
        id: GoalId::new(goal_id),
        revision: revision as u64,
    })
}

fn guidance(blocked_after: u64) -> String {
    format!(
        "Use goal tools for one long-running completion objective in the current session. create_goal may infer goal intent from a direct human request in any language; do not create a goal for routine single-turn work. Call get_goal before update_goal and copy its exact goal_id and revision. After session resume or fork, an active goal is disarmed: when a human asks to continue or resume in any wording or language, use update_goal action resume to rearm it. Mark complete only when the objective is actually achieved. Mark blocked only after the same blocking condition persists for at least {blocked_after} consecutive goal rounds, and report that concrete condition in blocked_reason; difficulty, uncertainty, or useful remaining work is not blocked."
    )
}

fn notice_source(summary: &str) -> MessageSource {
    let mut fields = Map::new();
    fields.insert("plugin".to_owned(), json!("tool-goal"));
    fields.insert("form".to_owned(), json!("notice"));
    fields.insert("summary".to_owned(), json!(summary));
    MessageSource {
        kind: "plugin".to_owned(),
        fields,
    }
}

fn present(title: &str, kind: ToolCallKind, raw_input: Option<Value>) -> ToolCallView {
    ToolCallView::Generic(GenericCallView {
        title: title.to_owned(),
        kind: Some(kind),
        raw_input,
        content: None,
        locations: None,
    })
}

/// Raw schema-validated `create_goal` arguments.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateGoalArgs {
    objective: String,
    #[serde(default)]
    max_goal_rounds: Option<u64>,
}

/// Empty argument set for `get_goal`.
#[derive(Clone, Debug, Deserialize)]
struct GetGoalArgs {}

/// Raw schema-validated `update_goal` arguments.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateGoalArgs {
    goal_id: String,
    revision: f64,
    action: String,
    #[serde(default)]
    objective: Option<String>,
    #[serde(default)]
    max_goal_rounds: Option<u64>,
    #[serde(default)]
    blocked_reason: Option<String>,
}

fn goal_value_schema() -> Value {
    json!({

    "oneOf": [
        {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "goal": {"type": "null", "required": true}
            }
        },
        {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "goal": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": true,
                    "properties": {
                        "id": {"type": "string", "required": true},
                        "revision": {"type": "integer", "required": true},
                        "objective": {"type": "string", "required": true},
                        "phase": {"type": "string", "required": true, "enum": ["active", "paused", "blocked", "complete"]},
                        "roundsStarted": {"type": "integer", "required": true},
                        "maxGoalRounds": {"type": "integer", "required": true},
                        "blockedReason": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "code": {"type": "string", "required": true},
                                "message": {"type": "string", "required": true}
                            }
                        }
                    }
                },
                "activation": {"type": "string", "required": true, "enum": ["armed", "disarmed"]}
            }
        }
    ]
    })
}

const GET_DESCRIPTION: &str = "Read the current same-session goal, including its exact id/revision, objective, phase, completed continuation rounds, round limit, blocker reason when present, and whether another continuation is armed. Call this before updating a goal.";

const CREATE_DESCRIPTION: &str = "Create one persisted same-session completion goal when the current direct human request is a long-running objective that should continue across autonomous goal rounds. You may infer that intent without requiring the user to say \"create a goal\". Do not use this for trivial single-turn work. Execution rejects non-human and subagent authority.";

const UPDATE_DESCRIPTION: &str = "Update the exact current goal revision. edit, pause, and resume require a direct top-level human request. During an automatic continuation of the current goal, complete and blocked are also allowed. blocked is rejected before the configured minimum round count; the model remains responsible for judging that the same condition persisted across those rounds and must explain it in blocked_reason.";

/// Registers the three goal tools and their shared policy section.
///
/// # Errors
///
/// Returns missing-service, prompt-registration, or tool-registration failures,
/// and invalid admission configuration.
#[allow(clippy::too_many_lines)]
pub fn apply(context: &Context, config: &Config) -> anyhow::Result<()> {
    let blocked_after = resolve_blocked_after(config)?;

    let prompt = context
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("tool-goal requires systemPrompt"))?;
    prompt.section(
        context,
        PromptSection::new(
            "tool:goal",
            114.0,
            PromptText::Static(guidance(blocked_after)),
        ),
    )?;

    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("tool-goal requires tools"))?;

    let execute_ctx = context.clone();
    let get_goal = define_tool(
        DefineToolOptions::new(
            "get_goal",
            GET_DESCRIPTION,
            json!({}),
            DefineToolOutput::new(
                goal_value_schema(),
                Arc::new(|_args: &GetGoalArgs, value: &GoalToolValue| {
                    Ok(vec![ContentBlock::Text {
                        text: serde_json::to_string(value)?,
                    }])
                }),
            ),
            Arc::new(move |_args: GetGoalArgs, exec| {
                let ctx = execute_ctx.clone();
                Box::pin(async move {
                    let execution = goal_tool_execution(&ctx, &exec)?;
                    let goals = ctx
                        .get(GOAL)
                        .ok_or_else(|| anyhow::anyhow!("tool-goal requires goals"))?;
                    let goal = goals.get(&execution.agent)?;
                    Ok(goal_value(goal.as_ref()))
                })
            }),
        )
        .present_call(Arc::new(|_args: &GetGoalArgs| {
            Some(present("Read current goal", ToolCallKind::Read, None))
        })),
    )?;
    tools.register(context, get_goal)?;

    let create_ctx = context.clone();
    let create_goal = define_tool(
        DefineToolOptions::new(
            "create_goal",
            CREATE_DESCRIPTION,
            json!({
                "objective": {"type": "string", "required": true, "description": "The concrete completion objective inferred from the direct human request."},
                "max_goal_rounds": {"type": "number", "description": "Optional positive safe-integer limit on automatic continuation rounds."}
            }),
            DefineToolOutput::new(
                goal_value_schema(),
                Arc::new(|_args: &CreateGoalArgs, value: &GoalToolValue| {
                    Ok(vec![ContentBlock::Text {
                        text: serde_json::to_string(value)?,
                    }])
                }),
            ),
            Arc::new(move |args: CreateGoalArgs, exec| {
                let ctx = create_ctx.clone();
                Box::pin(async move {
                    let execution = goal_tool_execution(&ctx, &exec)?;
                    require_direct_human(&ctx, &execution)?;
                    let goals = ctx
                        .get(GOAL)
                        .ok_or_else(|| anyhow::anyhow!("tool-goal requires goals"))?;
                    let goal = goals.create(
                        &execution.agent,
                        &CreateGoalRequest {
                            objective: args.objective.clone(),
                            max_goal_rounds: args.max_goal_rounds,
                        },
                    )?;
                    Ok(goal_value(Some(&goal)))
                })
            }),
        )
        .present_call(Arc::new(|args: &CreateGoalArgs| {
            Some(present(
                "Create goal",
                ToolCallKind::Other,
                Some(json!(args.objective)),
            ))
        })),
    )?;
    tools.register(context, create_goal)?;

    let update_ctx = context.clone();
    let update_goal = define_tool(
        DefineToolOptions::new(
            "update_goal",
            UPDATE_DESCRIPTION,
            json!({
                "goal_id": {"type": "string", "required": true, "description": "Exact id returned by get_goal."},
                "revision": {"type": "number", "required": true, "description": "Exact positive revision returned by get_goal."},
                "action": {"type": "string", "required": true, "enum": ["edit", "pause", "resume", "complete", "blocked"], "description": "edit | pause | resume | complete | blocked"},
                "objective": {"type": "string", "description": "Replacement objective; valid only with action edit."},
                "max_goal_rounds": {"type": "number", "description": "Replacement cap; valid only with action edit."},
                "blocked_reason": {"type": "string", "description": "Concrete blocking condition; required only with action blocked."}
            }),
            DefineToolOutput::new(
                goal_value_schema(),
                Arc::new(|_args: &UpdateGoalArgs, value: &GoalToolValue| {
                    Ok(vec![ContentBlock::Text {
                        text: serde_json::to_string(value)?,
                    }])
                }),
            ),
            Arc::new(move |args: UpdateGoalArgs, exec| {
                let ctx = update_ctx.clone();
                Box::pin(async move {
                    let execution = goal_tool_execution(&ctx, &exec)?;
                    let goal_ref = goal_ref(&args.goal_id, args.revision)?;
                    let replacements = EditGoalRequest {
                        objective: has_text(args.objective.as_deref()).then(|| args.objective.clone()).flatten(),
                        max_goal_rounds: has_round_cap(args.max_goal_rounds),
                    };
                    let goals = ctx
                        .get(GOAL)
                        .ok_or_else(|| anyhow::anyhow!("tool-goal requires goals"))?;
                    match args.action.as_str() {
                        "edit" => {
                            require_direct_human(&ctx, &execution)?;
                            if has_text(args.blocked_reason.as_deref()) {
                                return Err(anyhow::Error::from(HarnessError::new(
                                    "blocked_reason is valid only with action blocked",
                                    "GOAL_TOOL_INVALID_UPDATE",
                                )));
                            }
                            let goal = goals.edit(&execution.agent, &goal_ref, &replacements)?;
                            Ok(goal_value(Some(&goal)))
                        }
                        "pause" | "resume" => {
                            require_direct_human(&ctx, &execution)?;
                            if has_text(args.objective.as_deref())
                                || has_round_cap(args.max_goal_rounds).is_some()
                                || has_text(args.blocked_reason.as_deref())
                            {
                                return Err(anyhow::Error::from(HarnessError::new(
                                    "objective and max_goal_rounds are valid only with action edit; blocked_reason is valid only with action blocked",
                                    "GOAL_TOOL_INVALID_UPDATE",
                                )));
                            }
                            let goal = if args.action == "pause" {
                                goals.pause(&execution.agent, &goal_ref)?
                            } else {
                                goals.resume(&execution.agent, &goal_ref)?
                            };
                            Ok(goal_value(Some(&goal)))
                        }
                        "complete" | "blocked" => {
                            let authority = completion_authority(&ctx, &execution)?;
                            if has_text(args.objective.as_deref())
                                || has_round_cap(args.max_goal_rounds).is_some()
                            {
                                return Err(anyhow::Error::from(HarnessError::new(
                                    "objective and max_goal_rounds are valid only with action edit",
                                    "GOAL_TOOL_INVALID_UPDATE",
                                )));
                            }
                            if args.action == "complete" && has_text(args.blocked_reason.as_deref()) {
                                return Err(anyhow::Error::from(HarnessError::new(
                                    "blocked_reason is valid only with action blocked",
                                    "GOAL_TOOL_INVALID_UPDATE",
                                )));
                            }
                            if args.action == "blocked" && !has_non_blank(args.blocked_reason.as_deref()) {
                                return Err(anyhow::Error::from(HarnessError::new(
                                    "blocked_reason is required with action blocked",
                                    "GOAL_TOOL_INVALID_UPDATE",
                                )));
                            }
                            if args.action == "blocked"
                                && let GoalToolAuthority::GoalRound { goal } = &authority
                                && goal.rounds_started < blocked_after
                            {
                                return Err(anyhow::Error::from(HarnessError::new(
                                    format!(
                                        "blocked requires at least {blocked_after} consecutive goal rounds; current round is {}",
                                        goal.rounds_started
                                    ),
                                    "GOAL_TOOL_BLOCK_THRESHOLD",
                                )));
                            }
                            let goal = if args.action == "complete" {
                                goals.complete(&execution.agent, &goal_ref)?
                            } else {
                                goals.block(
                                    &execution.agent,
                                    &goal_ref,
                                    &json!({
                                        "code": "model-reported",
                                        "message": args.blocked_reason.as_deref().unwrap_or_default()
                                    }),
                                )?
                            };
                            if matches!(authority, GoalToolAuthority::GoalRound { .. }) {
                                let wrapup_content = if args.action == "complete" {
                                    render_wrapup_context(&goal.objective, None)
                                } else {
                                    render_wrapup_context(
                                        &goal.objective,
                                        args.blocked_reason.as_deref(),
                                    )
                                };
                                exec.defer_context(UserMessage::new(
                                    wrapup_content,
                                    notice_source(&bound_context_summary(&format!(
                                        "{}: {}",
                                        args.action, goal.objective
                                    ))),
                                ));
                            }
                            Ok(goal_value(Some(&goal)))
                        }
                        other => Err(anyhow::Error::from(HarnessError::new(
                            format!("unknown goal action {other:?}"),
                            "GOAL_TOOL_INVALID_UPDATE",
                        ))),
                    }
                })
            }),
        )
        .present_call(Arc::new(|args: &UpdateGoalArgs| {
            let title = if args.action == "blocked" {
                "Mark goal".to_owned()
            } else {
                let mut chars = args.action.chars();
                let first = chars.next().map_or(' ', |c| c.to_ascii_uppercase());
                format!("{first}{} goal", chars.as_str())
            };
            let raw_input = if has_text(args.blocked_reason.as_deref()) {
                Some(json!(args.blocked_reason.as_deref().unwrap_or_default()))
            } else if has_text(args.objective.as_deref()) {
                Some(json!(args.objective.as_deref().unwrap_or_default()))
            } else if let Some(cap) = has_round_cap(args.max_goal_rounds) {
                Some(json!(cap))
            } else {
                Some(json!(args.goal_id))
            };
            Some(present(&title, ToolCallKind::Other, raw_input))
        })),
    )?;
    tools.register(context, update_goal)?;

    Ok(())
}

/// Validates the blocked-after threshold even outside Loader normalization.
fn resolve_blocked_after(config: &Config) -> anyhow::Result<u64> {
    let blocked_after = config
        .blocked_after_consecutive_rounds
        .unwrap_or(DEFAULT_BLOCKED_AFTER);
    if !blocked_after.is_finite()
        || blocked_after.fract() != 0.0
        || !(1.0..=MAX_SAFE_INTEGER).contains(&blocked_after)
    {
        anyhow::bail!("blockedAfterConsecutiveRounds must be a positive safe integer");
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(blocked_after as u64)
}

/// Builds the loader-compatible goal control tools plugin.
#[must_use]
pub fn plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(config)?;
            apply(&context, &config)?;
            Ok(())
        })
    })
    .with_config_validator(|value: &Value| {
        config_schema()
            .resolve(value)
            .map_err(|error| anyhow::anyhow!("{error}"))
    })
}
