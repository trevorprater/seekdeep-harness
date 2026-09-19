//! Exact Skill row CSS projection parity.

use regex::Regex;
use seekdeep_client_ui_skill::SKILL_ROW_STYLES;

const SOURCE: &str =
    include_str!("../../../packages/client/ui-skill/src/client/SkillRow.module.css");

fn namespace(source: &str) -> String {
    Regex::new(r"\.([A-Za-z_][A-Za-z0-9_-]*)")
        .unwrap()
        .replace_all(source, ".seekdeep-skill-$1")
        .into_owned()
}

#[test]
fn compiled_skill_row_styles_are_an_exact_namespaced_source_projection() {
    assert_eq!(SKILL_ROW_STYLES, namespace(SOURCE));
}
