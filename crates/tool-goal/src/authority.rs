//! Execution-time authority checks for the model-facing goal tools.

use std::sync::Arc;

use seekdeep_agent::{AGENTS, Agent, AgentStatus};
use seekdeep_cordis::Context;
use seekdeep_core::session::SessionEvent;
use seekdeep_goal::{GOAL, GoalView};
use seekdeep_llm::HarnessError;
use seekdeep_tools::ToolRunContext;

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
    resolve_goal_tool(ctx, exec, true)
}

/// Resolves a read-only goal query.
///
/// Reads do not require initiator authority. The documented workflow reads the goal before
/// updating it, and a nested tool call - a program dispatching `get_goal` - runs without a
/// current initiator, so requiring one makes the read unreachable. Liveness is still
/// required: a stale handle must not read registry state its caller no longer owns.
///
/// # Errors
///
/// Returns a structured authority failure when no calling agent is present or the caller is
/// not the exact live agent.
pub fn goal_tool_read(
    ctx: &Context,
    exec: &ToolRunContext,
) -> Result<GoalToolExecution, HarnessError> {
    resolve_goal_tool(ctx, exec, false)
}

fn resolve_goal_tool(
    ctx: &Context,
    exec: &ToolRunContext,
    require_initiator: bool,
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
    let mut unmet = Vec::new();
    if !live {
        unmet.push("the calling agent is not the live agent registered under its id");
    }
    if !running {
        unmet.push("the calling agent is not running");
    }
    if require_initiator && !initiator {
        unmet.push("the calling agent is not the current initiator");
    }
    if !unmet.is_empty() {
        // Name the unmet conditions: the caller cannot tell them apart from the outside, and
        // which one fails decides whether the fault is the caller's driver scope or the bridge
        // that dispatched the call.
        return reject(
            format!(
                "goal tools require the exact live calling agent inside its active driver: {}",
                unmet.join("; ")
            ),
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
                .and_then(|kind| kind.deserialize::<String>().ok())
                .as_deref()
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
        source
            .get("kind")
            .and_then(|kind| kind.deserialize::<String>().ok())
            .as_deref()
            == Some("goal")
            && source
                .get("goalId")
                .and_then(|id| id.deserialize::<String>().ok())
                .as_deref()
                == Some(goal.id.as_str())
            && source
                .get("revision")
                .and_then(seekdeep_core::session::JsonRef::as_u64)
                == Some(goal.revision)
            && source
                .get("round")
                .and_then(seekdeep_core::session::JsonRef::as_u64)
                == Some(goal.rounds_started)
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
