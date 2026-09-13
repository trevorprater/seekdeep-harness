//! Exact source-CSS projection for the compiled model selector.

use regex::Regex;
use seekdeep_client_ui_model_selection::MODEL_SELECT_STYLES;

const SOURCE: &str =
    include_str!("../../../packages/client/ui-model-selection/src/client/ModelSelect.module.css");

fn namespace(source: &str) -> String {
    Regex::new(r"\.([A-Za-z_][A-Za-z0-9_-]*)")
        .unwrap()
        .replace_all(source, ".seekdeep-model-$1")
        .into_owned()
}

#[test]
fn compiled_model_select_styles_are_an_exact_namespaced_source_projection() {
    assert_eq!(MODEL_SELECT_STYLES, namespace(SOURCE));
}
