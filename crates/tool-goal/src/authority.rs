//! Execution-time authority checks for the model-facing goal tools.

use std::sync::Arc;

use seekdeep_agent::{AGENTS, Agent, AgentStatus};
use seekdeep_cordis::Context;
use seekdeep_core::session::SessionEvent;
use seekdeep_goal::{GOAL, GoalView};
use seekdeep_llm::HarnessError;
use seekdeep_tools::ToolRunContext;
use serde_json::Value;

/// Current open turn plus the events accepted after its start boundary.
#[derive(Clone, Debug)]
pub struct GoalToolExecution {
    /// Authenticated live calling agent.
    pub agent: Arc<Agent>,
    /// The `turn/start` boundary enclosing the call.
    pub start: SessionEvent,
    /// Events accepted after the start boundary.
    pub events: Vec<SessionEvent>,
}

/// Hard authority granted to one state-changing call.
#[derive(Clone, Debug)]
pub enum GoalToolAuthority {
    /// A direct human turn on a runtime root.
    DirectHuman,
    /// The current goal's exact admitted round.
    GoalRound {
        /// The matching live goal.
        goal: GoalView,
    },
}

fn reject<T>(message: impl Into<String>, code: impl Into<String>) -> Result<T, HarnessError> {
    Err(HarnessError::new(message, code))
}

fn open_turn(agent: &Arc<Agent>) -> Result<(SessionEvent, Vec<SessionEvent>), HarnessError> {
    let events = agent.session().events();
    for (index, event) in events.iter().enumerate().rev() {
        if event.event_type == "turn/end" {
            return reject(
                "goal tools require an open model turn",
                "GOAL_TOOL_DRIVER_REQUIRED",
            );
        }
        if event.event_type == "turn/start" {
            return Ok((event.clone(), events[index + 1..].to_vec()));
        }
    }
    reject(
        "goal tools require an open model turn",
        "GOAL_TOOL_DRIVER_REQUIRED",
    )
}

/// Resolves and authenticates the calling agent and its driver boundary.
///
/// # Errors
///
/// Returns a structured authority failure when no calling agent is present or
/// the caller is not the exact live agent inside its active driver.
pub fn goal_tool_execution(
    ctx: &Context,
    exec: &ToolRunContext,
) -> Result<GoalToolExecution, HarnessError> {
    let Some(agent) = exec.agent.as_ref() else {
        return reject(
            "goal tools require a calling agent",
            "GOAL_TOOL_AGENT_REQUIRED",
        );
    };
    let agents = ctx.get(AGENTS).ok_or_else(|| {
        HarnessError::new(
            "goal tools require the agent registry",
            "GOAL_TOOL_DRIVER_REQUIRED",
        )
    })?;
    let live = agents
        .get(agent.id())
        .is_some_and(|live| Arc::ptr_eq(&live, agent));
    let running = agent.status() == AgentStatus::Running;
    let initiator = agents.current_initiator().is_ok_and(|initiator| {
        initiator
            .as_ref()
            .is_some_and(|live| Arc::ptr_eq(live, agent))
    });
    if !live || !running || !initiator {
        return reject(
            "goal tools require the exact live calling agent inside its active driver",
            "GOAL_TOOL_DRIVER_REQUIRED",
        );
    }
    let (start, events) = open_turn(agent)?;
    Ok(GoalToolExecution {
        agent: agent.clone(),
        start,
        events,
    })
}

fn has_direct_human_input(ctx: &Context, execution: &GoalToolExecution) -> bool {
    let Some(agents) = ctx.get(AGENTS) else {
        return false;
    };
    let is_root = agents
        .roots()
        .iter()
        .any(|root| Arc::ptr_eq(root, &execution.agent));
    if !is_root {
        return false;
    }
    execution.events.iter().any(|event| {
        event.event_type == "user/message"
            && event
                .data
                .get("source")
                .and_then(|source| source.get("kind"))
                .and_then(Value::as_str)
                == Some("user")
    })
}

fn is_matching_goal_round(execution: &GoalToolExecution, goal: &GoalView) -> bool {
    execution.events.iter().any(|event| {
        if event.event_type != "user/message" {
            return false;
        }
        let Some(source) = event.data.get("source") else {
            return false;
        };
        source.get("kind").and_then(Value::as_str) == Some("goal")
            && source.get("goalId").and_then(Value::as_str) == Some(goal.id.as_str())
            && source.get("revision").and_then(Value::as_u64) == Some(goal.revision)
            && source.get("round").and_then(Value::as_u64) == Some(goal.rounds_started)
    })
}

/// Requires authority originating in a human message accepted by a runtime root.
///
/// # Errors
///
/// Returns a structured authority failure when the current root turn carries
/// no human-authored message.
pub fn require_direct_human(
    ctx: &Context,
    execution: &GoalToolExecution,
) -> Result<(), HarnessError> {
    if has_direct_human_input(ctx, execution) {
        return Ok(());
    }
    reject(
        "this goal operation requires a direct human turn on a top-level agent",
        "GOAL_TOOL_AUTHORITY_REQUIRED",
    )
}

/// Resolves completion authority from direct human input or the exact goal round.
///
/// # Errors
///
/// Returns a structured authority failure when neither a direct human turn
/// nor the current goal's exact admitted round grants the operation.
pub fn completion_authority(
    ctx: &Context,
    execution: &GoalToolExecution,
) -> Result<GoalToolAuthority, HarnessError> {
    if has_direct_human_input(ctx, execution) {
        return Ok(GoalToolAuthority::DirectHuman);
    }
    let goals = ctx.get(GOAL).ok_or_else(|| {
        HarnessError::new(
            "goal tools require the goal registry",
            "GOAL_TOOL_DRIVER_REQUIRED",
        )
    })?;
    let goal = goals
        .get(&execution.agent)
        .map_err(|error| HarnessError::new(format!("{error}"), "GOAL_TOOL_DRIVER_REQUIRED"))?;
    if let Some(goal) = goal
        && is_matching_goal_round(execution, &goal)
    {
        return Ok(GoalToolAuthority::GoalRound { goal });
    }
    reject(
        "complete and blocked require a direct human turn or the current goal round",
        "GOAL_TOOL_AUTHORITY_REQUIRED",
    )
}
