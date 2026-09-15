//! First-party Client Inspect provider manifest and owner-routing parity.

use std::sync::Arc;

use seekdeep_cordis_client_runner::*;
use seekdeep_identity::SessionId;
use serde_json::{Value, json};

fn context() -> ClientCordisInspectQueryContext {
    ClientCordisInspectQueryContext {
        signal: ClientAbortSignal::default(),
        session_id: SessionId::new("session-a"),
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
async fn provider_directory_order_schemas_builtins_and_exact_inputs_match_source() {
    let sources = ClientInspectProviderSources {
        services: Arc::new(|exact| Box::pin(async move { Ok(json!({"service": exact})) })),
        events: Arc::new(|exact| Box::pin(async move { Ok(json!({"event": exact})) })),
        slots: Arc::new(|_| Box::pin(async move { Ok(Vec::new()) })),
        theme: Arc::new(|| Box::pin(async { Ok(json!({"tokens": []})) })),
    };
    let providers = client_inspect_providers(sources);
    assert_eq!(
        providers
            .iter()
            .map(|provider| provider.manifest.id.as_str())
            .collect::<Vec<_>>(),
        ["Service", "Event", "Builtin", "Slots", "Theme"]
    );
    assert!(
        providers[0].manifest.methods[0].input_schema["properties"]
            .get("service")
            .is_some()
    );
    assert!(
        providers[1].manifest.methods[0].input_schema["properties"]
            .get("event")
            .is_some()
    );
    assert!(
        providers[3].manifest.methods[0].input_schema["properties"]
            .get("root")
            .is_some()
    );

    assert_eq!(
        (providers[0].query)(
            "listService".to_owned(),
            Some(json!({"service": "slots"})),
            context(),
        )
        .await
        .unwrap(),
        json!({"service": "slots"})
    );
    assert_eq!(
        (providers[1].query)(
            "listEvents".to_owned(),
            Some(Value::Array(Vec::new())),
            context(),
        )
        .await
        .unwrap(),
        json!({"event": null})
    );
    let builtins = (providers[2].query)("listBuiltins".to_owned(), None, context())
        .await
        .unwrap();
    assert_eq!(builtins["builtins"][0]["name"], "ctx");
    assert_eq!(builtins["builtins"][1]["name"], "React");
    assert_eq!(builtins["referencedTypes"], json!([]));
    assert_eq!(
        (providers[3].query)(
            "listSubTree".to_owned(),
            Some(json!({"root": "missing"})),
            context(),
        )
        .await
        .unwrap(),
        json!({
            "requestedRoot": {"name": "missing", "available": false},
            "trees": [],
            "referencedTypes": [],
        })
    );
    assert!(
        (providers[4].query)("unknown".to_owned(), None, context())
            .await
            .unwrap_err()
            .to_string()
            .contains("unknown Theme inspect method")
    );
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), tokio::test)]
async fn generated_sources_route_service_and_event_queries_without_adapter_reimplementation() {
    let sources = generated_client_inspect_sources(
        Arc::new(|_| Box::pin(async { Ok(Vec::new()) })),
        Arc::new(|| Box::pin(async { Ok(json!({"tokens": []})) })),
    );
    let providers = client_inspect_providers(sources);
    let service = (providers[0].query)(
        "listService".to_owned(),
        Some(json!({"service": "theme"})),
        context(),
    )
    .await
    .unwrap();
    assert_eq!(service["mode"], "service");
    assert_eq!(service["service"]["key"], "theme");
    let events = (providers[1].query)("listEvents".to_owned(), None, context())
        .await
        .unwrap();
    assert_eq!(events["mode"], "catalog");
    assert!(
        events["events"]
            .as_array()
            .is_some_and(|events| !events.is_empty())
    );
}
