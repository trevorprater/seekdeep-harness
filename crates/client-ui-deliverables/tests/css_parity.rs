//! Exact produced-files CSS projection parity.

use regex::Regex;
use seekdeep_client_ui_deliverables::PRODUCED_FILES_STYLES;

const SOURCE: &str =
    include_str!("../../../packages/client/ui-deliverables/src/client/ProducedFiles.module.css");

fn namespace(source: &str) -> String {
    Regex::new(r"\.([A-Za-z_][A-Za-z0-9_-]*)")
        .unwrap()
        .replace_all(source, ".seekdeep-deliverables-$1")
        .into_owned()
}

#[test]
fn compiled_produced_files_styles_are_an_exact_namespaced_source_projection() {
    assert_eq!(PRODUCED_FILES_STYLES, namespace(SOURCE));
}
