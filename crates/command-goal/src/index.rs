//! Human-facing `/goal` command over the persisted same-session goal domain.

use std::sync::Arc;

use seekdeep_commands::{COMMANDS, CommandDefinition, CommandInvocation, CommandResult};
use seekdeep_cordis::Context;
use seekdeep_goal::{
    CreateGoalRequest, EditGoalRequest, GOAL, GoalActivation, GoalPhase, GoalRef, GoalView,
    runtime::GoalError,
};

/// Cordis plugin name.
pub const NAME: &str = "command-goal";

/// Services required by the goal command.
pub const INJECT: &[&str] = &["commands", "goals"];

const USAGE: &str = "Usage: /goal [<objective>|clear|edit <objective>|pause|resume]";

const FRIENDLY_GOAL_ERROR: &str =
    "The goal command is not valid for the current state. Run /goal to view available commands.";

/// One parsed human command.
#[derive(Clone, Debug, PartialEq, Eq)]
enum GoalCommand {
    Show,
    Create { objective: String },
    Edit { objective: String },
    InvalidEdit,
    Pause,
    Resume,
    Clear,
}

/// Parses only the grammar owned by `/goal`; arbitrary other input is an objective.
fn parse_goal_command(raw_input: &str) -> GoalCommand {
    let input = raw_input.trim();
    if input.is_empty() {
        return GoalCommand::Show;
    }
    let lower = input.to_lowercase();
    match lower.as_str() {
        "clear" => return GoalCommand::Clear,
        "pause" => return GoalCommand::Pause,
        "resume" => return GoalCommand::Resume,
        "edit" => return GoalCommand::InvalidEdit,
        _ => {}
    }
    if lower.starts_with("edit") {
        let rest = &input[4..];
        if rest.chars().next().is_some_and(char::is_whitespace) {
            return GoalCommand::Edit {
                objective: rest.trim().to_owned(),
            };
        }
    }
    GoalCommand::Create {
        objective: input.to_owned(),
    }
}

/// Human label for one durable goal phase.
fn phase_label(phase: GoalPhase) -> &'static str {
    match phase {
        GoalPhase::Active => "active",
        GoalPhase::Paused => "paused",
        GoalPhase::Blocked => "blocked",
        GoalPhase::Complete => "complete",
    }
}

fn activation_label(activation: GoalActivation) -> &'static str {
    match activation {
        GoalActivation::Armed => "armed",
        GoalActivation::Disarmed => "disarmed",
    }
}

/// Commands that are meaningful from one exact live state.
fn command_hint(goal: &GoalView) -> String {
    if goal.phase == GoalPhase::Active {
        return if goal.activation == GoalActivation::Armed {
            "/goal edit <objective>, /goal pause, /goal clear".to_owned()
        } else {
            "/goal edit <objective>, /goal resume, /goal clear".to_owned()
        };
    }
    match goal.phase {
        GoalPhase::Paused | GoalPhase::Blocked => {
            "/goal edit <objective>, /goal resume, /goal clear".to_owned()
        }
        GoalPhase::Complete => "/goal <objective>, /goal clear".to_owned(),
        GoalPhase::Active => unreachable!(),
    }
}

/// Renders direct UI output without exposing compare-and-set internals.
fn render_goal(title: &str, goal: &GoalView) -> CommandResult {
    let mut lines = vec![
        title.to_owned(),
        format!("Status: {}", phase_label(goal.phase)),
    ];
    if goal.phase == GoalPhase::Blocked {
        let reason = goal
            .blocked_reason
            .as_ref()
            .expect("durable replay guarantees a blocked goal carries its reason");
        lines.push(format!("Blocker: {}: {}", reason.code, reason.message));
    }
    lines.push(format!("Objective: {}", goal.objective));
    lines.push(format!(
        "Rounds: {}/{}",
        goal.rounds_started, goal.max_goal_rounds
    ));
    lines.push(format!("Activation: {}", activation_label(goal.activation)));
    lines.push(String::new());
    lines.push(format!("Commands: {}", command_hint(goal)));
    CommandResult::success(Some(lines.join("\n")))
}

/// Exact current compare-and-set ref.
fn goal_ref(goal: &GoalView) -> GoalRef {
    GoalRef {
        id: goal.id.clone(),
        revision: goal.revision,
    }
}

fn missing_goal(action: &str) -> CommandResult {
    CommandResult::error(format!(
        "No goal is currently set; /goal {action} requires one. {USAGE}"
    ))
}

/// Executes one parsed human command through the goal domain.
fn execute_goal_command(
    ctx: &Context,
    invocation: &CommandInvocation,
) -> anyhow::Result<CommandResult> {
    let command = parse_goal_command(&invocation.raw_input);
    match execute_inner(ctx, invocation, command) {
        Ok(result) => Ok(result),
        Err(error) if error.downcast_ref::<GoalError>().is_some() => {
            Ok(CommandResult::error(FRIENDLY_GOAL_ERROR))
        }
        Err(error) => Err(error),
    }
}

