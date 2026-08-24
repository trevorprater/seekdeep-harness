//! Package-owned goal-round prompt invariants.

use std::sync::Arc;

use seekdeep_cordis::{DispatchMode, EventArgs, EventOptions, EventReply};
use seekdeep_core::{
    session::{Session, SessionEvent},
    session_store::SESSIONS,
};
use seekdeep_goal::fold::fold_goal;
use seekdeep_goal::{
    FoldedGoal, GoalActivation, GoalId, GoalMessageSource, GoalPhase, GoalSourceKind, GoalView,
};
use seekdeep_invariants::{
    InvariantFailure, InvariantInstaller, InvariantRegistration, InvariantRegistry,
};
use seekdeep_llm::{ContentBlock, MessageSource};
use serde_json::Value;

use crate::prompt::render_goal_round_prompt;

/// Package name reserved by this companion.
pub const PACKAGE_NAME: &str = "seekdeep-goal-round-driver";

/// Cordis companion plugin name.
pub const NAME: &str = "goal-round-driver-invariant";

/// Services required before the companion can reserve package ownership.
pub const INJECT: &[&str] = &["invariants"];

fn fail_invariant(fail: &InvariantFailure, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::from(fail.fail(message))
}

fn fold_checked(events: &[SessionEvent], fail: &InvariantFailure) -> anyhow::Result<FoldedGoal> {
    fold_goal(events).map_err(|error| {
        fail_invariant(
            fail,
            format!("cannot reconstruct the goal before a continuation message: {error}"),
        )
    })
}

fn goal_source(source: &MessageSource) -> Option<GoalMessageSource> {
    if source.kind != "goal" {
        return None;
    }
    let goal_id = source.fields.get("goalId").and_then(Value::as_str)?;
    let revision = source.fields.get("revision").and_then(Value::as_u64)?;
    let round = source.fields.get("round").and_then(Value::as_u64)?;
    if goal_id.is_empty() || revision < 1 || round < 1 {
        return None;
    }
    Some(GoalMessageSource {
        kind: GoalSourceKind::Goal,
        goal_id: GoalId::new(goal_id),
        revision,
        round,
    })
}

fn goal_view(
    folded: &FoldedGoal,
    source: &GoalMessageSource,
    fail: &InvariantFailure,
) -> anyhow::Result<GoalView> {
    let Some(goal) = &folded.goal else {
        return Err(fail_invariant(
            fail,
            format!(
                "goal round {} cannot be reconstructed from the preceding durable goal state",
                source.round
            ),
        ));
    };
    let (Some(created_at), Some(updated_at)) = (folded.created_at, folded.updated_at) else {
        return Err(fail_invariant(
            fail,
            format!(
                "goal round {} cannot be reconstructed from the preceding durable goal state",
                source.round
            ),
        ));
    };
    if goal.phase != GoalPhase::Active
        || goal.id != source.goal_id
        || goal.revision != source.revision
        || source.round != folded.rounds_started + 1
        || source.round > goal.max_goal_rounds
    {
        return Err(fail_invariant(
            fail,
            format!(
                "goal round {} cannot be reconstructed from the preceding durable goal state",
                source.round
            ),
        ));
    }
    Ok(GoalView {
        id: goal.id.clone(),
        revision: goal.revision,
        objective: goal.objective.clone(),
        phase: goal.phase,
        blocked_reason: goal.blocked_reason.clone(),
        max_goal_rounds: goal.max_goal_rounds,
        rounds_started: folded.rounds_started,
        created_at,
        updated_at,
        activation: GoalActivation::Armed,
    })
}

fn validate_event(
    prior: &[SessionEvent],
    event: &SessionEvent,
    fail: &InvariantFailure,
) -> anyhow::Result<()> {
    if event.event_type != "user/message" {
        return Ok(());
    }
    let source: MessageSource =
        match serde_json::from_value(event.data.get("source").cloned().unwrap_or(Value::Null)) {
            Ok(source) => source,
            Err(_) => return Ok(()),
        };
    let Some(source) = goal_source(&source) else {
        return Ok(());
    };
    let expected = render_goal_round_prompt(
        &goal_view(&fold_checked(prior, fail)?, &source, fail)?,
        source.round,
    );
    let content: Vec<ContentBlock> =
        serde_json::from_value(event.data.get("content").cloned().unwrap_or(Value::Null))
            .unwrap_or_default();
    if content != expected {
        return Err(fail_invariant(
            fail,
            format!(
                "goal round {} content does not match the package-owned continuation prompt",
                source.round
            ),
        ));
    }
    Ok(())
}

fn global_events() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}

/// Registers the goal-round-driver invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant registration or installer failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(["sessions"], |context, failure| async move {
            let sessions = context
                .get(SESSIONS)
                .ok_or_else(|| anyhow::anyhow!("goal-round-driver invariant requires sessions"))?;
            for session in sessions.list() {
                let mut prior = Vec::new();
                for event in session.events() {
                    validate_event(&prior, &event, &failure)?;
                    prior.push(event);
                }
            }

            let dispatch_failure = failure.clone();
            context.events().on_sync(
                &context,
                "internal/dispatch",
                move |_, args| {
                    args.get::<DispatchMode>(0).ok_or_else(|| {
                        anyhow::anyhow!("internal/dispatch lacks a dispatch mode")
                    })?;
                    let event_name = args
                        .get::<String>(1)
                        .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks an event name"))?;
                    let event_args = args.get::<EventArgs>(2).ok_or_else(|| {
                        anyhow::anyhow!("internal/dispatch lacks event arguments")
                    })?;
                    if event_name.as_str() != "session/event" {
                        return Ok(EventReply::Undefined);
                    }
                    let session = event_args
                        .get::<Session>(0)
                        .ok_or_else(|| anyhow::anyhow!("session/event lacks its session"))?;
                    let event = event_args
                        .get::<SessionEvent>(1)
                        .ok_or_else(|| anyhow::anyhow!("session/event lacks its event"))?;
                    validate_event(&session.events(), &event, &dispatch_failure)?;
                    Ok(EventReply::Undefined)
                },
                global_events(),
            )?;
            Ok(())
        }),
    )
}
