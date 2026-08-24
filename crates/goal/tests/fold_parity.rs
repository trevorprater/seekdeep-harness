//! Strict replay matrix mirrored from `packages/goal/goal/tests/goal.spec.ts`.

use seekdeep_core::session::SessionEvent;
use seekdeep_goal::fold::{decode_goal_change, fold_goal};
use serde_json::{Value, json};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn event(event_type: &str, seq: u64, data: Value) -> SessionEvent {
    SessionEvent {
        event_type: event_type.to_owned(),
        seq,
        time: i64::try_from(seq).unwrap_or(i64::MAX),
        data,
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn snapshot_change(
    operation: &str,
    id: &str,
    revision: u64,
    objective: &str,
    phase: &str,
    max_goal_rounds: u64,
    rounds_started: u64,
    created_at: u64,
    updated_at: u64,
) -> Value {
    let mut goal = json!({
        "id": id,
        "revision": revision,
        "objective": objective,
        "phase": phase,
        "maxGoalRounds": max_goal_rounds,
    });
    if phase == "blocked" {
        goal.as_object_mut().expect("goal object").insert(
            "blockedReason".to_owned(),
            json!({"code": "test-blocker", "message": "Blocked for replay validation."}),
        );
    }
    json!({
        "kind": "goal/change",
        "version": 1,
        "operation": operation,
        "goal": goal,
        "roundsStarted": rounds_started,
        "createdAt": created_at,
        "updatedAt": updated_at,
    })
}

fn create(id: &str, seq: u64, max_goal_rounds: u64) -> SessionEvent {
    event(
        "goal/change",
        seq,
        snapshot_change(
            "create",
            id,
            1,
            "validate",
            "active",
            max_goal_rounds,
            0,
            10,
            10,
        ),
    )
}

fn round(seq: u64, id: &str, revision: u64, round: u64) -> SessionEvent {
    event(
        "user/message",
        seq,
        json!({
            "id": format!("message-{seq}"),
            "role": "user",
            "content": [{"type": "text", "text": format!("round {round}")}],
            "source": {"kind": "goal", "goalId": id, "revision": revision, "round": round},
        }),
    )
}

fn error(events: &[SessionEvent]) -> String {
    fold_goal(events).expect_err("stream must fail").to_string()
}

#[test]
fn decoder_is_strict_but_ignores_values_that_do_not_claim_goal_change() {
    assert!(decode_goal_change(&Value::Null).unwrap().is_none());
    assert!(
        decode_goal_change(&json!({"kind": "other"}))
            .unwrap()
            .is_none()
    );

    let base = snapshot_change("create", "goal-wire", 1, "validate", "active", 2, 0, 10, 10);
    for (candidate, expected) in [
        (
            json!({"kind": "goal/change", "version": 2, "operation": "create"}),
            "unsupported goal change version",
        ),
        (
            json!({"kind": "goal/change", "version": 1, "operation": "explode"}),
            "operation is invalid",
        ),
    ] {
        assert!(
            decode_goal_change(&candidate)
                .expect_err("malformed wire change")
                .to_string()
                .contains(expected)
        );
    }

    let mut extra = base.clone();
    extra
        .as_object_mut()
        .expect("snapshot object")
        .insert("extra".to_owned(), json!(true));
    assert!(
        decode_goal_change(&extra)
            .expect_err("extra snapshot field")
            .to_string()
            .contains("must have exactly")
    );
    let clear_extra = json!({
        "kind": "goal/change", "version": 1, "operation": "clear",
        "cleared": {"id": "goal-wire", "revision": 2}, "clearedAt": 11, "extra": true,
    });
    assert!(
        decode_goal_change(&clear_extra)
            .expect_err("extra clear field")
            .to_string()
            .contains("must have exactly")
    );

    for pointer in [
        "/goal/revision",
        "/goal/maxGoalRounds",
        "/roundsStarted",
        "/createdAt",
    ] {
        let mut unsafe_number = base.clone();
        *unsafe_number
            .pointer_mut(pointer)
            .expect("wire numeric field") = json!(MAX_SAFE_INTEGER + 1);
        assert!(
            decode_goal_change(&unsafe_number)
                .expect_err("unsafe integer")
                .to_string()
                .contains("safe integer"),
            "pointer {pointer} was accepted"
        );
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn create_and_mutation_continuity_rejects_stale_identity_counters_and_definition_changes() {
    let base = create("goal-validation", 0, 2);
    for candidate in [
        snapshot_change(
            "create",
            "goal-second",
            2,
            "validate",
            "active",
            2,
            0,
            20,
            20,
        ),
        snapshot_change(
            "create",
            "goal-second",
            1,
            "validate",
            "paused",
            2,
            0,
            20,
            20,
        ),
        snapshot_change(
            "create",
            "goal-second",
            1,
            "validate",
            "active",
            2,
            1,
            20,
            20,
        ),
    ] {
        assert!(error(&[event("goal/change", 0, candidate)]).contains("goal create requires"));
    }

    let edit_without_current = event(
        "goal/change",
        0,
        snapshot_change(
            "edit",
            "goal-validation",
            2,
            "changed",
            "active",
            2,
            0,
            10,
            11,
        ),
    );
    assert!(error(&[edit_without_current]).contains("requires a current goal"));

    let invalid = [
        snapshot_change("edit", "goal-wrong", 2, "changed", "active", 2, 0, 10, 11),
        snapshot_change(
            "edit",
            "goal-validation",
            3,
            "changed",
            "active",
            2,
            0,
            10,
            11,
        ),
        snapshot_change(
            "edit",
            "goal-validation",
            2,
            "changed",
            "active",
            2,
            0,
            11,
            11,
        ),
        snapshot_change(
            "edit",
            "goal-validation",
            2,
            "changed",
            "active",
            2,
            0,
            10,
            9,
        ),
        snapshot_change(
            "edit",
            "goal-validation",
            2,
            "changed",
            "active",
            2,
            1,
            10,
            11,
        ),
        snapshot_change(
            "pause",
            "goal-validation",
            2,
            "changed illegally",
            "paused",
            2,
            0,
            10,
            11,
        ),
        snapshot_change(
            "pause",
            "goal-validation",
            2,
            "validate",
            "paused",
            3,
            0,
            10,
            11,
        ),
    ];
    for (index, candidate) in invalid.into_iter().enumerate() {
        assert!(
            fold_goal(&[base.clone(), event("goal/change", 1, candidate),]).is_err(),
            "invalid continuity case {index} was accepted"
        );
    }
}

#[test]
fn lifecycle_transitions_and_round_budget_are_enforced_during_replay() {
    let base = create("goal-lifecycle", 0, 2);
    for candidate in [
        snapshot_change(
            "edit",
            "goal-lifecycle",
            2,
            "validate",
            "paused",
            2,
            0,
            10,
            11,
        ),
        snapshot_change(
            "pause",
            "goal-lifecycle",
            2,
            "validate",
            "active",
            2,
            0,
            10,
            11,
        ),
        snapshot_change(
            "resume",
            "goal-lifecycle",
            2,
            "validate",
            "paused",
            2,
            0,
            10,
            11,
        ),
        snapshot_change(
            "complete",
            "goal-lifecycle",
            2,
            "validate",
            "active",
            2,
            0,
            10,
            11,
        ),
        snapshot_change(
            "block",
            "goal-lifecycle",
            2,
            "validate",
            "active",
            2,
            0,
            10,
            11,
        ),
    ] {
        assert!(
            fold_goal(&[base.clone(), event("goal/change", 1, candidate)]).is_err(),
            "invalid lifecycle transition was accepted"
        );
    }

    let paused = snapshot_change(
        "pause",
        "goal-lifecycle",
        2,
        "validate",
        "paused",
        2,
        2,
        10,
        11,
    );
    let resumed = snapshot_change(
        "resume",
        "goal-lifecycle",
        3,
        "validate",
        "active",
        2,
        2,
        10,
        12,
    );
    let exhausted = [
        base,
        round(1, "goal-lifecycle", 1, 1),
        round(2, "goal-lifecycle", 1, 2),
        event("goal/change", 3, paused),
        event("goal/change", 4, resumed),
    ];
    assert!(error(&exhausted).contains("exhausted round budget"));
}

#[test]
fn only_exact_positive_sequential_goal_sources_advance_rounds() {
    let base = create("goal-rounds", 0, 3);
    let unrelated = event(
        "user/message",
        1,
        json!({"source": {"kind": "plugin", "plugin": "test"}}),
    );
    let folded = fold_goal(&[base.clone(), unrelated]).expect("unrelated source");
    assert_eq!(folded.rounds_started, 0);

    let valid = fold_goal(&[
        base.clone(),
        round(1, "goal-rounds", 1, 1),
        round(2, "goal-rounds", 1, 2),
    ])
    .expect("sequential rounds");
    assert_eq!(valid.rounds_started, 2);

    for invalid in [
        round(1, "goal-other", 1, 1),
        round(1, "goal-rounds", 2, 1),
        round(1, "goal-rounds", 1, 2),
        round(1, "goal-rounds", 1, 0),
    ] {
        assert!(fold_goal(&[base.clone(), invalid]).is_err());
    }
    assert!(
        error(&[
            base.clone(),
            round(1, "goal-rounds", 1, 1),
            round(2, "goal-rounds", 1, 1),
        ])
        .contains("not the next admitted round")
    );
}

#[test]
fn clear_requires_exact_continuity_and_goal_ids_are_never_reused() {
    let base = create("goal-reuse", 0, 2);
    for clear in [
        json!({
            "kind": "goal/change", "version": 1, "operation": "clear",
            "cleared": {"id": "goal-reuse", "revision": 3}, "clearedAt": 11,
        }),
        json!({
            "kind": "goal/change", "version": 1, "operation": "clear",
            "cleared": {"id": "goal-reuse", "revision": 2}, "clearedAt": 9,
        }),
    ] {
        assert!(fold_goal(&[base.clone(), event("goal/change", 1, clear)]).is_err());
    }

    let valid_clear = event(
        "goal/change",
        1,
        json!({
            "kind": "goal/change", "version": 1, "operation": "clear",
            "cleared": {"id": "goal-reuse", "revision": 2}, "clearedAt": 11,
        }),
    );
    let cleared = fold_goal(&[base.clone(), valid_clear.clone()]).expect("valid clear");
    assert!(cleared.goal.is_none());
    assert_eq!(cleared.last_ref.expect("tombstone").revision, 2);

    let reused = event(
        "goal/change",
        2,
        snapshot_change(
            "create",
            "goal-reuse",
            1,
            "validate",
            "active",
            2,
            0,
            20,
            20,
        ),
    );
    assert!(error(&[base, valid_clear, reused]).contains("fresh active revision-one"));
}

#[test]
fn malformed_snapshots_block_reasons_refs_counters_and_times_fail_loudly() {
    let base = snapshot_change(
        "create",
        "goal-shape",
        1,
        "validate",
        "active",
        2,
        0,
        10,
        10,
    );
    let mut cases = vec![Value::Null];
    for (pointer, replacement) in [
        ("/goal/id", json!("")),
        ("/goal/objective", json!(" ")),
        ("/goal/phase", json!("unknown")),
        ("/goal/revision", json!(0)),
        ("/goal/maxGoalRounds", json!(0)),
        ("/roundsStarted", json!(-1)),
        ("/createdAt", json!(-1)),
    ] {
        let mut candidate = base.clone();
        *candidate.pointer_mut(pointer).expect("shape pointer") = replacement;
        cases.push(candidate);
    }
    let mut extra_goal = base.clone();
    extra_goal
        .pointer_mut("/goal")
        .expect("goal")
        .as_object_mut()
        .expect("goal object")
        .insert("extra".to_owned(), json!(true));
    cases.push(extra_goal);
    for (index, candidate) in cases.into_iter().enumerate() {
        assert!(
            decode_goal_change(&candidate).is_err() || candidate.is_null(),
            "malformed snapshot case {index} was accepted"
        );
    }

    for reason in [
        Value::Null,
        json!({"code": "NOT_CANONICAL", "message": "Bad code."}),
        json!({"code": "test-blocker", "message": " padded "}),
        json!({"code": "test-blocker", "message": "Valid.", "extra": true}),
    ] {
        let mut blocked = snapshot_change(
            "create",
            "goal-blocked",
            1,
            "validate",
            "blocked",
            2,
            0,
            10,
            10,
        );
        *blocked
            .pointer_mut("/goal/blockedReason")
            .expect("blocked reason") = reason;
        assert!(decode_goal_change(&blocked).is_err());
    }

    for clear in [
        json!({
            "kind": "goal/change", "version": 1, "operation": "clear",
            "cleared": null, "clearedAt": 1,
        }),
        json!({
            "kind": "goal/change", "version": 1, "operation": "clear",
            "cleared": {"id": "", "revision": 1}, "clearedAt": 1,
        }),
        json!({
            "kind": "goal/change", "version": 1, "operation": "clear",
            "cleared": {"id": "goal", "revision": 0}, "clearedAt": 1,
        }),
        json!({
            "kind": "goal/change", "version": 1, "operation": "clear",
            "cleared": {"id": "goal", "revision": 1}, "clearedAt": MAX_SAFE_INTEGER + 1,
        }),
    ] {
        assert!(decode_goal_change(&clear).is_err());
    }
}
