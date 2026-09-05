//! Exact source-CSS projections for compiled subagent surfaces.

use regex::Regex;
use seekdeep_client_ui_subagent::{SUBAGENT_CATALOG_STYLES, SUBAGENT_READ_ONLY_STYLES};

const CATALOG_SOURCE: &str = include_str!(
    "../../../packages/client/ui-subagent/src/client/SubagentCatalogAction.module.css"
);
const READ_ONLY_SOURCE: &str = include_str!(
    "../../../packages/client/ui-subagent/src/client/SubagentReadOnlyComposer.module.css"
);

fn namespace(source: &str, prefix: &str) -> String {
    let source = Regex::new(r":global\(([^)]+)\)")
        .unwrap()
        .replace_all(source, "$1");
    Regex::new(r"\.([A-Za-z_][A-Za-z0-9_-]*)")
        .unwrap()
        .replace_all(&source, format!(".{prefix}$1"))
        .into_owned()
}

#[test]
fn compiled_subagent_styles_are_exact_namespaced_source_projections() {
    assert_eq!(
        SUBAGENT_CATALOG_STYLES,
        namespace(CATALOG_SOURCE, "seekdeep-subagent-catalog-")
    );
    assert_eq!(
        SUBAGENT_READ_ONLY_STYLES,
        namespace(READ_ONLY_SOURCE, "seekdeep-subagent-readonly-")
    );
}
