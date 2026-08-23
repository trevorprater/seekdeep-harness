//! Pinned generated Host Service/Event catalogs and exact query projections.

use std::sync::OnceLock;

use serde_json::Value;

const CATALOG_SOURCE: &str = include_str!("../data/api-catalog.json");
const FIXTURE_SOURCE: &str = include_str!("../data/api-query-fixtures.json");

fn catalog() -> &'static Value {
    static CATALOG: OnceLock<Value> = OnceLock::new();
    CATALOG.get_or_init(|| serde_json::from_str(CATALOG_SOURCE).expect("valid Host API catalog"))
}

fn fixtures() -> &'static Value {
    static FIXTURES: OnceLock<Value> = OnceLock::new();
    FIXTURES.get_or_init(|| serde_json::from_str(FIXTURE_SOURCE).expect("valid Host API fixtures"))
}

/// Generated Host Service entries in source order.
///
/// # Panics
///
/// Panics only if the checked-in generated catalog is malformed.
#[must_use]
pub fn service_api() -> &'static [Value] {
    catalog()["services"].as_array().expect("services array")
}

/// Generated Host Event entries in source order.
///
/// # Panics
///
/// Panics only if the checked-in generated catalog is malformed.
#[must_use]
pub fn event_api() -> &'static [Value] {
    catalog()["events"].as_array().expect("events array")
}

/// Generated public type declarations.
///
/// # Panics
///
/// Panics only if the checked-in generated catalog is malformed.
#[must_use]
pub fn type_api() -> &'static [Value] {
    catalog()["types"].as_array().expect("types array")
}

/// Generated inherited Context API directory.
///
/// # Panics
///
/// Panics only if the checked-in generated catalog is malformed.
#[must_use]
pub fn inherited_context_api() -> &'static [Value] {
    catalog()["inheritedContext"]
        .as_array()
        .expect("inherited Context array")
}

/// Returns the exact source Service catalog or named contract.
///
/// # Errors
///
/// Rejects an unknown Service key.
pub fn query_service_api(key: Option<&str>) -> anyhow::Result<Value> {
    key.map_or_else(
        || Ok(fixtures()["serviceCatalog"].clone()),
        |key| {
            fixtures()["services"]
                .get(key)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no catalogued Service named \"{key}\""))
        },
    )
}

/// Returns the exact source Event catalog or named contract.
///
/// # Errors
///
/// Rejects an unknown Event name.
pub fn query_event_api(name: Option<&str>) -> anyhow::Result<Value> {
    name.map_or_else(
        || Ok(fixtures()["eventCatalog"].clone()),
        |name| {
            fixtures()["events"]
                .get(name)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no catalogued Event named \"{name}\""))
        },
    )
}

/// Host-provider Event projection excluding dynamic Cordis control events.
///
/// # Errors
///
/// Rejects an unknown or excluded Event name.
pub fn query_host_event_api(name: Option<&str>) -> anyhow::Result<Value> {
    name.map_or_else(
        || Ok(fixtures()["hostEventCatalog"].clone()),
        |name| {
            fixtures()["hostEvents"]
                .get(name)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no catalogued Event named \"{name}\""))
        },
    )
}
