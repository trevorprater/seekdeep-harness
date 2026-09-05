//! Generated Slot catalog and compact/full live subtree projection parity.

use seekdeep_cordis_client_runner::{
    LiveSlotNode, client_slot_api, client_slot_notes, query_client_slots,
};
use serde_json::json;

#[test]
fn generated_slot_catalog_retains_notes_contracts_and_source_order() {
    assert_eq!(client_slot_notes().len(), 6);
    assert!(!client_slot_api().is_empty());
    assert_eq!(client_slot_api()[0]["key"], "conversation");
    assert!(
        client_slot_api()
            .iter()
            .any(|entry| entry["key"] == "tool.view.cordis")
    );
}

fn tree() -> LiveSlotNode {
    LiveSlotNode {
        name: "tool.view.cordis".to_owned(),
        kind: "keyed".to_owned(),
        scope: "session".to_owned(),
        declared_by: Some("tool.call.toolview".to_owned()),
        occupants: vec![json!({"key": "panel-1.pkg-2", "priority": -1})],
        children: vec![LiveSlotNode {
            name: "unknown.child".to_owned(),
            kind: "single".to_owned(),
            scope: "root".to_owned(),
            declared_by: None,
            occupants: Vec::new(),
            children: Vec::new(),
        }],
    }
}

#[test]
fn compact_and_selected_projection_merge_live_tree_with_guarded_catalog_contract() {
    let result = query_client_slots(Some("tool.view.cordis"), &[tree()]);
    assert_eq!(
        result["requestedRoot"],
        json!({"name": "tool.view.cordis", "available": true})
    );
    assert_eq!(
        result["trees"][0]["purpose"],
        client_slot_api()
            .iter()
            .find(|entry| entry["key"] == "tool.view.cordis")
            .unwrap()["summary"]
    );
    assert_eq!(
        result["trees"][0]["keyDomain"],
        "fixed by the dynamic Client Guard"
    );
    assert_eq!(result["trees"][0]["allowedKeys"][0]["value"], "self");
    assert_eq!(
        result["trees"][0]["children"][0],
        json!({
            "name": "unknown.child",
            "kind": "single",
            "scope": "root",
            "children": [],
        })
    );
    assert_eq!(result["selected"]["declaredBy"], "tool.call.toolview");
    assert_eq!(result["selected"]["occupants"][0]["priority"], -1);
    assert_eq!(
        result["selected"]["catalog"]["allowedKeys"][0]["value"],
        "self"
    );
    assert_eq!(result["referencedTypes"], json!([]));
}

#[test]
fn missing_root_and_navigation_only_queries_keep_exact_optional_fields_absent() {
    let missing = query_client_slots(Some("missing"), &[]);
    assert_eq!(
        missing,
        json!({
            "requestedRoot": {"name": "missing", "available": false},
            "trees": [],
            "referencedTypes": [],
        })
    );
    let navigation = query_client_slots(None, &[tree()]);
    assert!(navigation.get("requestedRoot").is_none());
    assert!(navigation.get("selected").is_none());
}
