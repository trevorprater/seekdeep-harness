//! Exact source-CSS projection for the compiled Permission row.

use regex::Regex;
use seekdeep_client_ui_permission_presets::PERMISSION_ROW_STYLES;

const SOURCE: &str = include_str!(
    "../../../packages/client/ui-permission-presets/src/client/PermissionRow.module.css"
);

fn namespace(source: &str) -> String {
    Regex::new(r"\.([A-Za-z_][A-Za-z0-9_-]*)")
        .unwrap()
        .replace_all(source, ".seekdeep-permission-$1")
        .into_owned()
}

#[test]
fn compiled_permission_row_styles_are_an_exact_namespaced_source_projection() {
    assert_eq!(PERMISSION_ROW_STYLES, namespace(SOURCE));
}
