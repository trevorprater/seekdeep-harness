//! Generated Client Slot catalog plus live compact/full subtree projection.

use std::{collections::BTreeMap, sync::OnceLock};

use serde::Deserialize;
use serde_json::{Value, json};

const SLOT_SOURCE: &str = include_str!("../data/slot-catalog.json");

fn catalog() -> &'static Value {
    static CATALOG: OnceLock<Value> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str(SLOT_SOURCE).expect("generated Client Slot catalog must be valid JSON")
    })
}

fn catalog_array(key: &str) -> &'static [Value] {
    catalog()[key]
        .as_array()
        .expect("generated Slot catalog field must be an array")
}

/// Generated Slot authoring notes.
#[must_use]
pub fn client_slot_notes() -> &'static [Value] {
    catalog_array("notes")
}

/// Generated Slot contracts in source order.
#[must_use]
pub fn client_slot_api() -> &'static [Value] {
    catalog_array("entries")
}

/// One live Slot node returned by the browser Slot registry.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LiveSlotNode {
    /// Exact Slot key.
    pub name: String,
    /// Live Slot kind.
    pub kind: String,
    /// Live scope.
    pub scope: String,
    /// Entry that declared this Slot.
    pub declared_by: Option<String>,
    /// Current live occupant rows.
    pub occupants: Vec<Value>,
    /// Nested live Slots.
    pub children: Vec<LiveSlotNode>,
}

/// Projects compact navigation trees and one optional selected full contract.
#[must_use]
pub fn query_client_slots(root: Option<&str>, trees: &[LiveSlotNode]) -> Value {
    let selected = trees.first();
    let mut output = serde_json::Map::new();
    if let Some(root) = root {
        output.insert(
            "requestedRoot".to_owned(),
            json!({"name": root, "available": !trees.is_empty()}),
        );
    }
    output.insert(
        "trees".to_owned(),
        Value::Array(trees.iter().map(compact_slot_tree).collect()),
    );
    if root.is_some()
        && let Some(selected) = selected
    {
        output.insert("selected".to_owned(), inspect_live_slot(selected));
    }
    output.insert("referencedTypes".to_owned(), Value::Array(Vec::new()));
    Value::Object(output)
}

fn catalog_map() -> BTreeMap<&'static str, &'static Value> {
    client_slot_api()
        .iter()
        .filter_map(|entry| Some((entry.get("key")?.as_str()?, entry)))
        .collect()
}

fn compact_slot_tree(node: &LiveSlotNode) -> Value {
    let catalog = catalog_map().get(node.name.as_str()).copied();
    let mut output = json!({
        "name": node.name,
        "kind": node.kind,
        "scope": node.scope,
        "children": node.children.iter().map(compact_slot_tree).collect::<Vec<_>>(),
    });
    let Some(catalog) = catalog else {
        return output;
    };
    let object = output.as_object_mut().expect("literal object");
    object.insert("purpose".to_owned(), catalog["summary"].clone());
    object.insert("replaceRisk".to_owned(), catalog["replaceRisk"].clone());
    if let Some(options) = catalog["registerOptions"].as_array()
        && !options.is_empty()
    {
        object.insert(
            "registration".to_owned(),
            Value::Array(
                options
                    .iter()
                    .map(|option| {
                        json!({
                            "name": option["name"],
                            "type": option["type"],
                            "required": option["requirement"] == "required",
                        })
                    })
                    .collect(),
            ),
        );
    }
    if catalog["keyDomain"]
        .as_str()
        .is_some_and(|domain| !domain.is_empty())
    {
        if node.name == "tool.view.cordis" {
            object.insert(
                "keyDomain".to_owned(),
                Value::String("fixed by the dynamic Client Guard".to_owned()),
            );
            object.insert(
                "allowedKeys".to_owned(),
                json!([{
                    "value": "self",
                    "description": "The only accepted key. The Guard binds it to this Package's pluginId and packageId.",
                }]),
            );
        } else {
            object.insert("keyDomain".to_owned(), catalog["keyDomain"].clone());
        }
    }
    output
}

fn inspect_live_slot(node: &LiveSlotNode) -> Value {
    let catalog = catalog_map().get(node.name.as_str()).copied();
    let mut output = json!({
        "name": node.name,
        "kind": node.kind,
        "scope": node.scope,
        "occupants": node.occupants,
    });
    let object = output.as_object_mut().expect("literal object");
    if let Some(declared_by) = &node.declared_by {
        object.insert("declaredBy".to_owned(), Value::String(declared_by.clone()));
    }
    if let Some(catalog) = catalog {
        object.insert(
            "catalog".to_owned(),
            inspect_slot_catalog(catalog, &node.name),
        );
    }
    output
}

fn inspect_slot_catalog(entry: &Value, key: &str) -> Value {
    let guarded = key == "tool.view.cordis";
    let mut output = json!({
        "description": entry["doc"],
        "registration": entry["registerOptions"].as_array().into_iter().flatten().map(|option| json!({
            "name": option["name"],
            "type": option["type"],
            "required": option["requirement"] == "required",
            "description": option["doc"],
        })).collect::<Vec<_>>(),
        "ownerProps": entry["ownerProps"],
        "ownerPropsReferences": entry["ownerPropsReferences"],
        "standardProps": entry["standardProps"],
        "keyDomain": if guarded {
            Value::String("fixed by the dynamic Client Guard".to_owned())
        } else {
            entry["keyDomain"].clone()
        },
        "hookContext": entry["hookContext"],
        "slotInject": entry["slotInject"],
        "replaceRisk": entry["replaceRisk"],
    });
    if guarded {
        output
            .as_object_mut()
            .expect("literal object")
            .insert(
                "allowedKeys".to_owned(),
                json!([{
                    "value": "self",
                    "description": "The only accepted key. The Guard binds it to this Package's pluginId and packageId.",
                }]),
            );
    }
    output
}
