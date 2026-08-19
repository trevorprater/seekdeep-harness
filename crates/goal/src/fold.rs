//! Pure replay fold and strict decoder for durable goal changes.

use std::collections::HashSet;

use seekdeep_core::session::SessionEvent;
use seekdeep_llm::MessageSource;
use serde_json::{Map, Value};

use crate::domain::{
    FoldedGoal, GoalChangeMeta, GoalClearChangeMeta, GoalMessageSource, GoalOperation,
    GoalSnapshotChangeMeta, GoalSourceKind,
};
use crate::runtime::GOAL_CHANGE_VERSION;
use crate::types::{GoalBlockReason, GoalId, GoalPhase, GoalRef, GoalSnapshot};

/// Mutable accumulator kept private to the pure fold.
#[derive(Debug, Default)]
pub struct GoalFoldState {
    /// Current goal.
    pub goal: Option<GoalSnapshot>,
    /// Highest admitted round.
    pub rounds_started: u64,
    /// Create epoch milliseconds.
    pub created_at: Option<u64>,
    /// Mutation epoch milliseconds.
    pub updated_at: Option<u64>,
    /// Latest mutation ref.
    pub last_ref: Option<GoalRef>,
    /// Every goal id this fold has admitted.
    pub seen_goal_ids: HashSet<GoalId>,
}

/// Builds an empty replay accumulator.
#[must_use]
pub fn empty_goal_fold_state() -> GoalFoldState {
    GoalFoldState::default()
}

fn check_exact_keys(
    value: &Map<String, Value>,
    expected: &[&str],
    subject: &str,
) -> anyhow::Result<()> {
    let mut keys: Vec<&str> = value.keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut expected_sorted: Vec<&str> = expected.to_vec();
    expected_sorted.sort_unstable();
    if keys != expected_sorted {
        anyhow::bail!(
            "{subject} must have exactly {} fields",
            expected_sorted.join(",")
        );
    }
    Ok(())
}

fn positive_integer(value: &Value, field: &str) -> anyhow::Result<u64> {
    let Some(number) = value.as_u64() else {
        anyhow::bail!("goal change {field} must be a positive safe integer");
    };
    if number < 1 {
        anyhow::bail!("goal change {field} must be a positive safe integer");
    }
    Ok(number)
}

