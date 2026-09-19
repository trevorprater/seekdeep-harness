//! Target-portable queries over generated Cordis catalog data.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Generated catalog partitions, independent of an evaluator or registry.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeApiCatalog {
    /// Service descriptions and authored method contracts.
    pub services: Vec<Value>,
    /// Event descriptions and authored listener contracts.
    pub events: Vec<Value>,
    /// Public type declarations in catalog order.
    pub types: Vec<Value>,
    /// Curated inherited Context directory.
    pub inherited_context: Vec<Value>,
}

/// Projects a directory or one exact Service contract from the supplied catalog.
///
/// The visible Service subset does not restrict the shared type catalog.
/// Unknown fields on retained method records survive the projection.
///
/// # Errors
/// Rejects an unknown key or a malformed generated catalog record.
pub fn query_service_api(
    key: Option<&str>,
    services: &[Value],
    types: &[Value],
) -> anyhow::Result<Value> {
    let Some(key) = key else {
        let mut entries = Vec::new();
        for service in services {
            entries.push(json!({
                "key": service["key"],
                "description": service["summary"],
                "methods": array(service, "methods")?.iter().map(|method| json!({
                    "signature": method["signature"],
                })).collect::<Vec<_>>(),
            }));
        }
        return Ok(json!({"mode":"catalog", "services":entries}));
    };
    let service = services
        .iter()
        .find(|service| service["key"].as_str() == Some(key))
        .ok_or_else(|| anyhow::anyhow!("no catalogued Service named \"{key}\""))?;
    let seeds = array(service, "methods")?
        .iter()
        .map(|method| string(method, "signature"))
        .collect::<anyhow::Result<Vec<_>>>()?;
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
                    "expression": context_property(key),
                },
            },
            "methods": service["methods"],
        },
        "referencedTypes": referenced_type_closure(&seeds, types)?,
    }))
}

/// Projects a directory or one exact listener contract from a visible Event subset.
///
/// # Errors
/// Rejects an unknown name or a malformed generated catalog record.
pub fn query_event_api(
    name: Option<&str>,
    events: &[Value],
    types: &[Value],
) -> anyhow::Result<Value> {
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
        "referencedTypes": referenced_type_closure(&[string(event, "signature")?], types)?,
    }))
}

fn referenced_type_closure(seeds: &[&str], types: &[Value]) -> anyhow::Result<Vec<Value>> {
    let mut included = HashSet::new();
    let mut frontier = seeds.to_vec();
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for entry in types {
            let name = string(entry, "name")?;
            if included.contains(name) {
                continue;
            }
            // The pinned generated JavaScript evaluates its single-escaped
            // `\b` template literals as backspaces, unlike the build-time walk.
            let pattern = regress::Regex::new(&format!("\u{8}{name}\u{8}"))?;
            if frontier.iter().any(|text| pattern.find(text).is_some()) {
                included.insert(name);
                next.push(string(entry, "declaration")?);
            }
        }
        frontier = next;
    }
    Ok(types
        .iter()
        .filter(|entry| {
            entry["name"]
                .as_str()
                .is_some_and(|name| included.contains(name))
        })
        .cloned()
        .collect())
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
        format!("ctx[{}]", serde_json::to_string(key).expect("JSON string"))
    }
}

fn string<'a>(record: &'a Value, field: &str) -> anyhow::Result<&'a str> {
    record[field]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("generated catalog field {field} must be a string"))
}

fn array<'a>(record: &'a Value, field: &str) -> anyhow::Result<&'a [Value]> {
    record[field]
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| anyhow::anyhow!("generated catalog field {field} must be an array"))
}
