//! Generated catalog, visible prompt, and replay-safe presentation parity.

use seekdeep_tool_cordis::{
    api_catalog::{
        event_api, inherited_context_api, query_event_api, query_host_event_api, query_service_api,
        service_api, type_api,
    },
    cordis_system_prompt,
    present::{
        define_call, inspect_list_call, inspect_query_call, inspect_self_call, run_call, stop_call,
        undefine_call,
    },
};
use serde_json::json;

#[test]
fn generated_host_catalog_and_every_exact_query_are_self_consistent() {
    assert_eq!(service_api().len(), 55);
    assert_eq!(event_api().len(), 56);
    assert_eq!(type_api().len(), 510);
    assert_eq!(inherited_context_api().len(), 10);
    assert_eq!(
        query_service_api(None).unwrap()["services"]
            .as_array()
            .unwrap()
            .len(),
        55
    );
    for service in service_api() {
        let key = service["key"].as_str().unwrap();
        assert_eq!(query_service_api(Some(key)).unwrap()["service"]["key"], key);
    }
    for event in event_api() {
        let name = event["name"].as_str().unwrap();
        assert_eq!(query_event_api(Some(name)).unwrap()["event"]["name"], name);
        if !name.starts_with("cordis/") {
            assert_eq!(
                query_host_event_api(Some(name)).unwrap()["event"]["name"],
                name
            );
        }
    }
    assert!(query_service_api(Some("missing")).is_err());
    assert!(query_event_api(Some("missing")).is_err());
}

#[test]
fn prompt_preserves_workflow_safety_and_renamed_product_identity() {
    let prompt = cordis_system_prompt();
    assert!(prompt.starts_with("# Dynamic Cordis Plugins\n"));
    assert!(prompt.contains("cordis_define only defines and presents code; it does not run it"));
    assert!(prompt.contains("An update stops the old Run before starting the target Package"));
    assert!(prompt.contains("Use ctx.effect(), ctx.on(), or official APIs"));
    assert!(prompt.contains("SeekDeep Harness Host process"));
    assert!(!prompt.contains("DSH process"));
    assert!(!prompt.ends_with('\n'));
}

#[test]
fn presentation_wire_shapes_match_each_source_tool() {
    assert_eq!(
        serde_json::to_value(inspect_list_call()).unwrap(),
        json!({"card":"generic","title":"List Cordis Inspect Providers","kind":"read"})
    );
    assert_eq!(
        serde_json::to_value(inspect_query_call("host", "Service", "listService")).unwrap(),
        json!({"card":"generic","title":"Query Cordis host Service.listService","kind":"read"})
    );
    assert_eq!(
        serde_json::to_value(inspect_self_call(Some("abc-1"), Some("pkg-2"))).unwrap(),
        json!({"card":"generic","title":"Inspect abc-1/pkg-2","kind":"read"})
    );
    assert_eq!(
        serde_json::to_value(define_call(
            "new abc-*",
            "Example",
            "Show an example",
            &json!({"host":"return {}"}),
        ))
        .unwrap(),
        json!({
            "card":"generic",
            "title":"Register Cordis Plugin \"Example\" for new abc-*: Show an example",
            "kind":"execute",
            "rawInput":{"host":"return {}"}
        })
    );
    assert_eq!(
        serde_json::to_value(run_call("abc-1", "pkg-2", true)).unwrap()["title"],
        "Update Cordis Plugin abc-1 · pkg-2"
    );
    assert_eq!(
        serde_json::to_value(stop_call("abc-1")).unwrap()["title"],
        "Stop Cordis Plugin abc-1"
    );
    assert_eq!(
        serde_json::to_value(undefine_call("abc-1")).unwrap()["kind"],
        "delete"
    );
}
