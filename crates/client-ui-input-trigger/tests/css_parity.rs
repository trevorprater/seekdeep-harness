//! Exact source-CSS projection for the compiled trigger menu.

use regex::Regex;
use seekdeep_client_ui_input_trigger::MENU_VIEW_STYLES;

const SOURCE: &str =
    include_str!("../../../packages/client/ui-input-trigger/src/client/MenuView.module.css");

fn namespace(source: &str) -> String {
    Regex::new(r"\.([A-Za-z_][A-Za-z0-9_-]*)")
        .unwrap()
        .replace_all(source, ".seekdeep-trigger-$1")
        .into_owned()
}

#[test]
fn compiled_trigger_menu_styles_are_an_exact_namespaced_source_projection() {
    assert_eq!(MENU_VIEW_STYLES, namespace(SOURCE));
}