fn non_negative_integer(value: &Value, field: &str) -> anyhow::Result<u64> {
    let Some(number) = value.as_u64() else {
        anyhow::bail!("goal change {field} must be a non-negative safe integer");
    };
    Ok(number)
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

fn decode_block_reason(value: &Value) -> anyhow::Result<GoalBlockReason> {
    let Some(object) = value.as_object() else {
        anyhow::bail!("goal change goal.blockedReason must have exactly code and message fields");
    };
    check_exact_keys(
        object,
        &["code", "message"],
        "goal change goal.blockedReason",
    )?;
    let code = object.get("code").and_then(Value::as_str);
    let message = object.get("message").and_then(Value::as_str);
    let Some(code) = code else {
        anyhow::bail!("goal change goal.blockedReason.code must be lower-kebab-case");
    };
    if !is_lower_kebab(code) {
        anyhow::bail!("goal change goal.blockedReason.code must be lower-kebab-case");
    }
    let Some(message) = message else {
        anyhow::bail!("goal change goal.blockedReason.message must be non-empty and normalized");
    };
    if message.trim().is_empty() || message != message.trim() {
        anyhow::bail!("goal change goal.blockedReason.message must be non-empty and normalized");
    }
    Ok(GoalBlockReason {
        code: code.to_owned(),
        message: message.to_owned(),
    })
}

fn decode_snapshot(value: &Value) -> anyhow::Result<GoalSnapshot> {
    let Some(object) = value.as_object() else {
        anyhow::bail!("goal change goal must be a record");
    };
    let id = object.get("id").and_then(Value::as_str);
    let Some(id) = id else {
        anyhow::bail!("goal change goal.id must be a non-empty string");
    };
    if id.is_empty() {
        anyhow::bail!("goal change goal.id must be a non-empty string");
    }
    let objective = object.get("objective").and_then(Value::as_str);
    let Some(objective) = objective else {
        anyhow::bail!("goal change goal.objective must be non-empty and normalized");
    };
    if objective.trim().is_empty() || objective != objective.trim() {
        anyhow::bail!("goal change goal.objective must be non-empty and normalized");
    }
    let phase = object.get("phase").and_then(Value::as_str);
    let phase = match phase {
        Some("active") => GoalPhase::Active,
        Some("paused") => GoalPhase::Paused,
        Some("blocked") => GoalPhase::Blocked,
        Some("complete") => GoalPhase::Complete,
        _ => anyhow::bail!("goal change goal.phase is invalid"),
    };
    let expected: &[&str] = if phase == GoalPhase::Blocked {
        &[
            "blockedReason",
            "id",
            "maxGoalRounds",
            "objective",
            "phase",
            "revision",
        ]
    } else {
        &["id", "maxGoalRounds", "objective", "phase", "revision"]
    };
    check_exact_keys(
        object,
        expected,
        &format!("goal change goal for phase {phase:?}"),
    )?;
    let revision = positive_integer(
        object.get("revision").unwrap_or(&Value::Null),
        "goal.revision",
    )?;
    let max_goal_rounds = positive_integer(
        object.get("maxGoalRounds").unwrap_or(&Value::Null),
        "goal.maxGoalRounds",
    )?;
    let blocked_reason = if phase == GoalPhase::Blocked {
        Some(decode_block_reason(
            object.get("blockedReason").unwrap_or(&Value::Null),
        )?)
    } else {
        None
    };
    Ok(GoalSnapshot {
        id: GoalId::new(id),
        revision,
        objective: objective.to_owned(),
        phase,
        blocked_reason,
        max_goal_rounds,
    })
}

fn decode_ref(value: &Value) -> anyhow::Result<GoalRef> {
    let Some(object) = value.as_object() else {
        anyhow::bail!("goal clear tombstone must have exactly id and revision fields");
    };
    check_exact_keys(object, &["id", "revision"], "goal clear tombstone")?;
    let id = object.get("id").and_then(Value::as_str);
    let Some(id) = id else {
        anyhow::bail!("goal clear tombstone id must be a non-empty string");
    };
    if id.is_empty() {
        anyhow::bail!("goal clear tombstone id must be a non-empty string");
    }
    Ok(GoalRef {
        id: GoalId::new(id),
        revision: positive_integer(
            object.get("revision").unwrap_or(&Value::Null),
            "cleared.revision",
        )?,
    })
}

/// Decodes a value that declares itself as a goal change.
///
/// # Errors
///
/// Returns a malformed-goal-change failure; unrelated values return none.
pub fn decode_goal_change(value: &Value) -> anyhow::Result<Option<GoalChangeMeta>> {
    let Some(object) = value.as_object() else {
        return Ok(None);
    };
    if object.get("kind").and_then(Value::as_str) != Some("goal/change") {
        return Ok(None);
    }
    if object.get("version").and_then(Value::as_u64) != Some(u64::from(GOAL_CHANGE_VERSION)) {
        anyhow::bail!(
            "unsupported goal change version {}",
            object
                .get("version")
                .map_or_else(|| "undefined".to_owned(), std::string::ToString::to_string)
        );
    }
    let operation = object.get("operation").and_then(Value::as_str);
    if operation == Some("clear") {
        check_exact_keys(
            object,
            &["cleared", "clearedAt", "kind", "operation", "version"],
            "goal clear change",
        )?;
        return Ok(Some(GoalChangeMeta::Clear(GoalClearChangeMeta {
            kind: crate::domain::GoalChangeKind::GoalChange,
            version: GOAL_CHANGE_VERSION,
            operation: crate::domain::GoalClearOperation::Clear,
            cleared: decode_ref(object.get("cleared").unwrap_or(&Value::Null))?,
            cleared_at: non_negative_integer(
                object.get("clearedAt").unwrap_or(&Value::Null),
                "clearedAt",
            )?,
        })));
    }
    let Some(operation) = operation else {
        anyhow::bail!("goal change operation is invalid");
    };
    let operation = match operation {
        "create" => GoalOperation::Create,
        "edit" => GoalOperation::Edit,
        "pause" => GoalOperation::Pause,
        "resume" => GoalOperation::Resume,
        "complete" => GoalOperation::Complete,
        "block" => GoalOperation::Block,
        _ => anyhow::bail!("goal change operation is invalid"),
    };
    check_exact_keys(
        object,
        &[
            "createdAt",
            "goal",
            "kind",
            "operation",
            "roundsStarted",
            "updatedAt",
            "version",
        ],
        "goal snapshot change",
    )?;
    let created_at =
        non_negative_integer(object.get("createdAt").unwrap_or(&Value::Null), "createdAt")?;
    let updated_at =
        non_negative_integer(object.get("updatedAt").unwrap_or(&Value::Null), "updatedAt")?;
    if updated_at < created_at {
        anyhow::bail!("goal change updatedAt cannot precede createdAt");
    }
    Ok(Some(GoalChangeMeta::Snapshot(GoalSnapshotChangeMeta {
        kind: crate::domain::GoalChangeKind::GoalChange,
        version: GOAL_CHANGE_VERSION,
        operation,
        goal: decode_snapshot(object.get("goal").unwrap_or(&Value::Null))?,
        rounds_started: non_negative_integer(
            object.get("roundsStarted").unwrap_or(&Value::Null),
            "roundsStarted",
        )?,
        created_at,
        updated_at,
    })))
}

fn goal_source(source: &MessageSource) -> anyhow::Result<Option<GoalMessageSource>> {
    if source.kind != "goal" {
        return Ok(None);
    }
    let goal_id = source.fields.get("goalId").and_then(Value::as_str);
    let revision = source.fields.get("revision").and_then(Value::as_u64);
    let round = source.fields.get("round").and_then(Value::as_u64);
    if goal_id.is_none_or(str::is_empty)
        || revision.is_none_or(|value| value < 1)
        || round.is_none_or(|value| value < 1)
    {
        anyhow::bail!("goal message source is invalid");
    }
    Ok(Some(GoalMessageSource {
        kind: GoalSourceKind::Goal,
        goal_id: GoalId::new(goal_id.expect("non-empty")),
        revision: revision.expect("positive"),
        round: round.expect("positive"),
    }))
}

fn require_same_definition(
    current: &GoalSnapshot,
    next: &GoalSnapshot,
    operation: GoalOperation,
) -> anyhow::Result<()> {
    if next.objective != current.objective || next.max_goal_rounds != current.max_goal_rounds {
        anyhow::bail!("goal {operation:?} cannot change objective or maxGoalRounds");
    }
    Ok(())
}

fn require_next_revision(
    current: &GoalSnapshot,
    next_id: &GoalId,
    next_revision: u64,
    operation: GoalOperation,
) -> anyhow::Result<()> {
    if next_id != &current.id || next_revision != current.revision + 1 {
        anyhow::bail!("goal {operation:?} must advance the current goal by one revision");
    }
    Ok(())
}

fn validate_snapshot_transition(
    state: &GoalFoldState,
    change: &GoalSnapshotChangeMeta,
    current: &GoalSnapshot,
) -> anyhow::Result<()> {
    let next = &change.goal;
    require_next_revision(current, &next.id, next.revision, change.operation)?;
    let Some(updated_at) = state.updated_at else {
        anyhow::bail!("current goal fold lacks updatedAt");
    };
    if change.created_at != state.created_at.unwrap_or(0)
        || change.updated_at < updated_at
        || change.rounds_started != state.rounds_started
    {
        anyhow::bail!(
            "goal {:?} does not preserve the current counters and timestamps",
            change.operation
        );
    }
    match change.operation {
        GoalOperation::Edit => {
            if next.phase != current.phase || next.blocked_reason != current.blocked_reason {
                anyhow::bail!("goal edit cannot change phase or blocked reason");
            }
        }
        GoalOperation::Pause => {
            require_same_definition(current, next, change.operation)?;
            if current.phase != GoalPhase::Active || next.phase != GoalPhase::Paused {
                anyhow::bail!("goal pause has an invalid phase transition");
            }
        }
        GoalOperation::Resume => {
            require_same_definition(current, next, change.operation)?;
            let resumable = matches!(
                current.phase,
                GoalPhase::Active | GoalPhase::Paused | GoalPhase::Blocked
            );
            if !resumable
                || next.phase != GoalPhase::Active
                || state.rounds_started >= next.max_goal_rounds
            {
                anyhow::bail!(
                    "goal resume has an invalid phase transition or exhausted round budget"
                );
            }
        }
        GoalOperation::Complete => {
            require_same_definition(current, next, change.operation)?;
            if current.phase == GoalPhase::Complete || next.phase != GoalPhase::Complete {
                anyhow::bail!("goal complete has an invalid phase transition");
            }
        }
        GoalOperation::Block => {
            require_same_definition(current, next, change.operation)?;
            if current.phase != GoalPhase::Active || next.phase != GoalPhase::Blocked {
                anyhow::bail!("goal block has an invalid phase transition");
            }
        }
        GoalOperation::Create => {
            anyhow::bail!("goal create cannot be validated as a current-goal transition")
        }
        GoalOperation::Clear => anyhow::bail!("unknown goal snapshot operation"),
    }
    Ok(())
}

/// Returns the revision identity carried by a snapshot or tombstone.
#[must_use]
pub fn goal_change_ref(change: &GoalChangeMeta) -> GoalRef {
    match change {
        GoalChangeMeta::Clear(clear) => clear.cleared.clone(),
        GoalChangeMeta::Snapshot(snapshot) => GoalRef {
            id: snapshot.goal.id.clone(),
            revision: snapshot.goal.revision,
        },
    }
}

/// Validates and applies one decoded change to a mutable accumulator.
///
/// # Errors
///
/// Returns a transition-validation failure.
pub fn apply_goal_change(state: &mut GoalFoldState, change: &GoalChangeMeta) -> anyhow::Result<()> {
    let reference = goal_change_ref(change);
    match change {
        GoalChangeMeta::Clear(clear) => {
            let Some(current) = &state.goal else {
                anyhow::bail!("goal clear requires a current goal");
            };
            require_next_revision(
                current,
                &clear.cleared.id,
                clear.cleared.revision,
                GoalOperation::Clear,
            )?;
            let Some(updated_at) = state.updated_at else {
                anyhow::bail!("current goal fold lacks updatedAt");
            };
            if clear.cleared_at < updated_at {
                anyhow::bail!("goal clear timestamp cannot precede the current goal update");
            }
            state.goal = None;
            state.rounds_started = 0;
            state.created_at = None;
            state.updated_at = None;
            state.last_ref = Some(reference);
        }
        GoalChangeMeta::Snapshot(snapshot) => {
            if snapshot.operation == GoalOperation::Create {
                if snapshot.goal.revision != 1
                    || snapshot.goal.phase != GoalPhase::Active
                    || snapshot.rounds_started != 0
                    || state
                        .goal
                        .as_ref()
                        .is_some_and(|goal| goal.phase != GoalPhase::Complete)
                    || state.seen_goal_ids.contains(&snapshot.goal.id)
                {
                    anyhow::bail!(
                        "goal create requires a fresh active revision-one goal with zero rounds"
                    );
                }
                state.seen_goal_ids.insert(snapshot.goal.id.clone());
            } else {
                let Some(current) = &state.goal else {
                    anyhow::bail!("goal {:?} requires a current goal", snapshot.operation);
                };
                validate_snapshot_transition(state, snapshot, current)?;
            }
            state.goal = Some(snapshot.goal.clone());
            state.rounds_started = snapshot.rounds_started;
            state.created_at = Some(snapshot.created_at);
            state.updated_at = Some(snapshot.updated_at);
            state.last_ref = Some(reference);
        }
    }
    Ok(())
}

/// Applies one session event to the strict durable goal fold.
///
/// # Errors
///
/// Returns a malformed-change or round-admission failure.
pub fn apply_goal_event(state: &mut GoalFoldState, event: &SessionEvent) -> anyhow::Result<()> {
    if event.event_type == "goal/change" {
        let change = decode_goal_change(&event.data)?;
        let Some(change) = change else {
            anyhow::bail!(
                "goal change at session event {} has an invalid kind",
                event.seq
            );
        };
        apply_goal_change(state, &change)?;
        return Ok(());
    }
    if event.event_type == "user/message" {
        let source: MessageSource =
            serde_json::from_value(event.data.get("source").cloned().unwrap_or(Value::Null))
                .map_err(|_| anyhow::anyhow!("goal message source is invalid"))?;
        let Some(source) = goal_source(&source)? else {
            return Ok(());
        };
        let current = &state.goal;
        let valid = current.as_ref().is_some_and(|current| {
            current.phase == GoalPhase::Active
                && source.goal_id == current.id
                && source.revision == current.revision
                && source.round == state.rounds_started + 1
                && source.round <= current.max_goal_rounds
        });
        if !valid {
            anyhow::bail!(
                "goal round at session event {} is not the next admitted round of the active goal",
                event.seq
            );
        }
        state.rounds_started = source.round;
    }
    Ok(())
}

/// Folds current goal state from a contiguous session event log.
///
/// # Errors
///
/// Returns the first malformed or invalid change.
pub fn fold_goal(events: &[SessionEvent]) -> anyhow::Result<FoldedGoal> {
    let mut state = empty_goal_fold_state();
    for event in events {
        apply_goal_event(&mut state, event)?;
    }
    Ok(FoldedGoal {
        goal: state.goal,
        rounds_started: state.rounds_started,
        created_at: state.created_at,
        updated_at: state.updated_at,
        last_ref: state.last_ref,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn event(event_type: &str, seq: u64, data: Value) -> SessionEvent {
        SessionEvent {
            event_type: event_type.to_owned(),
            seq,
            time: 0,
            data,
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    fn create(id: &str, seq: u64, at: u64) -> SessionEvent {
        event(
            "goal/change",
            seq,
            json!({
                "kind": "goal/change",
                "version": 1,
                "operation": "create",
                "goal": {"id": id, "revision": 1, "objective": "port it", "phase": "active", "maxGoalRounds": 10},
                "roundsStarted": 0,
                "createdAt": at,
                "updatedAt": at,
            }),
        )
    }

    #[test]
    fn folds_create_then_complete() {
        let events = vec![
            create("g1", 0, 100),
            event(
                "goal/change",
                1,
                json!({
                    "kind": "goal/change",
                    "version": 1,
                    "operation": "complete",
                    "goal": {"id": "g1", "revision": 2, "objective": "port it", "phase": "complete", "maxGoalRounds": 10},
                    "roundsStarted": 0,
                    "createdAt": 100,
                    "updatedAt": 200,
                }),
            ),
        ];
        let folded = fold_goal(&events).expect("fold");
        assert_eq!(
            folded.goal.as_ref().map(|goal| goal.phase),
            Some(GoalPhase::Complete)
        );
        assert_eq!(folded.rounds_started, 0);
        assert_eq!(
            folded.last_ref.as_ref().map(|reference| reference.revision),
            Some(2)
        );
    }

    #[test]
    fn rejects_edit_that_changes_objective() {
        let events = vec![
            create("g1", 0, 100),
            event(
                "goal/change",
                1,
                json!({
                    "kind": "goal/change",
                    "version": 1,
                    "operation": "edit",
                    "goal": {"id": "g1", "revision": 2, "objective": "changed", "phase": "active", "maxGoalRounds": 10},
                    "roundsStarted": 0,
                    "createdAt": 100,
                    "updatedAt": 200,
                }),
            ),
        ];
        assert!(fold_goal(&events).is_err());
    }
}
