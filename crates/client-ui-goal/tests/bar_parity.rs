//! Goal bar local lifecycle parity.

use seekdeep_client_ui_goal::{
    GoalBarAction, GoalBarBlockReason, GoalBarController, GoalBarPhase, GoalBarSnapshot,
};

fn goal(id: &str, phase: GoalBarPhase) -> GoalBarSnapshot {
    GoalBarSnapshot {
        id: id.to_owned(),
        revision: 1,
        objective: "Ship it".to_owned(),
        phase,
        blocked_reason: (phase == GoalBarPhase::Blocked).then(|| GoalBarBlockReason {
            code: "waiting".to_owned(),
            message: "Waiting for review".to_owned(),
        }),
    }
}

#[test]
fn visibility_edit_identity_and_clear_commit_match_source() {
    let active = goal("g1", GoalBarPhase::Active);
    let mut controller = GoalBarController::new();
    controller.reconcile(Some(&active));
    assert!(controller.visible(Some(&active)));
    assert!(!controller.visible(None));
    assert!(!controller.visible(Some(&goal("done", GoalBarPhase::Complete))));

    controller.begin_edit(&active.objective);
    assert!(controller.editing());
    assert_eq!(controller.draft(), "Ship it");
    controller.set_draft("Changed");
    controller.cancel_edit();
    assert!(!controller.editing());
    controller.begin_edit(&active.objective);
    assert_eq!(controller.draft(), "Ship it");

    assert!(controller.begin_action());
    assert!(!controller.begin_action());
    controller.settle_action(GoalBarAction::Clear, Some("g1"), Ok(()));
    assert!(!controller.visible(Some(&active)));
    let replacement = goal("g2", GoalBarPhase::Paused);
    controller.reconcile(Some(&replacement));
    assert!(controller.visible(Some(&replacement)));
    assert!(!controller.editing());
}

#[test]
fn edit_success_and_remote_failures_rearm_with_exact_inline_text() {
    let active = goal("g1", GoalBarPhase::Active);
    let mut controller = GoalBarController::new();
    controller.reconcile(Some(&active));
    controller.begin_edit(&active.objective);
    assert!(controller.begin_action());
    controller.settle_action(GoalBarAction::Edit, Some("g1"), Ok(()));
    assert!(!controller.editing());
    assert!(!controller.pending());

    assert!(controller.begin_action());
    controller.settle_action(
        GoalBarAction::Resume,
        Some("g1"),
        Err(("conflict", "revision changed")),
    );
    assert_eq!(
        controller.action_error(),
        Some("revision changed (conflict)")
    );
    assert!(!controller.pending());
}
