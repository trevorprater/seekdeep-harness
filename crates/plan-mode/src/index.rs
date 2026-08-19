//! Plan mode pure fold and validation plus the live controller.

use std::{
    collections::HashMap,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
    },
};

use parking_lot::Mutex;
use regex::Regex;
use seekdeep_agent::{Agent, AgentEvent, PreStepDecision};
use seekdeep_agent_loop::AgentPreStepEvent;
use seekdeep_commands::{COMMANDS, CommandDefinition, CommandInvocation, CommandResult};
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_core::session::{AppendOptions, Session, SessionEvent};
use seekdeep_llm::{ContentBlock, MessageSource, UserMessage};
use seekdeep_session_projection::{
    ProjectionDefinition, ProjectionTransition, SESSION_PROJECTIONS,
};
use seekdeep_system_prompt::{PromptSection, PromptText, SYSTEM_PROMPT};
use seekdeep_tools::{
    DefineToolOptions, DefineToolOutput, GenericCallView, GenericResultView, TOOLS, ToolCallKind,
    ToolCallView, ToolResult, ToolResultView, define_tool,
};
use seekdeep_user_questions::{
    AskUserQuestionIntent, AskUserQuestionItem, AskUserQuestionOption, AskUserQuestionRequest,
    USER_QUESTIONS, UserQuestionError,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The model-facing exit tool's name.
pub const EXIT_PLAN_MODE: &str = "exit_plan_mode";

/// The review question's id.
pub const REVIEW_ID: &str = "plan-review";
/// The review question's approve option label.
pub const APPROVE_LABEL: &str = "Approve";
/// The review question's keep-planning option label.
pub const KEEP_PLANNING_LABEL: &str = "Keep planning";

/// The model-facing exit tool description.
pub const EXIT_DESCRIPTION: &str = "Use only in plan mode. Present your plan for the user's review and, on approval, leave plan mode. Send the COMPLETE plan as markdown, starting with a # heading that names it. The user may approve (carry out the plan from your next step) or keep planning — their feedback comes back in the tool result; revise and present again.";

/// Deployment-owned plan guidance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanModeConfig {
    /// Guidance rendered while plan mode is active.
    pub section: String,
}

static HEADING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^#{1,6}\s+(.+?)\s*$").expect("static heading regex"));

/// The plan's first markdown heading (any level), or none.
#[must_use]
pub fn first_heading(plan: &str) -> Option<String> {
    for line in plan.split('\n') {
        if let Some(captures) = HEADING.captures(line)
            && let Some(heading) = captures.get(1)
        {
            return Some(heading.as_str().to_owned());
        }
    }
    None
}

/// Validates deployment-owned plan guidance.
///
/// # Errors
///
/// Returns a blank-section failure.
pub fn resolve_config(config: &PlanModeConfig) -> anyhow::Result<PlanModeConfig> {
    if config.section.trim().is_empty() {
        anyhow::bail!("PlanModeConfig needs a non-empty 'section'");
    }
    Ok(config.clone())
}

/// Whether plan mode is active after the first end events.
#[must_use]
pub fn fold_plan_mode(events: &[SessionEvent], end: usize) -> bool {
    let mut active = false;
    for event in events.iter().take(end) {
        if event.event_type == "plan/mode" {
            active = event
                .data
                .get("active")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        }
    }
    active
}

/// Whether the log holds an opened turn without its closing turn/end.
#[must_use]
pub fn has_open_turn(events: &[SessionEvent]) -> bool {
    let mut open = false;
    for event in events {
        match event.event_type.as_str() {
            "turn/start" => open = true,
            "turn/end" => open = false,
            _ => {}
        }
    }
    open
}

/// Plan state at the last logged request header, or none before the first header.
#[must_use]
pub fn plan_mode_at_last_header(events: &[SessionEvent]) -> Option<bool> {
    let mut last_header = None;
    for (index, event) in events.iter().enumerate() {
        if event.event_type == "request/header" {
            last_header = Some(index);
        }
    }
    Some(fold_plan_mode(events, last_header? + 1))
}

/// Typed Cordis slot for the plan-mode controller.
pub const PLAN_MODE: ServiceKey<PlanModeController> = ServiceKey::new("planMode");

