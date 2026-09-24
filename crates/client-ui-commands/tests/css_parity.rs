//! Exact source-CSS projection for the compiled popup-select shell.

use regex::Regex;
use seekdeep_client_ui_commands::POPUP_VIEW_STYLES;

const SOURCE: &str =
    include_str!("../../../packages/client/ui-commands/src/client/PopupSelectView.module.css");

fn namespace(source: &str) -> String {
    Regex::new(r"\.([A-Za-z_][A-Za-z0-9_-]*)")
        .unwrap()
        .replace_all(source, ".seekdeep-command-$1")
        .into_owned()
}

#[test]
fn compiled_popup_styles_are_an_exact_namespaced_source_projection() {
    assert_eq!(POPUP_VIEW_STYLES, namespace(SOURCE));
}
