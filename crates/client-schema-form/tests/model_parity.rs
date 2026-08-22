//! Rehydration, schema navigation, and immutable path edit parity.

use std::sync::Arc;

use seekdeep_client_schema_form::*;
use seekdeep_cordis::Context;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_schemastery::Schema;
use serde_json::{Value, json};

fn path(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn rehydrates_validates_and_navigates_object_dict_and_array_nodes() {
    let source = Schema::object([
        (
            "providers",
            Schema::dict(Schema::object([("baseURL", Schema::string())])),
        ),
        (
            "models",
            Schema::array(Schema::object([("id", Schema::string())])),
        ),
        ("leaf", Schema::string()),
        ("name", Schema::string().required()),
    ]);
    let root = rehydrate_schema(&source.to_json()).unwrap();
    assert_eq!(validate_draft(&root, &json!({"name": "ok"})), None);
    assert!(
        validate_draft(&root, &json!({"name": 42}))
            .unwrap()
            .contains("name")
    );
    assert_eq!(node_at_path(&root, &[]).unwrap().kind_name(), "object");
    assert_eq!(
        node_at_path(&root, &path(&["providers", "openai"]))
            .unwrap()
            .kind_name(),
        "object"
    );
    assert_eq!(
        node_at_path(&root, &path(&["providers", "openai", "baseURL"]))
            .unwrap()
            .kind_name(),
        "string"
    );
    assert_eq!(
        node_at_path(&root, &path(&["models", "0", "id"]))
            .unwrap()
            .kind_name(),
        "string"
    );
    assert!(node_at_path(&root, &path(&["missing"])).is_none());
    assert!(node_at_path(&root, &path(&["leaf", "below"])).is_none());
}

#[test]
fn reads_and_tests_presence_by_key_not_truthiness() {
    let root = json!({
        "providers": {"openai": {"baseURL": "https://x"}},
        "models": [{"id": "a"}],
        "flag": false,
        "nested": {"key": null}
    });
    assert!(std::ptr::eq(
        get_path(&root, &[]).unwrap(),
        std::ptr::from_ref(&root)
    ));
    assert_eq!(
        get_path(&root, &path(&["providers", "openai", "baseURL"])),
        Some(&json!("https://x"))
    );
    assert_eq!(
        get_path(&root, &path(&["models", "0", "id"])),
        Some(&json!("a"))
    );
    assert!(get_path(&root, &path(&["providers", "missing", "x"])).is_none());
    assert!(has_path(Some(&root), &path(&["flag"])));
    assert!(has_path(Some(&root), &path(&["nested", "key"])));
    assert!(!has_path(Some(&root), &path(&["missing"])));
    assert!(has_path(Some(&root), &path(&["models", "0"])));
    assert!(!has_path(Some(&root), &path(&["models", "1"])));
    assert!(has_path(Some(&root), &[]));
    assert!(!has_path(None, &[]));
}

#[test]
fn sets_paths_immutably_and_materializes_container_shape() {
    let draft = Arc::new(json!({}));
    let next = set_path(
        &draft,
        &path(&["providers", "openai", "baseURL"]),
        json!("https://y"),
    )
    .unwrap();
    assert_eq!(*draft, json!({}));
    assert_eq!(
        *next,
        json!({"providers": {"openai": {"baseURL": "https://y"}}})
    );
    let with_array = set_path(&next, &path(&["models", "0"]), json!({"id": "a"})).unwrap();
    assert_eq!(
        *with_array,
        json!({"providers": {"openai": {"baseURL": "https://y"}}, "models": [{"id": "a"}]})
    );
    let replaced = set_path(&with_array, &path(&["models", "0", "id"]), json!("b")).unwrap();
    assert_eq!(replaced["models"], json!([{"id": "b"}]));
    assert_eq!(with_array["models"], json!([{"id": "a"}]));
    assert!(
        set_path(&draft, &[], Value::Null)
            .unwrap_err()
            .to_string()
            .contains("non-empty path")
    );
}

#[test]
fn deletes_keys_and_array_indexes_immutably_and_retains_identity_on_miss() {
    let draft = Arc::new(json!({
        "providers": {"openai": {"baseURL": "https://x", "apiKey": "k"}},
        "models": [{"id": "a", "contextWindow": 1}, {"id": "b"}]
    }));
    let without_key = delete_path(&draft, &path(&["providers", "openai", "apiKey"])).unwrap();
    assert_eq!(
        without_key["providers"]["openai"],
        json!({"baseURL": "https://x"})
    );
    assert_eq!(draft["providers"]["openai"]["apiKey"], "k");
    let without_model = delete_path(&without_key, &path(&["models", "0"])).unwrap();
    assert_eq!(without_model["models"], json!([{"id": "b"}]));
    let missing = delete_path(&draft, &path(&["providers", "missing", "x"])).unwrap();
    assert!(Arc::ptr_eq(&missing, &draft));
    let nested = delete_path(&draft, &path(&["models", "0", "contextWindow"])).unwrap();
    assert_eq!(nested["models"][0], json!({"id": "a"}));
    assert_eq!(draft["models"][0], json!({"id": "a", "contextWindow": 1}));
    assert!(
        delete_path(&draft, &[])
            .unwrap_err()
            .to_string()
            .contains("non-empty path")
    );
}

#[tokio::test]
async fn invariant_reserves_and_releases_package_identity() {
    let registry =
        Arc::new(InvariantRegistry::new(&Context::new(), &InvariantConfig::default()).unwrap());
    let registration = register_invariant(&registry).unwrap();
    assert!(register_invariant(&registry).is_err());
    registration.dispose().await.unwrap();
    register_invariant(&registry)
        .unwrap()
        .dispose()
        .await
        .unwrap();
}
