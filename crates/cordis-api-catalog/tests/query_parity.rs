//! Queries are computed from catalog data; oracle responses are test inputs only.

use seekdeep_cordis_api_catalog::{RuntimeApiCatalog, query_event_api, query_service_api};
use serde_json::{Value, json};

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn every_pinned_host_and_client_query_matches() {
    for (catalog, fixtures) in [
        (
            include_str!("../../tool-cordis/data/api-catalog.json"),
            include_str!("../../tool-cordis/data/api-query-fixtures.json"),
        ),
        (
            include_str!("../../cordis-client-runner/data/api-catalog.json"),
            include_str!("../../cordis-client-runner/data/api-query-fixtures.json"),
        ),
    ] {
        let catalog: RuntimeApiCatalog = serde_json::from_str(catalog).unwrap();
        let fixtures: Value = serde_json::from_str(fixtures).unwrap();
        assert_eq!(
            query_service_api(None, &catalog.services, &catalog.types).unwrap(),
            fixtures["serviceCatalog"]
        );
        for service in &catalog.services {
            let key = service["key"].as_str().unwrap();
            assert_eq!(
                query_service_api(Some(key), &catalog.services, &catalog.types).unwrap(),
                fixtures["services"][key],
                "Service {key}"
            );
        }
        assert_eq!(
            query_event_api(None, &catalog.events, &catalog.types).unwrap(),
            fixtures["eventCatalog"]
        );
        for event in &catalog.events {
            let name = event["name"].as_str().unwrap();
            assert_eq!(
                query_event_api(Some(name), &catalog.events, &catalog.types).unwrap(),
                fixtures["events"][name],
                "Event {name}"
            );
        }
        if fixtures.get("hostEventCatalog").is_some() {
            let events = catalog
                .events
                .iter()
                .filter(|event| !event["name"].as_str().unwrap().starts_with("cordis/"))
                .cloned()
                .collect::<Vec<_>>();
            assert_eq!(
                query_event_api(None, &events, &catalog.types).unwrap(),
                fixtures["hostEventCatalog"]
            );
            for event in &events {
                let name = event["name"].as_str().unwrap();
                assert_eq!(
                    query_event_api(Some(name), &events, &catalog.types).unwrap(),
                    fixtures["hostEvents"][name]
                );
            }
        }
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test::wasm_bindgen_test)]
#[cfg_attr(not(target_arch = "wasm32"), test)]
fn custom_catalogs_preserve_method_records_key_quoting_and_source_delimiters() {
    let types = vec![
        json!({"name":"Beta","declaration":"interface Beta { next: \u{8}Alpha\u{8} }"}),
        json!({"name":"Alpha","declaration":"interface Alpha { next: \u{8}Beta\u{8} }"}),
        json!({"name":"unused","declaration":"type unused = number"}),
        json!({"name":"Alpha","declaration":"duplicate name also survives the final source-order filter"}),
    ];
    let method = json!({"signature":"run(value: \u{8}Alpha\u{8}): void","description":"Pass a value.","parameters":[],"futureMetadata":{"retained":true}});
    let services = vec![
        json!({"key":"quoted-\"key","summary":"Short.","description":"Complete.","methods":[method]}),
    ];
    let actual = query_service_api(Some("quoted-\"key"), &services, &types).unwrap();
    assert_eq!(actual["service"]["methods"][0], method);
    assert_eq!(
        actual["service"]["access"]["hardDependency"]["expression"],
        "ctx[\"quoted-\\\"key\"]"
    );
    assert_eq!(
        actual["referencedTypes"],
        json!([types[0], types[1], types[3]])
    );
    let events = vec![
        json!({"name":"changed","mode":"emit","signature":"changed(value: Alpha): void","summary":"Short.","description":"Complete.","parameters":[]}),
    ];
    assert_eq!(
        query_event_api(Some("changed"), &events, &types).unwrap()["referencedTypes"],
        json!([])
    );
    assert_eq!(
        query_service_api(Some("missing"), &services, &types)
            .unwrap_err()
            .to_string(),
        "no catalogued Service named \"missing\""
    );
    assert_eq!(
        query_event_api(Some("changed"), &[], &types)
            .unwrap_err()
            .to_string(),
        "no catalogued Event named \"changed\""
    );
}
