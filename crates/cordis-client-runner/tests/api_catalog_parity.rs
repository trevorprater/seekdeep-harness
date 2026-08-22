//! Differential parity against every pinned Client Service/Event query result.

use seekdeep_cordis_client_runner::{
    client_event_api, client_service_api, client_type_api, inherited_context_api,
    query_client_event_api, query_client_service_api,
};
use serde_json::Value;

fn fixtures() -> Value {
    serde_json::from_str(include_str!("../data/api-query-fixtures.json")).unwrap()
}

#[test]
fn compact_and_every_exact_service_query_match_the_pinned_source() {
    let fixtures = fixtures();
    assert_eq!(
        query_client_service_api(None).unwrap(),
        fixtures["serviceCatalog"]
    );
    for service in client_service_api() {
        let key = service["key"].as_str().unwrap();
        assert_eq!(
            query_client_service_api(Some(key)).unwrap(),
            fixtures["services"][key],
            "{key}"
        );
    }
    assert!(query_client_service_api(Some("missing")).is_err());
}

#[test]
fn compact_and_every_exact_event_query_match_the_pinned_source() {
    let fixtures = fixtures();
    assert_eq!(
        query_client_event_api(None).unwrap(),
        fixtures["eventCatalog"]
    );
    for event in client_event_api() {
        let name = event["name"].as_str().unwrap();
        assert_eq!(
            query_client_event_api(Some(name)).unwrap(),
            fixtures["events"][name],
            "{name}"
        );
    }
    assert!(query_client_event_api(Some("missing")).is_err());
}

#[test]
fn generated_catalog_contains_types_and_inherited_context_directory() {
    assert!(!client_type_api().is_empty());
    assert_eq!(inherited_context_api().len(), 10);
    assert_eq!(inherited_context_api()[0]["name"], "ctx.on / ctx.once");
}
