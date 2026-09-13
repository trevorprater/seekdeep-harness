//! Catalog validation, source metadata retention, and independent rendering paths.

use serde_json::{Value, json};

#[path = "support/catalog_cases.rs"]
mod cases;

fn outcome(name: &str) -> Value {
    cases::outcome(
        &cases::cases()
            .into_iter()
            .find(|case| case["name"] == name)
            .unwrap(),
    )
}

#[test]
fn complete_contract_retains_paragraphs_lists_tags_and_transitive_types() {
    let result = outcome("complete-contract");
    let result = &result["ok"];
    assert_eq!(
        result["model"]["services"][0]["doc"],
        "First sentence. More detail.\n\n- First item continued.\n- Second Known item."
    );
    let method = &result["catalog"]["services"][0]["methods"][0];
    assert_eq!(
        method["parameters"],
        json!([{"name":"value","description":"first line continued value."}])
    );
    assert_eq!(method["returns"], "returned value.");
    assert_eq!(
        method["throws"],
        json!(["first failure continued failure.", "second failure."])
    );
    assert_eq!(result["catalog"]["types"].as_array().unwrap().len(), 2);
    assert!(
        result["region"]
            .as_str()
            .unwrap()
            .contains("Types: [Known](core.md)")
    );
}

#[test]
fn validation_is_aggregated_before_type_classification() {
    for (name, message) in [
        ("missing-mode", "missing an @mode tag"),
        ("invalid-mode", "missing an @mode tag"),
        ("event-not-callable", "not represented by a callable type"),
        ("waterfall-without-next", "has no trailing 'next' parameter"),
        ("non-waterfall-next", "tagged '@mode emit'"),
        ("event-missing-param", "is missing @param value"),
        ("event-empty-param", "@param value has an empty description"),
        ("event-stale-param", "@param absent does not match"),
        ("event-empty-description", "has no description prose"),
        ("event-binding-pattern", "is a binding pattern"),
        (
            "unclassified-type",
            "signature type-link coverage violation",
        ),
        (
            "documentation-before-classification",
            "JSDoc completeness violation",
        ),
        ("event-aggregates", "2 JSDoc completeness violation"),
        ("method-empty-description", "has no description prose"),
        ("method-missing-param", "is missing @param value"),
        (
            "method-empty-param",
            "@param value has an empty description",
        ),
        ("method-missing-returns", "is missing @returns"),
        ("method-empty-returns", "@returns has an empty description"),
        ("method-stale-param", "@param other does not match"),
        ("method-no-jsdoc", "has no JSDoc"),
        ("service-no-jsdoc", "has no JSDoc"),
        ("method-binding-pattern", "is a binding pattern"),
    ] {
        let result = outcome(name);
        assert_eq!(result["error"]["name"], "Error", "{name}: {result}");
        assert!(
            result["error"]["message"]
                .as_str()
                .unwrap()
                .contains(message),
            "{name}: {result}"
        );
    }
}

#[test]
fn selected_services_and_runtime_types_follow_distinct_policies() {
    assert_eq!(
        outcome("class-precedes-interface")["ok"]["model"]["services"][0]["type"],
        "CatalogService"
    );
    assert_eq!(
        outcome("later-class-replaces-interface")["ok"]["model"]["services"][0]["type"],
        "OtherService"
    );
    assert!(
        outcome("nested-host-merge-excluded")["ok"]["model"]["services"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        outcome("foreign-service-declaration-excluded")["ok"]["model"]["services"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        outcome("runtime-service-exclusion")["ok"]["catalog"]["services"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        outcome("ambiguous-runtime-type")["ok"]["catalog"]["types"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        !outcome("documented-property")["ok"]["region"]
            .as_str()
            .unwrap()
            .contains("```ts cordis-catalog\n/**\n * Run")
    );
    assert!(
        outcome("client-face-classification-bypass")
            .get("ok")
            .is_some()
    );
    assert!(outcome("cyclic-signature-graph").get("ok").is_some());
}