/// Cordis plugin name.
pub const NAME: &str = "plan-mode";

/// Services required by the controller.
pub const INJECT: &[&str] = &["tools", "systemPrompt"];

/// What one plan-mode selection did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanSetOutcome {
    /// Logged immediately.
    Committed,
    /// Awaiting the next accepted in-turn pre-step.
    Queued,
    /// An opposite pending selection was cleared.
    Cancelled,
    /// Already in that state.
    Noop,
}

/// Read result: logged state plus any pending selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanGetResult {
    /// Logged state in force.
    pub active: bool,
    /// Pending target, when one awaits the next pre-step.
    pub pending: Option<bool>,
}

/// One pending selection awaiting the next accepted in-turn pre-step.
#[derive(Clone, Copy, Debug)]
struct PendingIntent {
    active: bool,
    narrate: bool,
}

/// Projection unit state: logged mode plus the latest unresolved selection.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
struct PlanUnitState {
    active: bool,
    wanted: Option<bool>,
}

/// Raw exit-tool arguments.
#[derive(Clone, Debug, Deserialize)]
struct ExitPlanModeArgs {
    plan: String,
}

/// Exit-tool output value.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
struct ExitPlanModeOutcome {
    approved: bool,
}

fn session_key(session: &Arc<Session>) -> usize {
    Arc::as_ptr(session) as usize
}

/// Whether a trimmed plan starts with a markdown heading and has content.
fn valid_exit_plan(plan: &str) -> bool {
    let Some(after_hash) = plan.trim().strip_prefix('#') else {
        return false;
    };
    let mut chars = after_hash.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_whitespace() && chars.any(|c| !c.is_whitespace())
}

/// Owns logged plan state, the guidance section, the plan command, and the
/// stable exit tool.
pub struct PlanModeController {
    section: String,
    pending_intents: Mutex<HashMap<usize, PendingIntent>>,
    disposed: Arc<AtomicBool>,
}

impl PlanModeController {
    /// Builds the controller and registers every scoped effect.
    ///
    /// # Errors
    ///
    /// Returns config-validation or registration failures.
    pub fn new(context: &Context, config: &PlanModeConfig) -> anyhow::Result<Arc<Self>> {
        let section = resolve_config(config)?.section;
        let controller = Arc::new(Self {
            section,
            pending_intents: Mutex::new(HashMap::new()),
            disposed: Arc::new(AtomicBool::new(false)),
        });
        controller.register_pre_step(context)?;
        controller.register_system_prompt(context)?;
        Self::register_projection(context)?;
        controller.register_command(context)?;
        controller.register_exit_tool(context)?;
        controller.register_dispose_effect(context)?;
        Ok(controller)
    }

