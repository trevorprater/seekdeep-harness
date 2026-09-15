//! Schema rehydration, validation, path lookup, and immutable draft editing.

use std::sync::Arc;

use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_schemastery::Schema;
use serde_json::{Map, Value};

#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;

/// Stable invariant companion name.
pub const INVARIANT_NAME: &str = "client-schema-form-invariant";
const PACKAGE_NAME: &str = "@seekdeep-ai/seekdeep-client-schema-form";

/// Rehydrates a serialized Schemastery envelope.
///
/// # Errors
///
/// Returns malformed graph, relation, or node-kind failures.
pub fn rehydrate_schema(serialized: &Value) -> anyhow::Result<Schema> {
    Schema::from_json(serialized)
}

/// Returns a validation failure message, or `None` for a valid draft.
#[must_use]
pub fn validate_draft(schema: &Schema, draft: &Value) -> Option<String> {
    schema.resolve(draft).err().map(|error| error.message)
}

/// Resolves one structural schema node by object/dict/array path.
#[must_use]
pub fn node_at_path(root: &Schema, path: &[String]) -> Option<Schema> {
    let mut node = root.clone();
    for key in path {
        node = match node.kind_name() {
            "object" => node.field(key)?,
            "dict" | "array" => node.inner()?,
            _ => return None,
        };
    }
    Some(node)
}

/// Reads one JSON value by object/array path.
#[must_use]
pub fn get_path<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = match current {
            Value::Array(values) => values.get(parse_index(key)?),
            Value::Object(values) => values.get(key),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
        }?;
    }
    Some(current)
}

/// Tests explicit path presence independently of leaf truthiness.
#[must_use]
pub fn has_path(value: Option<&Value>, path: &[String]) -> bool {
    let Some(value) = value else {
        return false;
    };
    let Some((leaf, parent_path)) = path.split_last() else {
        return true;
    };
    let Some(parent) = get_path(value, parent_path) else {
        return false;
    };
    match parent {
        Value::Array(values) => parse_index(leaf).is_some_and(|index| index < values.len()),
        Value::Object(values) => values.contains_key(leaf),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

/// Immutably sets a non-empty draft path.
///
/// # Errors
///
/// Rejects an empty path.
pub fn set_path(root: &Arc<Value>, path: &[String], value: Value) -> anyhow::Result<Arc<Value>> {
    anyhow::ensure!(
        !path.is_empty(),
        "schema-form: setPath needs a non-empty path"
    );
    Ok(Arc::new(set_node(root, path, value)))
}

/// Immutably removes a non-empty draft path, retaining identity on a miss.
///
/// # Errors
///
/// Rejects an empty path.
pub fn delete_path(root: &Arc<Value>, path: &[String]) -> anyhow::Result<Arc<Value>> {
    anyhow::ensure!(
        !path.is_empty(),
        "schema-form: deletePath needs a non-empty path"
    );
    if !has_path(Some(root), path) {
        return Ok(root.clone());
    }
    Ok(Arc::new(delete_node(root, path)))
}

/// Registers the explained-empty invariant companion.
///
/// # Errors
///
/// Returns ordinary invariant-registry failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(PACKAGE_NAME, InvariantInstaller::noop())
}

fn set_node(current: &Value, path: &[String], value: Value) -> Value {
    let key = &path[0];
    if path.len() == 1 {
        return set_child(clone_container(current, key), key, value);
    }
    let child = child(current, key).cloned().unwrap_or(Value::Null);
    let child = if child.is_array() || child.is_object() {
        child
    } else {
        materialized_container(&path[1])
    };
    let replacement = set_node(&child, &path[1..], value);
    set_child(clone_container(current, key), key, replacement)
}

fn delete_node(current: &Value, path: &[String]) -> Value {
    let key = &path[0];
    let mut container = clone_container(current, key);
    if path.len() == 1 {
        match &mut container {
            Value::Array(values) => {
                if let Some(index) = parse_index(key)
                    && index < values.len()
                {
                    values.remove(index);
                }
            }
            Value::Object(values) => {
                values.remove(key);
            }
            _ => unreachable!("clone_container returns a container"),
        }
        return container;
    }
    let replacement = delete_node(
        child(current, key).expect("has_path proved intermediate"),
        &path[1..],
    );
    set_child(container, key, replacement)
}

fn child<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Array(values) => values.get(parse_index(key)?),
        Value::Object(values) => values.get(key),
        _ => None,
    }
}

fn clone_container(value: &Value, key: &str) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.clone()),
        Value::Object(values) => Value::Object(values.clone()),
        _ => materialized_container(key),
    }
}

fn materialized_container(next_key: &str) -> Value {
    if is_decimal_index(next_key) {
        Value::Array(Vec::new())
    } else {
        Value::Object(Map::new())
    }
}

fn set_child(mut container: Value, key: &str, value: Value) -> Value {
    match &mut container {
        Value::Array(values) => {
            let index = parse_index(key).unwrap_or(0);
            if values.len() <= index {
                values.resize(index + 1, Value::Null);
            }
            values[index] = value;
        }
        Value::Object(values) => {
            values.insert(key.to_owned(), value);
        }
        _ => unreachable!("clone_container returns a container"),
    }
    container
}

fn parse_index(key: &str) -> Option<usize> {
    key.parse().ok()
}

fn is_decimal_index(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(|byte| byte.is_ascii_digit())
}
