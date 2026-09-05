//! Exact Goal surface CSS projection parity.

use regex::Regex;
use seekdeep_client_ui_goal::{GOAL_BAR_STYLES, GOAL_COMMAND_STYLES};

const BAR_SOURCE: &str =
    include_str!("../../../packages/client/ui-goal/src/client/GoalBar.module.css");
const COMMAND_SOURCE: &str =
    include_str!("../../../packages/client/ui-goal/src/client/GoalCommandInputView.module.css");

fn namespace(source: &str, prefix: &str) -> String {
    Regex::new(r"\.([A-Za-z_][A-Za-z0-9_-]*)")
        .unwrap()
        .replace_all(source, format!(".{prefix}$1"))
        .into_owned()
}

#[test]
fn compiled_goal_styles_are_exact_namespaced_source_projections() {
    assert_eq!(GOAL_BAR_STYLES, namespace(BAR_SOURCE, "seekdeep-goal-"));
    assert_eq!(
        GOAL_COMMAND_STYLES,
        namespace(COMMAND_SOURCE, "seekdeep-goal-command-")
    );
}
