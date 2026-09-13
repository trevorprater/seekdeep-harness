//! Generated Client Service/Event catalogs and exact query projections.

use std::sync::OnceLock;

use serde_json::{Map, Value};

const CATALOG_SOURCE: &str = include_str!("../data/api-catalog.json");

fn catalog() -> &'static Value {
    static CATALOG: OnceLock<Value> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(CATALOG_SOURCE)
            .expect("generated Client API catalog must be valid JSON")
    })
}

fn catalog_array(key: &str) -> &'static [Value] {
    catalog()[key]
        .as_array()
        .expect("generated catalog field must be an array")
}

/// Generated visible Client Service entries in source order.
#[must_use]
pub fn client_service_api() -> &'static [Value] {
    catalog_array("services")
}

/// Generated visible Client Event entries in source order.
#[must_use]
pub fn client_event_api() -> &'static [Value] {
    catalog_array("events")
}

/// Generated Type declarations used by Client catalogs.
#[must_use]
pub fn client_type_api() -> &'static [Value] {
    catalog_array("types")
}

/// Generated inherited Context directory.
#[must_use]
pub fn inherited_context_api() -> &'static [Value] {
    catalog_array("inheritedContext")
}

/// Returns the compact Service directory or one exact coding contract.
///
/// # Errors
///
/// Rejects an exact key absent from the pinned generated catalog.
pub fn query_client_service_api(key: Option<&str>) -> anyhow::Result<Value> {
    seekdeep_cordis_api_catalog::query_service_api(key, client_service_api(), client_type_api())
}

/// Returns the compact Event directory or one exact listener contract.
///
/// # Errors
///
/// Rejects an exact name absent from the pinned generated catalog.
pub fn query_client_event_api(name: Option<&str>) -> anyhow::Result<Value> {
    seekdeep_cordis_api_catalog::query_event_api(name, client_event_api(), client_type_api())
}

/// Detaches an arbitrary generated catalog object for callers that need ownership.
#[must_use]
pub fn clone_catalog_object(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}
