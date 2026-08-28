//! Mechanical source-CSS projection for the compiled global Table renderer.

use seekdeep_client_ui_trajectory::{TRAJECTORY_TABLE_STYLES, TRAJECTORY_VIEW_STYLES};

const SOURCE: &str =
    include_str!("../../../packages/client/ui-trajectory/src/client/TrajectoryTable.module.css");
const PREFIX: &str = "seekdeep-trajectory-table-";
const VIEW_SOURCE: &str =
    include_str!("../../../packages/client/ui-trajectory/src/client/views.module.css");

fn globalize_css_modules(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = String::with_capacity(source.len() + source.len() / 2);
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'.'
            && bytes
                .get(index + 1)
                .is_some_and(|next| next.is_ascii_alphabetic() || *next == b'_')
        {
            output.push('.');
            output.push_str(PREFIX);
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'_' | b'-'))
            {
                output.push(char::from(bytes[index]));
                index += 1;
            }
            continue;
        }
        output.push(char::from(bytes[index]));
        index += 1;
    }
    output = output.replace(":global([role='tooltip'])", "[role='tooltip']");
    output = output.replace(
        &format!(":global(.{PREFIX}md-code-block)"),
        ".md-code-block",
    );
    output.replace(
        "history-loading-spin",
        "seekdeep-trajectory-table-history-loading-spin",
    )
}

#[test]
fn compiled_table_styles_are_an_exact_globalized_source_projection() {
    assert_eq!(TRAJECTORY_TABLE_STYLES, globalize_css_modules(SOURCE));
}

#[test]
fn compiled_view_styles_are_an_exact_namespaced_source_projection() {
    assert_eq!(
        TRAJECTORY_VIEW_STYLES,
        VIEW_SOURCE
            .replace(".root", ".seekdeep-trajectory-view-root")
            .replace(".ledger", ".seekdeep-trajectory-view-ledger")
    );
}