fn execute_inner(
    ctx: &Context,
    invocation: &CommandInvocation,
    command: GoalCommand,
) -> anyhow::Result<CommandResult> {
    let goals = ctx
        .get(GOAL)
        .ok_or_else(|| anyhow::anyhow!("command-goal requires goals"))?;
    let current = goals.get(&invocation.agent)?;
    match command {
        GoalCommand::Show => Ok(match current {
            None => CommandResult::success(Some(format!("No goal is currently set.\n{USAGE}"))),
            Some(goal) => render_goal("Goal", &goal),
        }),
        GoalCommand::InvalidEdit => Ok(CommandResult::error(format!(
            "Goal editing requires a replacement objective.\n{USAGE}"
        ))),
        GoalCommand::Create { objective } => {
            if let Some(current) = &current
                && current.phase != GoalPhase::Complete
            {
                return Ok(CommandResult::error(format!(
                    "A goal is already {}. Use /goal edit <objective> to change it or /goal clear before replacing it.",
                    phase_label(current.phase)
                )));
            }
            let goal = goals.create(
                &invocation.agent,
                &CreateGoalRequest {
                    objective: objective.clone(),
                    max_goal_rounds: None,
                },
            )?;
            Ok(render_goal("Goal created", &goal))
        }
        GoalCommand::Edit { objective } => {
            let Some(current) = &current else {
                return Ok(missing_goal("edit"));
            };
            if current.phase == GoalPhase::Complete {
                let goal = goals.create(
                    &invocation.agent,
                    &CreateGoalRequest {
                        objective: objective.clone(),
                        max_goal_rounds: None,
                    },
                )?;
                return Ok(render_goal("Goal created", &goal));
            }
            let goal = goals.edit(
                &invocation.agent,
                &goal_ref(current),
                &EditGoalRequest {
                    objective: Some(objective.clone()),
                    max_goal_rounds: None,
                },
            )?;
            Ok(render_goal("Goal updated", &goal))
        }
        GoalCommand::Pause => {
            let Some(current) = &current else {
                return Ok(missing_goal("pause"));
            };
            let goal = goals.pause(&invocation.agent, &goal_ref(current))?;
            Ok(render_goal("Goal paused", &goal))
        }
        GoalCommand::Resume => {
            let Some(current) = &current else {
                return Ok(missing_goal("resume"));
            };
            let goal = goals.resume(&invocation.agent, &goal_ref(current))?;
            Ok(render_goal("Goal resumed", &goal))
        }
        GoalCommand::Clear => {
            let Some(current) = &current else {
                return Ok(CommandResult::success(Some("No goal to clear.".to_owned())));
            };
            let _ = goals.clear(&invocation.agent, &goal_ref(current))?;
            Ok(CommandResult::success(Some("Goal cleared.".to_owned())))
        }
    }
}

/// Registers the Codex-shaped `/goal` command for every composed command adapter.
///
/// # Errors
///
/// Returns missing-service or registration failures.
pub fn apply(context: &Context) -> anyhow::Result<()> {
    let commands = context
        .get(COMMANDS)
        .ok_or_else(|| anyhow::anyhow!("command-goal requires commands"))?;
    let handler_ctx = context.clone();
    let definition = CommandDefinition::new(
        "goal",
        "set or view the goal for a long-running task",
        Arc::new(move |invocation| {
            let ctx = handler_ctx.clone();
            Box::pin(async move { execute_goal_command(&ctx, &invocation) })
        }),
    )
    .with_input("[<objective>|clear|edit <objective>|pause|resume]");
    commands.register(context, definition)?;
    Ok(())
}

/// Builds the loader-compatible goal command plugin.
#[must_use]
pub fn plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, INJECT.iter().copied(), move |context, _config| {
        Box::pin(async move {
            apply(&context)?;
            Ok(())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_owned_grammar_and_objective_fallback() {
        assert_eq!(parse_goal_command(""), GoalCommand::Show);
        assert_eq!(parse_goal_command("  "), GoalCommand::Show);
        assert_eq!(parse_goal_command("clear"), GoalCommand::Clear);
        assert_eq!(parse_goal_command("CLEAR"), GoalCommand::Clear);
        assert_eq!(parse_goal_command("pause"), GoalCommand::Pause);
        assert_eq!(parse_goal_command("resume"), GoalCommand::Resume);
        assert_eq!(parse_goal_command("edit"), GoalCommand::InvalidEdit);
        assert_eq!(
            parse_goal_command("edit build the thing"),
            GoalCommand::Edit {
                objective: "build the thing".to_owned()
            }
        );
        assert_eq!(
            parse_goal_command("EDIT\tbuild"),
            GoalCommand::Edit {
                objective: "build".to_owned()
            }
        );
        assert_eq!(
            parse_goal_command("editability"),
            GoalCommand::Create {
                objective: "editability".to_owned()
            }
        );
        assert_eq!(
            parse_goal_command("just do the thing"),
            GoalCommand::Create {
                objective: "just do the thing".to_owned()
            }
        );
    }

    #[test]
    fn labels_phases_and_activations() {
        assert_eq!(phase_label(GoalPhase::Active), "active");
        assert_eq!(phase_label(GoalPhase::Paused), "paused");
        assert_eq!(phase_label(GoalPhase::Blocked), "blocked");
        assert_eq!(phase_label(GoalPhase::Complete), "complete");
        assert_eq!(activation_label(GoalActivation::Armed), "armed");
        assert_eq!(activation_label(GoalActivation::Disarmed), "disarmed");
    }
}
