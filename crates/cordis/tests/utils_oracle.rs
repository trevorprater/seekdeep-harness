//! Disposable-list and error-composition utility source oracle.

use std::sync::Arc;

use seekdeep_cordis::{DisposableList, compose_error, is_json_object_like};
use serde_json::json;

#[test]
fn duplicate_identity_delete_handles_and_reverse_clear_match_source() {
    let list = DisposableList::new();
    let a = Arc::new("a");
    let b = Arc::new("b");
    let first_a = list.push(a.clone());
    let _b = list.push(b.clone());
    let second_a = list.push(a.clone());
    assert_eq!(
        list.values()
            .iter()
            .map(Arc::as_ref)
            .copied()
            .collect::<Vec<_>>(),
        ["a", "b", "a"]
    );
    assert!(list.delete(&a));
    assert_eq!(
        list.values()
            .iter()
            .map(Arc::as_ref)
            .copied()
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert!(first_a.dispose());
    assert!(!first_a.dispose());
    assert!(!second_a.dispose());
    assert_eq!(
        list.clear()
            .iter()
            .map(Arc::as_ref)
            .copied()
            .collect::<Vec<_>>(),
        ["b"]
    );
    assert!(list.is_empty());
}

#[test]
fn json_object_classification_and_error_context_are_explicit() {
    assert!(!is_json_object_like(&json!(null)));
    assert!(!is_json_object_like(&json!(0)));
    assert!(!is_json_object_like(&json!("")));
    assert!(is_json_object_like(&json!({})));
    assert!(is_json_object_like(&json!([])));

    let error = compose_error::<()>("outer boundary", || anyhow::bail!("inner failure"))
        .expect_err("must fail");
    assert_eq!(format!("{error:#}"), "outer boundary: inner failure");
}