    /// Publishes this controller on the plan-mode service slot.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(PLAN_MODE, self.clone())
    }

    /// Builds and publishes the controller.
    ///
    /// # Errors
    ///
    /// Returns config-validation, registration, or provide failures.
    pub fn install(context: &Context, config: &PlanModeConfig) -> anyhow::Result<Arc<Self>> {
        let controller = Self::new(context, config)?;
        controller.provide(context)?;
        Ok(controller)
    }

    /// Reads the logged state and any pending selection.
    #[must_use]
    pub fn get(&self, agent: &Agent) -> PlanGetResult {
        let session = agent.session();
        let events = session.events();
        let active = fold_plan_mode(&events, events.len());
        let pending = self
            .pending_intents
            .lock()
            .get(&session_key(session))
            .copied();
        PlanGetResult {
            active,
            pending: pending.map(|pending| pending.active),
        }
    }

    /// Selects whether plan mode should be active.
    ///
    /// # Errors
    ///
    /// Returns append or narration-injection failures.
    pub fn set(&self, agent: &Agent, active: bool) -> anyhow::Result<PlanSetOutcome> {
        let session = agent.session();
        let key = session_key(session);
        let pending = self.pending_intents.lock().get(&key).copied();
        let events = session.events();
        let logged = fold_plan_mode(&events, events.len());
        let target = pending.map_or(logged, |pending| pending.active);
        if active == target {
            return Ok(PlanSetOutcome::Noop);
        }
        if has_open_turn(&events) {
            self.pending_intents.lock().insert(
                key,
                PendingIntent {
                    active,
                    narrate: true,
                },
            );
            return Ok(if logged == active {
                PlanSetOutcome::Cancelled
            } else {
                PlanSetOutcome::Queued
            });
        }
        if active == logged {
            self.pending_intents.lock().remove(&key);
            return Ok(PlanSetOutcome::Cancelled);
        }
        session.append(
            "plan/mode",
            json!({"active": active}),
            AppendOptions::default(),
        )?;
        self.pending_intents.lock().remove(&key);
        if let Some(narration) = Self::narration(session, active) {
            agent.inject(narration)?;
        }
        Ok(PlanSetOutcome::Committed)
    }

    fn narration(session: &Arc<Session>, target: bool) -> Option<UserMessage> {
        let events = session.events();
        let told = plan_mode_at_last_header(&events)?;
        if told == target {
            return None;
        }
        let text = if target {
            "The user switched this session to plan mode."
        } else {
            "The user switched this session back to the default mode."
        };
        let mut source = MessageSource::plugin("plan-mode");
        source
            .fields
            .insert("form".to_owned(), Value::String("notice".to_owned()));
        source
            .fields
            .insert("summary".to_owned(), Value::String(text.to_owned()));
        Some(UserMessage::new(
            vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            source,
        ))
    }

    fn on_boundary(&self, session: &Arc<Session>) -> anyhow::Result<()> {
        let key = session_key(session);
        let pending = self.pending_intents.lock().get(&key).copied();
        let Some(pending) = pending else {
            return Ok(());
        };
        let events = session.events();
        let target = pending.active;
        if target == fold_plan_mode(&events, events.len()) {
            self.pending_intents.lock().remove(&key);
            return Ok(());
        }
        session.append(
            "plan/mode",
            json!({"active": target}),
            AppendOptions::default(),
        )?;
        self.pending_intents.lock().remove(&key);
        Ok(())
    }

    fn register_pre_step(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let controller = Arc::clone(self);
        context.events().on_waterfall(
            context,
            "agent/pre-step",
            move |_, args, next| {
                let controller = Arc::clone(&controller);
                let Some(event) = args.get::<AgentEvent<AgentPreStepEvent>>(0) else {
                    return Box::pin(async move {
                        Err(anyhow::anyhow!("agent/pre-step lacks its payload"))
                    });
                };
                let agent = event.agent.clone();
                let signal = event.payload.signal.clone();
                Box::pin(async move {
                    let reply = next.run().await?;
                    let decision = reply
                        .downcast::<PreStepDecision>()
                        .map(|decision| (*decision).clone())
                        .ok_or_else(|| {
                            anyhow::anyhow!("agent/pre-step returned an invalid decision")
                        })?;
                    let key = session_key(agent.session());
                    let pending = controller.pending_intents.lock().get(&key).copied();
                    if matches!(decision, PreStepDecision::Reject)
                        || signal.is_aborted()
                        || pending.is_none()
                    {
                        return Ok(EventReply::Value(Arc::new(decision)));
                    }
                    let pending = pending.expect("checked above");
                    let narration = Self::narration(agent.session(), pending.active);
                    if let Err(error) = controller.on_boundary(agent.session()) {
                        tracing::warn!(
                            ?error,
                            "plan-mode: failed to append selected plan mode at step start"
                        );
                        return Ok(EventReply::Value(Arc::new(decision)));
                    }
                    let Some(narration) = narration else {
                        return Ok(EventReply::Value(Arc::new(decision)));
                    };
                    if !pending.narrate {
                        return Ok(EventReply::Value(Arc::new(decision)));
                    }
                    let mut decision = decision;
                    if let PreStepDecision::Enter { messages } = &mut decision {
                        messages.push(narration);
                    }
                    Ok(EventReply::Value(Arc::new(decision)))
                })
            },
            EventOptions::default(),
        )?;
        Ok(())
    }

    fn register_system_prompt(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let prompt = context
            .get(SYSTEM_PROMPT)
            .ok_or_else(|| anyhow::anyhow!("plan-mode requires systemPrompt"))?;
        let controller = Arc::clone(self);
        prompt.section(
            context,
            PromptSection::new(
                "plan:policy",
                50.0,
                PromptText::Dynamic(Arc::new(move |assemble| {
                    let Some(session) = assemble.agent_session.as_ref() else {
                        return Ok(String::new());
                    };
                    let key = session_key(session);
                    let active = controller.pending_intents.lock().get(&key).map_or_else(
                        || {
                            let events = session.events();
                            fold_plan_mode(&events, events.len())
                        },
                        |pending| pending.active,
                    );
                    Ok(if active {
                        controller.section.clone()
                    } else {
                        String::new()
                    })
                })),
            ),
        )?;
        Ok(())
    }

    fn register_projection(context: &Context) -> anyhow::Result<()> {
        let Some(registry) = context.get(SESSION_PROJECTIONS) else {
            return Ok(());
        };
        let definition = ProjectionDefinition::new(
            "plan",
            1,
            || Ok(serde_json::to_value(PlanUnitState::default())?),
            |state: &Value, event: &SessionEvent| {
                let mut current: PlanUnitState = serde_json::from_value(state.clone())?;
                if event.event_type == "command/run"
                    && event.data.get("name").and_then(Value::as_str) == Some("plan")
                {
                    let Some(args) = event.data.get("args").and_then(Value::as_str) else {
                        return Ok(ProjectionTransition::Unchanged);
                    };
                    let wanted = args.trim() != "off";
                    if wanted == current.wanted.unwrap_or(false) {
                        return Ok(ProjectionTransition::Unchanged);
                    }
                    current.wanted = Some(wanted);
                    return ProjectionTransition::changed(current);
                }
                if event.event_type == "plan/mode" {
                    current.active = event
                        .data
                        .get("active")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    current.wanted = None;
                    return ProjectionTransition::changed(current);
                }
                Ok(ProjectionTransition::Unchanged)
            },
            |state: &Value| {
                let unit: PlanUnitState = serde_json::from_value(state.clone())?;
                let pending = unit.wanted.is_some() && unit.wanted.unwrap_or(false) != unit.active;
                Ok(serde_json::to_value(crate::types::PlanProjection {
                    active: unit.active,
                    pending,
                })?)
            },
        );
        registry.register(context, definition)?;
        Ok(())
    }

    fn register_command(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let Some(commands) = context.get(COMMANDS) else {
            return Ok(());
        };
        let controller = Arc::clone(self);
        let definition = CommandDefinition::new(
            "plan",
            "Enter or leave plan mode",
            Arc::new(move |invocation: CommandInvocation| {
                let controller = Arc::clone(&controller);
                Box::pin(async move {
                    let message = invocation.raw_input.trim().to_owned();
                    if message == "off" {
                        return Ok(match controller.set(&invocation.agent, false)? {
                            PlanSetOutcome::Committed => {
                                CommandResult::success(Some("Plan mode off."))
                            }
                            PlanSetOutcome::Queued => CommandResult::success(Some(
                                "Leaving plan mode (applies from the next step).",
                            )),
                            PlanSetOutcome::Cancelled => {
                                CommandResult::success(Some("Plan mode entry cancelled."))
                            }
                            PlanSetOutcome::Noop => {
                                let events = invocation.agent.session().events();
                                if fold_plan_mode(&events, events.len()) {
                                    CommandResult::success(Some(
                                        "Leaving plan mode (applies from the next step).",
                                    ))
                                } else {
                                    CommandResult::success(Some("Plan mode is already inactive."))
                                }
                            }
                        });
                    }
                    let outcome = controller.set(&invocation.agent, true)?;
                    if !message.is_empty() {
                        invocation.agent.steer(UserMessage::new(
                            vec![ContentBlock::Text { text: message }],
                            MessageSource::user(),
                        ))?;
                    }
                    let text = if outcome == PlanSetOutcome::Committed {
                        "Plan mode on. Use /plan off to leave."
                    } else {
                        "Entering plan mode (applies from the next step). Use /plan off to leave."
                    };
                    Ok(CommandResult::success(Some(text)))
                })
            }),
        )
        .with_input("[off|message]");
        let _ = commands.register(context, definition)?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn register_exit_tool(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let tools = context
            .get(TOOLS)
            .ok_or_else(|| anyhow::anyhow!("plan-mode requires tools"))?;
        let controller = Arc::clone(self);
        let execute_ctx = context.clone();
        let output = DefineToolOutput::new(
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "approved": {"type": "boolean", "const": true, "required": true},
                },
            }),
            Arc::new(|_args: &ExitPlanModeArgs, _value: &ExitPlanModeOutcome| {
                Ok(vec![ContentBlock::Text {
                    text: "Plan approved — plan mode exited; carry out the plan starting with your next step."
                        .to_owned(),
                }])
            }),
        );
        let definition = define_tool(
            DefineToolOptions::new(
                EXIT_PLAN_MODE,
                EXIT_DESCRIPTION,
                json!({
                    "plan": {"type": "string", "required": true, "description": "The complete plan, as markdown, starting with a # heading that names it."},
                }),
                output,
                Arc::new(move |args: ExitPlanModeArgs, exec| {
                    let controller = Arc::clone(&controller);
                    let ctx = execute_ctx.clone();
                    Box::pin(async move {
                        let agent = exec.agent.clone().ok_or_else(|| {
                            anyhow::anyhow!("{EXIT_PLAN_MODE} requires a calling agent (no session to switch)")
                        })?;
                        let events = agent.session().events();
                        if !fold_plan_mode(&events, events.len()) {
                            anyhow::bail!("{EXIT_PLAN_MODE} is only available in plan mode");
                        }
                        if !valid_exit_plan(&args.plan) {
                            anyhow::bail!("{EXIT_PLAN_MODE} requires a non-empty markdown plan starting with a # heading");
                        }
                        let interaction = ctx.get(USER_QUESTIONS).ok_or_else(|| {
                            anyhow::anyhow!("no user-questions channel is available to review the plan; ask the user to switch the session mode instead")
                        })?;
                        let request = AskUserQuestionRequest {
                            questions: vec![AskUserQuestionItem {
                                id: REVIEW_ID.to_owned(),
                                question: "Approve this plan and leave plan mode?".to_owned(),
                                detail: Some(args.plan.clone()),
                                header: Some("Plan review".to_owned()),
                                options: Some(vec![
                                    AskUserQuestionOption {
                                        label: APPROVE_LABEL.to_owned(),
                                        description: Some(
                                            "Leave plan mode; the plan is carried out from the next step."
                                                .to_owned(),
                                        ),
                                    },
                                    AskUserQuestionOption {
                                        label: KEEP_PLANNING_LABEL.to_owned(),
                                        description: Some(
                                            "Stay in plan mode; feedback goes back to the model."
                                                .to_owned(),
                                        ),
                                    },
                                ]),
                                multi_select: None,
                                intent: Some(AskUserQuestionIntent {
                                    kind: "plan-review".to_owned(),
                                    approve: APPROVE_LABEL.to_owned(),
                                    extra: serde_json::Map::new(),
                                }),
                            }],
                            agent: Some(agent.clone()),
                            signal: Some(exec.signal()),
                        };
                        let answer = match interaction.ask(request).await {
                            Ok(answer) => answer,
                            Err(error) => {
                                let cancelled = error
                                    .downcast_ref::<UserQuestionError>()
                                    .is_some_and(|error| error.code() == "ASK_CANCELLED");
                                if cancelled {
                                    anyhow::bail!("The user dismissed the plan review to speak instead; stay in plan mode, stop here, and wait for their message.");
                                }
                                return Err(error);
                            }
                        };
                        if controller.disposed.load(Ordering::Acquire) {
                            anyhow::bail!("the plan-mode service was reloaded while the plan was under review; present the plan again");
                        }
                        let review_items = answer
                            .answers
                            .iter()
                            .filter(|entry| entry.id == REVIEW_ID)
                            .collect::<Vec<_>>();
                        let item = if review_items.len() == 1 {
                            Some(review_items[0])
                        } else {
                            None
                        };
                        let approved = item.is_some_and(|item| {
                            item.selected.len() == 1
                                && item.selected[0] == APPROVE_LABEL
                                && item.custom.is_none()
                        });
                        if !approved {
                            let feedback = item.and_then(|item| item.custom.clone()).unwrap_or_default();
                            if feedback.is_empty() {
                                anyhow::bail!("The user chose to keep planning; revise the plan and present it again.");
                            }
                            anyhow::bail!("The user chose to keep planning; their feedback: {feedback}");
                        }
                        controller
                            .pending_intents
                            .lock()
                            .insert(session_key(agent.session()), PendingIntent {
                                active: false,
                                narrate: false,
                            });
                        Ok(ExitPlanModeOutcome { approved: true })
                    })
                }),
            )
            .present_call(Arc::new(|args: &ExitPlanModeArgs| {
                Some(ToolCallView::Generic(GenericCallView {
                    title: first_heading(&args.plan).unwrap_or_else(|| "Plan".to_owned()),
                    kind: Some(ToolCallKind::Other),
                    raw_input: None,
                    content: Some(vec![ContentBlock::Text { text: args.plan.clone() }]),
                    locations: None,
                }))
            }))
            .present_result(Arc::new(
                |_args: &ExitPlanModeArgs, result: &ToolResult| {
                    Some(ToolResultView::Generic(GenericResultView {
                        title: Some("Plan review".to_owned()),
                        content: Some(result.content.clone()),
                    }))
                },
            )),
        )?;
        let _ = tools.register(context, definition)?;
        Ok(())
    }

    fn register_dispose_effect(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let disposed = Arc::clone(&self.disposed);
        let effect = EffectHandle::synchronous("plan-mode: close service lifetime", move || {
            disposed.store(true, Ordering::Release);
            Ok(())
        });
        context.own(effect)?;
        Ok(())
    }
}

