//! Disabled and static metadata expression fixtures.

use seekdeep_repository_tools::cordis_config_metadata::metadata_expression_errors;

fn object(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    let serde_json::Value::Object(object) = value else {
        panic!("fixture must be an object");
    };
    object
}

#[test]
fn disabled_expression_is_allowed_when_it_parses() {
    assert!(
        metadata_expression_errors(
            &object(serde_json::json!({
                "id": "tool-bash",
                "name": "@seekdeep-ai/seekdeep-tool-bash",
                "disabled": { "__jsExpr": "process.platform === 'win32'" }
            })),
            "[0]",
        )
        .is_empty()
    );
}

#[test]
fn static_and_nested_disabled_expressions_are_rejected() {
    assert!(
        metadata_expression_errors(
            &object(serde_json::json!({ "id": { "__jsExpr": "process.platform" }, "name": "pkg" })),
            "[0]",
        )
        .contains(&"[0].id: !!js is not interpolated here".to_owned())
    );
    assert!(
        metadata_expression_errors(
            &object(serde_json::json!({
                "id": "tool-bash",
                "name": "pkg",
                "disabled": { "when": { "__jsExpr": "process.platform" } }
            })),
            "[0]",
        )
        .contains(&"[0].disabled.when: !!js is not interpolated here".to_owned())
    );
}

#[test]
fn invalid_disabled_expression_fails_parse_only_validation() {
    assert!(
        metadata_expression_errors(
            &object(serde_json::json!({
                "id": "tool-bash",
                "name": "pkg",
                "disabled": { "__jsExpr": "process.platform ===" }
            })),
            "[0]",
        )
        .iter()
        .any(|problem| problem.contains("[0].disabled: disabled expression does not parse"))
    );
}
