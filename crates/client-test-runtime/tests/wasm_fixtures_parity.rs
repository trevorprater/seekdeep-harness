//! Browser fixture object-shape parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Map, Reflect};
use seekdeep_client_test_runtime::{conversation_snapshot_js, workspace_list_state_js};
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_test::wasm_bindgen_test;

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

#[wasm_bindgen_test]
fn browser_defaults_keep_every_source_field_and_empty_identity_kind() {
    let snapshot = conversation_snapshot_js("s1".to_owned()).unwrap();
    assert_eq!(
        property(&snapshot, "sessionId").as_string().as_deref(),
        Some("s1")
    );
    for key in ["nodes", "runningCalls", "pending", "queue"] {
        assert!(Array::is_array(&property(&snapshot, key)), "{key}");
        assert_eq!(Array::from(&property(&snapshot, key)).length(), 0, "{key}");
    }
    assert!(property(&snapshot, "turnTimings").is_instance_of::<Map>());
    assert!(property(&snapshot, "turnEnds").is_instance_of::<Map>());
    for key in [
        "partial",
        "subagent",
        "openError",
        "promptError",
        "lastAgentError",
    ] {
        assert!(property(&snapshot, key).is_null(), "{key}");
    }
    assert_eq!(
        property(&snapshot, "composerPhase").as_string().as_deref(),
        Some("active")
    );
    assert_eq!(
        property(&snapshot, "openState").as_string().as_deref(),
        Some("open")
    );
    for key in ["running", "removed", "hasMore", "loadingOlder", "blank"] {
        assert_eq!(property(&snapshot, key).as_bool(), Some(false), "{key}");
    }

    let workspaces = workspace_list_state_js().unwrap();
    assert!(Array::is_array(&property(&workspaces, "items")));
    assert!(Array::is_array(&property(
        &workspaces,
        "archivedSessionIds"
    )));
    assert_eq!(
        property(&workspaces, "state").as_string().as_deref(),
        Some("idle")
    );
    assert_eq!(
        property(&workspaces, "phase").as_string().as_deref(),
        Some("ready")
    );
    assert!(property(&workspaces, "error").is_null());
    assert_eq!(
        property(&workspaces, "baselinesReady").as_bool(),
        Some(true)
    );
    assert!(property(&workspaces, "recentWorkspaceId").is_undefined());
}
