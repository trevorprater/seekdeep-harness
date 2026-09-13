//! Plan projection, exit state, and locale parity.

use seekdeep_client_ui_plan::{
    PLAN_LOCALES, PLAN_NS, PlanExitState, PlanProjection, effective_plan_target, plan_chip_visible,
};

#[test]
fn authoritative_effective_target_truth_table_matches_the_source() {
    assert!(!plan_chip_visible(None));
    for (active, pending, expected) in [
        (false, false, false),
        (true, false, true),
        (false, true, true),
        (true, true, false),
    ] {
        let projection = PlanProjection { active, pending };
        assert_eq!(effective_plan_target(projection), expected);
        assert_eq!(plan_chip_visible(Some(projection)), expected);
    }
}

#[test]
fn exit_attempt_rearms_and_retains_failures_without_hiding_the_chip() {
    let mut state = PlanExitState::default();
    assert!(!state.disabled(false));
    assert!(state.disabled(true));
    state.begin();
    assert!(state.leaving);
    assert!(state.disabled(false));
    assert_eq!(state.error, None);
    state.settle(Some("host said no".to_owned()));
    assert!(!state.leaving);
    assert_eq!(state.error.as_deref(), Some("host said no"));
    assert!(!state.disabled(false));
    state.begin();
    assert_eq!(state.error, None);
    state.settle(None);
    assert_eq!(state, PlanExitState::default());
}

#[test]
fn locale_namespace_and_copy_are_exact() {
    assert_eq!(PLAN_NS, "plan");
    assert_eq!(PLAN_LOCALES.len(), 4);
    assert_eq!(
        PLAN_LOCALES[1],
        (
            "chip.on.title",
            "plan mode 已开启 — 点击关闭（/plan off）",
            "Plan mode on — click to turn off (/plan off)"
        )
    );
    assert_eq!(
        PLAN_LOCALES[3],
        (
            "chip.off.title",
            "plan mode 已关闭 — 点击开启（/plan）",
            "Plan mode off — click to turn on (/plan)"
        )
    );
}
