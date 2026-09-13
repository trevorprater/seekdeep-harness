//! Exact workflow-run CSS projection parity.

use regex::Regex;
use seekdeep_client_ui_workflow_run::WORKFLOW_RUN_STYLES;

const SOURCE: &str =
    include_str!("../../../packages/client/ui-workflow-run/src/client/WorkflowRunPanel.module.css");

fn namespace(source: &str) -> String {
    Regex::new(r"\.([A-Za-z_][A-Za-z0-9_-]*)")
        .unwrap()
        .replace_all(source, ".seekdeep-workflow-run-$1")
        .into_owned()
}

#[test]
fn compiled_workflow_run_styles_are_an_exact_namespaced_source_projection() {
    assert_eq!(WORKFLOW_RUN_STYLES, namespace(SOURCE));
}
