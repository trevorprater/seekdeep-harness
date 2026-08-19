//! Same-session goal domain: event-sourced state and projection fold.

use seekdeep_core::session::SessionEvent;
use serde_json::Value;

use crate::domain::{GoalChangeMeta, GoalErrorCode};
use crate::fold::decode_goal_change;
use crate::runtime::GoalError;
use crate::types::{CreateGoalRequest, GoalBlockReason, GoalProjection};

/// Deployment default for goal creation.
pub const DEFAULT_MAX_GOAL_ROUNDS: u64 = 256;

/// Deployment defaults for goal creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    /// Total rounds used when a create request omits its own cap.
    pub default_max_goal_rounds: Option<u64>,
}

/// Resolved defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedConfig {
    /// Validated positive default round cap.
    pub default_max_goal_rounds: u64,
}

/// Validated create input with every deployment default materialized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedCreateGoal {
    /// Normalized objective.
    pub objective: String,
    /// Materialized round cap.
    pub max_goal_rounds: u64,
}

fn is_lower_kebab(value: &str) -> bool {
    !value.is_empty()
        && value.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// Validates a caller-visible positive round cap.
///
/// # Errors
///
/// Returns an invalid-round-cap failure.
pub fn resolve_max_goal_rounds(value: u64) -> Result<u64, GoalError> {
    if value < 1 {
        return Err(GoalError::new(
            "maxGoalRounds must be a positive safe integer",
            GoalErrorCode::GoalInvalidMaxRounds,
        ));
    }
    Ok(value)
}

/// Validates and normalizes an objective at the domain boundary.
///
/// # Errors
///
/// Returns an invalid-objective failure.
pub fn resolve_objective(value: &str) -> Result<String, GoalError> {
    if value.trim().is_empty() {
        return Err(GoalError::new(
            "goal objective must be a non-empty string",
            GoalErrorCode::GoalInvalidObjective,
        ));
    }
    Ok(value.trim().to_owned())
}

/// Materializes deployment defaults and validates one create request.
///
/// # Errors
///
/// Returns an invalid-objective or invalid-round-cap failure.
pub fn resolve_create_goal(
    request: &CreateGoalRequest,
    default_max_goal_rounds: u64,
) -> Result<ResolvedCreateGoal, GoalError> {
    Ok(ResolvedCreateGoal {
        objective: resolve_objective(&request.objective)?,
        max_goal_rounds: resolve_max_goal_rounds(
            request.max_goal_rounds.unwrap_or(default_max_goal_rounds),
        )?,
    })
}

/// Validates and detaches one policy-owned blocker explanation.
///
/// # Errors
///
/// Returns an invalid-block-reason failure.
pub fn resolve_block_reason(reason: &Value) -> Result<GoalBlockReason, GoalError> {
    let object = reason.as_object();
    let code = object
        .and_then(|object| object.get("code"))
        .and_then(Value::as_str);
    let message = object
        .and_then(|object| object.get("message"))
        .and_then(Value::as_str);
    let Some(code) = code.filter(|code| is_lower_kebab(code)) else {
        return Err(GoalError::new(
            "goal block reason requires a lower-kebab-case code and a non-empty message",
            GoalErrorCode::GoalInvalidBlockReason,
        ));
    };
    let Some(message) = message.filter(|message| !message.trim().is_empty()) else {
        return Err(GoalError::new(
            "goal block reason requires a lower-kebab-case code and a non-empty message",
            GoalErrorCode::GoalInvalidBlockReason,
        ));
    };
    Ok(GoalBlockReason {
        code: code.to_owned(),
        message: message.trim().to_owned(),
    })
}

/// Light last-wins fold of the goal projection unit.
#[must_use]
pub fn apply_goal_projection(
    state: Option<GoalProjection>,
    event: &SessionEvent,
) -> Option<GoalProjection> {
    if event.event_type != "goal/change" {
        return state;
    }
    let Some(change) = decode_goal_change(&event.data).ok().flatten() else {
        return state;
    };
    match change {
        GoalChangeMeta::Clear(_) => None,
        GoalChangeMeta::Snapshot(snapshot) => Some(GoalProjection {
            goal: snapshot.goal,
            rounds_started: snapshot.rounds_started,
            created_at: snapshot.created_at,
            updated_at: snapshot.updated_at,
        }),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::types::{GoalId, GoalPhase, GoalSnapshot};

    fn snapshot() -> GoalSnapshot {
        GoalSnapshot {
            id: GoalId::new("g1"),
            revision: 1,
            objective: "port it".to_owned(),
            phase: GoalPhase::Active,
            blocked_reason: None,
            max_goal_rounds: 10,
        }
    }

    #[test]
    fn projection_folds_snapshot_and_clear() {
        let create = SessionEvent {
            event_type: "goal/change".to_owned(),
            seq: 0,
            time: 0,
            data: json!({
                "kind": "goal/change", "version": 1, "operation": "create",
                "goal": {"id": "g1", "revision": 1, "objective": "port it", "phase": "active", "maxGoalRounds": 10},
                "roundsStarted": 0, "createdAt": 100, "updatedAt": 100,
            }),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        };
        let projected = apply_goal_projection(None, &create).expect("projected");
        assert_eq!(projected.goal.id, snapshot().id);
        assert_eq!(projected.rounds_started, 0);

        let clear = SessionEvent {
            event_type: "goal/change".to_owned(),
            seq: 1,
            time: 0,
            data: json!({
                "kind": "goal/change", "version": 1, "operation": "clear",
                "cleared": {"id": "g1", "revision": 2}, "clearedAt": 200,
            }),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        };
        assert!(apply_goal_projection(Some(projected), &clear).is_none());
    }

    #[test]
    fn validates_objective_and_round_cap() {
        assert!(resolve_objective("  ").is_err());
        assert!(resolve_max_goal_rounds(0).is_err());
        let resolved = resolve_create_goal(
            &CreateGoalRequest {
                objective: "  port it  ".to_owned(),
                max_goal_rounds: None,
            },
            256,
        )
        .expect("resolve");
        assert_eq!(resolved.objective, "port it");
        assert_eq!(resolved.max_goal_rounds, 256);
    }

    #[test]
    fn validates_block_reason() {
        assert!(resolve_block_reason(&json!({"code": "Bad-Code", "message": "x"})).is_err());
        assert!(resolve_block_reason(&json!({"code": "ok", "message": "  "})).is_err());
        let reason = resolve_block_reason(&json!({"code": "needs-approval", "message": "  hi  "}))
            .expect("valid");
        assert_eq!(reason.code, "needs-approval");
        assert_eq!(reason.message, "hi");
    }
}
