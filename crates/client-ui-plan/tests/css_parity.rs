//! Exact Plan chip CSS projection parity.

use regex::Regex;
use seekdeep_client_ui_plan::PLAN_CHIP_STYLES;

const SOURCE: &str =
    include_str!("../../../packages/client/ui-plan/src/client/PlanModeControl.module.css");

fn namespace(source: &str) -> String {
    Regex::new(r"\.([A-Za-z_][A-Za-z0-9_-]*)")
        .unwrap()
        .replace_all(source, ".seekdeep-plan-$1")
        .into_owned()
}

#[test]
fn compiled_plan_chip_styles_are_an_exact_namespaced_source_projection() {
    assert_eq!(PLAN_CHIP_STYLES, namespace(SOURCE));
}