/// Builds the loader-compatible plan-mode plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config: PlanModeConfig = serde_json::from_value(config)?;
            PlanModeController::install(&context, &config)?;
            Ok(())
        })
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn valid_exit_plan_requires_heading_and_content() {
        assert!(valid_exit_plan("# My plan"));
        assert!(!valid_exit_plan("## Deep plan"));
        assert!(valid_exit_plan("  # Heading  "));
        assert!(!valid_exit_plan("no heading"));
        assert!(!valid_exit_plan("#"));
        assert!(!valid_exit_plan("#   "));
        assert!(!valid_exit_plan("##heading"));
    }

    #[test]
    fn plan_unit_state_round_trips_null_wanted() {
        let state = PlanUnitState {
            active: false,
            wanted: None,
        };
        let value = serde_json::to_value(state).expect("serialize");
        assert_eq!(value, json!({"active": false, "wanted": null}));
        let decoded: PlanUnitState = serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded.wanted, None);
    }

    fn event(event_type: &str, data: Value) -> SessionEvent {
        SessionEvent {
            event_type: event_type.to_owned(),
            seq: 0,
            time: 0,
            data,
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    #[test]
    fn fold_is_last_plan_mode_wins() {
        let events = vec![
            event("plan/mode", json!({"active": true})),
            event("plan/mode", json!({"active": false})),
            event("plan/mode", json!({"active": true})),
        ];
        assert!(fold_plan_mode(&events, events.len()));
        assert!(!fold_plan_mode(&events, 2));
        assert!(!fold_plan_mode(&[], 0));
    }

    #[test]
    fn first_heading_extracts_any_level() {
        assert_eq!(first_heading("# My plan\nrest"), Some("My plan".to_owned()));
        assert_eq!(
            first_heading("## Deep plan  \nmore"),
            Some("Deep plan".to_owned())
        );
        assert_eq!(first_heading("no heading"), None);
    }

    #[test]
    fn open_turn_detection_and_last_header() {
        let events = vec![
            event("turn/start", json!({})),
            event("plan/mode", json!({"active": true})),
            event("request/header", json!({})),
            event("plan/mode", json!({"active": false})),
        ];
        assert!(has_open_turn(&events));
        assert_eq!(plan_mode_at_last_header(&events), Some(true));
        let closed = vec![event("turn/start", json!({})), event("turn/end", json!({}))];
        assert!(!has_open_turn(&closed));
    }

    #[test]
    fn config_requires_non_empty_section() {
        assert!(
            resolve_config(&PlanModeConfig {
                section: "  ".to_owned()
            })
            .is_err()
        );
        let ok = resolve_config(&PlanModeConfig {
            section: "guidance".to_owned(),
        })
        .expect("ok");
        assert_eq!(ok.section, "guidance");
    }
}
