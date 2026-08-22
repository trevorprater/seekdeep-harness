//! Generated Client Service/Event catalogs and exact query projections.

use std::sync::OnceLock;

use serde_json::{Map, Value, json};

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
    let services = client_service_api();
    let Some(key) = key else {
        return Ok(json!({
            "mode": "catalog",
            "services": services.iter().map(|service| json!({
                "key": service["key"],
                "description": service["summary"],
                "methods": service["methods"].as_array().into_iter().flatten().map(|method| {
                    json!({"signature": method["signature"]})
                }).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        }));
    };
    let service = services
        .iter()
        .find(|service| service["key"].as_str() == Some(key))
        .ok_or_else(|| anyhow::anyhow!("no catalogued Service named \"{key}\""))?;
    let expression = context_property(key);
    Ok(json!({
        "mode": "service",
        "service": {
            "key": service["key"],
            "description": service["description"],
            "access": {
                "optional": {
                    "expression": format!("ctx.get({})", serde_json::to_string(key)?),
                    "requiresUndefinedCheck": true,
                },
                "hardDependency": {
                    "inject": [key],
                    "expression": expression,
                },
            },
            "methods": service["methods"],
        },
        "referencedTypes": [],
    }))
}

/// Returns the compact Event directory or one exact listener contract.
///
/// # Errors
///
/// Rejects an exact name absent from the pinned generated catalog.
pub fn query_client_event_api(name: Option<&str>) -> anyhow::Result<Value> {
    let events = client_event_api();
    let Some(name) = name else {
        return Ok(json!({
            "mode": "catalog",
            "events": events.iter().map(|event| json!({
                "name": event["name"],
                "description": event["summary"],
                "mode": event["mode"],
                "signature": event["signature"],
            })).collect::<Vec<_>>(),
        }));
    };
    let event = events
        .iter()
        .find(|event| event["name"].as_str() == Some(name))
        .ok_or_else(|| anyhow::anyhow!("no catalogued Event named \"{name}\""))?;
    Ok(json!({
        "mode": "event",
        "event": {
            "name": event["name"],
            "description": event["description"],
            "mode": event["mode"],
            "signature": event["signature"],
            "parameters": event["parameters"],
        },
        "referencedTypes": [],
    }))
}

fn context_property(key: &str) -> String {
    let mut characters = key.chars();
    let direct = characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || matches!(character, '_' | '$'))
        && characters
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '$'));
    if direct {
        format!("ctx.{key}")
    } else {
        format!(
            "ctx[{}]",
            serde_json::to_string(key).expect("Service key is a JSON string")
        )
    }
}

/// Detaches an arbitrary generated catalog object for callers that need ownership.
#[must_use]
pub fn clone_catalog_object(value: &Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}
