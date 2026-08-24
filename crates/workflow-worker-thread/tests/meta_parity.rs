//! Behavioral mirror of the worker metadata validation source suite.

use seekdeep_workflow::{WorkflowError, WorkflowErrorCode};
use seekdeep_workflow_worker_thread::validate_meta_value;
use serde_json::{Value, json};

fn expect_invalid(value: &Value, fragments: &[&str]) {
    let error = validate_meta_value(value).unwrap_err();
    let typed = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<WorkflowError>())
        .expect("WorkflowError");
    assert_eq!(typed.code, WorkflowErrorCode::MetaInvalid);
    for fragment in fragments {
        assert!(
            typed.message.contains(fragment),
            "expected {fragment:?} in {:?}",
            typed.message
        );
    }
}

#[test]
fn accepts_minimal_meta_as_a_detached_normalized_copy() {
    let mut input = json!({"name": "audit", "description": "audit the repo"});
    let meta = validate_meta_value(&input).unwrap();
    assert_eq!(meta.name, "audit");
    assert_eq!(meta.description, "audit the repo");
    assert_eq!(meta.when_to_use, None);
    assert_eq!(meta.phases, None);
    input["name"] = json!("mutated");
    assert_eq!(meta.name, "audit");
}

#[test]
fn accepts_and_rebuilds_the_full_shape_entry_by_entry() {
    let value = json!({
        "name": "migrate",
        "description": "migrate call sites",
        "whenToUse": "large mechanical sweeps",
        "phases": [
            {"title": "Discover", "provider": "openai"},
            {"title": "Transform", "detail": "one agent per file", "model": "deepseek-v4-pro"}
        ]
    });
    let meta = validate_meta_value(&value).unwrap();
    assert_eq!(meta.name, "migrate");
    assert_eq!(meta.when_to_use.as_deref(), Some("large mechanical sweeps"));
    let phases = meta.phases.unwrap();
    assert_eq!(phases.len(), 2);
    assert_eq!(phases[0].title, "Discover");
    assert_eq!(phases[0].provider.as_deref(), Some("openai"));
    assert_eq!(phases[1].title, "Transform");
    assert_eq!(phases[1].detail.as_deref(), Some("one agent per file"));
    assert_eq!(phases[1].model.as_deref(), Some("deepseek-v4-pro"));
}

#[test]
fn rejects_non_objects_and_unknown_top_level_fields() {
    for value in [json!("a string"), Value::Null, json!([{"name": "x"}])] {
        expect_invalid(&value, &["meta must be an object"]);
    }
    expect_invalid(
        &json!({"name": "x", "description": "d", "color": "red"}),
        &["meta.color is not a recognized field"],
    );
}

#[test]
fn rejects_missing_or_mistyped_scalar_fields() {
    let cases = [
        (
            json!({"description": "d"}),
            "meta.name must be a non-empty string",
        ),
        (
            json!({"name": "", "description": "d"}),
            "meta.name must be a non-empty string",
        ),
        (
            json!({"name": "x"}),
            "meta.description must be a non-empty string",
        ),
        (
            json!({"name": "x", "description": 42}),
            "meta.description must be a non-empty string",
        ),
        (
            json!({"name": "x", "description": "d", "whenToUse": 3}),
            "meta.whenToUse must be a string",
        ),
    ];
    for (value, fragment) in cases {
        expect_invalid(&value, &[fragment]);
    }
}

#[test]
fn rejects_every_malformed_phase_shape_and_field() {
    let cases = [
        (
            json!({"name": "x", "description": "d", "phases": "Scan"}),
            "meta.phases must be an array",
        ),
        (
            json!({"name": "x", "description": "d", "phases": ["Scan"]}),
            "meta.phases[0] must be an object",
        ),
        (
            json!({"name": "x", "description": "d", "phases": [{"title": ""}]}),
            "meta.phases[0].title must be a non-empty string",
        ),
        (
            json!({"name": "x", "description": "d", "phases": [{"title": "Scan", "order": 1}]}),
            "meta.phases[0].order is not a recognized field",
        ),
        (
            json!({"name": "x", "description": "d", "phases": [{"title": "Scan", "detail": 9}]}),
            "meta.phases[0].detail must be a string",
        ),
        (
            json!({"name": "x", "description": "d", "phases": [{"title": "Scan", "provider": 9}]}),
            "meta.phases[0].provider must be a string",
        ),
        (
            json!({"name": "x", "description": "d", "phases": [{"title": "Scan", "model": 9}]}),
            "meta.phases[0].model must be a string",
        ),
    ];
    for (value, fragment) in cases {
        expect_invalid(&value, &[fragment]);
    }
}

#[test]
fn reports_every_violation_in_one_typed_failure() {
    expect_invalid(
        &json!({
            "description": 7,
            "extra": true,
            "phases": [{"title": "Scan"}, "bad"]
        }),
        &[
            "meta.extra is not a recognized field",
            "meta.name must be a non-empty string",
            "meta.description must be a non-empty string",
            "meta.phases[1] must be an object",
        ],
    );
}
