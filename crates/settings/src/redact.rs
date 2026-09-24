//! Structural redaction for schema-declared settings secrets.

use seekdeep_schemastery::{Schema, SchemaKind};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One schema-declared secret position inside a redacted value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactedSecret {
    /// Path from the section root to the removed field.
    pub path: Vec<String>,
    /// Whether the field held a value before redaction.
    pub set: bool,
}

/// A detached value with schema-declared secrets removed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RedactedValue {
    /// Redacted value. `None` represents JavaScript `undefined`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// Every reachable secret slot.
    pub secrets: Vec<RedactedSecret>,
}

/// Removes every structurally reachable `role("secret")` field.
#[must_use]
pub fn redact_secrets(schema: &Schema, value: Option<&Value>) -> RedactedValue {
    let mut secrets = Vec::new();
    let value = walk(schema, value, &mut Vec::new(), &mut secrets);
    RedactedValue { value, secrets }
}

fn walk(
    schema: &Schema,
    value: Option<&Value>,
    path: &mut Vec<String>,
    secrets: &mut Vec<RedactedSecret>,
) -> Option<Value> {
    if schema.meta().role.as_deref() == Some("secret") {
        secrets.push(RedactedSecret {
            path: path.clone(),
            set: value.is_some(),
        });
        return None;
    }
    match schema.kind() {
        SchemaKind::Object(fields) => walk_object(fields, value, path, secrets),
        SchemaKind::Dict { inner, .. } => {
            let Some(object) = value.and_then(Value::as_object) else {
                return value.cloned();
            };
            let mut output = Map::new();
            for (key, entry) in object {
                path.push(key.clone());
                let stripped = walk(inner, Some(entry), path, secrets);
                path.pop();
                if let Some(stripped) = stripped {
                    output.insert(key.clone(), stripped);
                }
            }
            Some(Value::Object(output))
        }
        SchemaKind::Array(inner) => {
            let Some(array) = value.and_then(Value::as_array) else {
                return value.cloned();
            };
            Some(Value::Array(
                array
                    .iter()
                    .enumerate()
                    .map(|(index, entry)| {
                        path.push(index.to_string());
                        let stripped = walk(inner, Some(entry), path, secrets);
                        path.pop();
                        stripped.unwrap_or(Value::Null)
                    })
                    .collect(),
            ))
        }
        _ => value.cloned(),
    }
}

fn walk_object(
    fields: &indexmap::IndexMap<String, Schema>,
    value: Option<&Value>,
    path: &mut Vec<String>,
    secrets: &mut Vec<RedactedSecret>,
) -> Option<Value> {
    let source = value.and_then(Value::as_object);
    let mut output = Map::new();
    if let Some(source) = source {
        for (key, entry) in source {
            if !fields.contains_key(key) {
                output.insert(key.clone(), entry.clone());
            }
        }
    }
    for (key, child) in fields {
        path.push(key.clone());
        let stripped = walk(
            child,
            source.and_then(|source| source.get(key)),
            path,
            secrets,
        );
        path.pop();
        if let Some(stripped) = stripped {
            output.insert(key.clone(), stripped);
        }
    }
    if source.is_none() && output.is_empty() {
        value.cloned()
    } else {
        Some(Value::Object(output))
    }
}
