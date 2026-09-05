//! JavaScript-compatible path and validator facade compiled into WASM.

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_schemastery::Schema;
use wasm_bindgen::{JsCast, JsValue, prelude::wasm_bindgen};

/// Opaque live schema node returned to browser settings editors.
#[wasm_bindgen]
pub struct WasmSchemaNode {
    schema: Schema,
}

#[wasm_bindgen]
impl WasmSchemaNode {
    #[wasm_bindgen(js_name = __seekdeepValidate)]
    /// Validates one JSON-compatible draft.
    ///
    /// # Errors
    ///
    /// Returns conversion or schema validation failures.
    pub fn validate(&self, draft: JsValue) -> Result<(), JsValue> {
        let draft: serde_json::Value = serde_wasm_bindgen::from_value(draft)
            .map_err(|error| js_sys::Error::new(&error.to_string()))?;
        self.schema
            .resolve(&draft)
            .map(|_| ())
            .map_err(|error| js_sys::Error::new(&error.message).into())
    }

    #[wasm_bindgen(js_name = __seekdeepNodeAtPath)]
    /// Resolves one child node for the compatibility free function.
    #[allow(clippy::needless_pass_by_value)]
    pub fn node_at_path(&self, path: Array) -> Option<WasmSchemaNode> {
        let path = string_path(&path).ok()?;
        crate::node_at_path(&self.schema, &path).map(|schema| Self { schema })
    }

    /// Structural node type.
    #[wasm_bindgen(getter, js_name = type)]
    pub fn kind(&self) -> String {
        self.schema.kind_name().to_owned()
    }
}

/// Rehydrates one serialized Schemastery envelope.
///
/// # Errors
///
/// Returns malformed graph failures as JavaScript errors.
#[wasm_bindgen(js_name = rehydrateSchema)]
#[allow(clippy::needless_pass_by_value)]
pub fn rehydrate_schema_js(serialized: JsValue) -> Result<WasmSchemaNode, JsValue> {
    let serialized: serde_json::Value = serde_wasm_bindgen::from_value(serialized)
        .map_err(|error| js_sys::Error::new(&error.to_string()))?;
    Ok(WasmSchemaNode {
        schema: crate::rehydrate_schema(&serialized)
            .map_err(|error| js_sys::Error::new(&error.to_string()))?,
    })
}

/// Validates a draft and stringifies Error and non-Error throws.
#[wasm_bindgen(js_name = validateDraft)]
#[allow(clippy::needless_pass_by_value)]
pub fn validate_draft_js(schema: JsValue, draft: JsValue) -> Option<String> {
    let candidate = Reflect::get(&schema, &JsValue::from_str("__seekdeepValidate"))
        .ok()
        .filter(JsValue::is_function)
        .or_else(|| schema.is_function().then_some(schema.clone()));
    let Some(candidate) = candidate.and_then(|value| value.dyn_into::<Function>().ok()) else {
        return Some("schema-form: value is not a schema validator".to_owned());
    };
    candidate
        .call1(&schema, &draft)
        .err()
        .map(|error| error_text(&error))
}

/// Resolves a structural schema node by path.
#[wasm_bindgen(js_name = nodeAtPath)]
#[allow(clippy::needless_pass_by_value)]
pub fn node_at_path_js(schema: JsValue, path: Array) -> JsValue {
    let Ok(candidate) = Reflect::get(&schema, &JsValue::from_str("__seekdeepNodeAtPath")) else {
        return JsValue::UNDEFINED;
    };
    let Ok(candidate) = candidate.dyn_into::<Function>() else {
        return JsValue::UNDEFINED;
    };
    candidate
        .call1(&schema, &path)
        .unwrap_or(JsValue::UNDEFINED)
}

/// Reads a nested object or array value.
#[wasm_bindgen(js_name = getPath)]
#[allow(clippy::needless_pass_by_value)]
pub fn get_path_js(value: JsValue, path: Array) -> JsValue {
    let mut current = value;
    for key in path.iter().filter_map(|value| value.as_string()) {
        if Array::is_array(&current) {
            current = Reflect::get(&current, &numeric_key(&key)).unwrap_or(JsValue::UNDEFINED);
        } else if current.is_object() && !current.is_null() {
            current =
                Reflect::get(&current, &JsValue::from_str(&key)).unwrap_or(JsValue::UNDEFINED);
        } else {
            return JsValue::UNDEFINED;
        }
    }
    current
}

/// Tests explicit final-key presence, including an `undefined` leaf.
#[wasm_bindgen(js_name = hasPath)]
#[allow(clippy::needless_pass_by_value)]
pub fn has_path_js(value: JsValue, path: Array) -> bool {
    if path.length() == 0 {
        return !value.is_undefined();
    }
    let parent = get_path_js(value, path.slice(0, path.length() - 1));
    let Some(key) = path.get(path.length() - 1).as_string() else {
        return false;
    };
    if Array::is_array(&parent) {
        return numeric_index(&key).is_some_and(|index| index < Array::from(&parent).length());
    }
    parent.is_object()
        && !parent.is_null()
        && Reflect::has(&parent, &JsValue::from_str(&key)).unwrap_or(false)
}

/// Immutably sets a non-empty nested path.
///
/// # Errors
///
/// Rejects an empty or non-string path.
#[wasm_bindgen(js_name = setPath)]
#[allow(clippy::needless_pass_by_value)]
pub fn set_path_js(root: JsValue, path: Array, value: JsValue) -> Result<JsValue, JsValue> {
    let path = string_path(&path)?;
    if path.is_empty() {
        return Err(js_sys::Error::new("schema-form: setPath needs a non-empty path").into());
    }
    set_node(&root, &path, value)
}

/// Immutably deletes a non-empty nested path.
///
/// # Errors
///
/// Rejects an empty or non-string path.
#[wasm_bindgen(js_name = deletePath)]
#[allow(clippy::needless_pass_by_value)]
pub fn delete_path_js(root: JsValue, path: Array) -> Result<JsValue, JsValue> {
    let keys = string_path(&path)?;
    if keys.is_empty() {
        return Err(js_sys::Error::new("schema-form: deletePath needs a non-empty path").into());
    }
    if !has_path_js(root.clone(), path) {
        return Ok(root);
    }
    delete_node(&root, &keys)
}

fn set_node(current: &JsValue, path: &[String], value: JsValue) -> Result<JsValue, JsValue> {
    let key = &path[0];
    let container = clone_container(current, key);
    if path.len() == 1 {
        set_child(&container, key, &value)?;
        return Ok(container);
    }
    let child = get_child(current, key).unwrap_or(JsValue::UNDEFINED);
    let child = if Array::is_array(&child) || child.is_object() && !child.is_null() {
        child
    } else {
        materialized_container(&path[1])
    };
    let child = set_node(&child, &path[1..], value)?;
    set_child(&container, key, &child)?;
    Ok(container)
}

fn delete_node(current: &JsValue, path: &[String]) -> Result<JsValue, JsValue> {
    let key = &path[0];
    let container = clone_container(current, key);
    if path.len() == 1 {
        if Array::is_array(&container) {
            if let Some(index) = numeric_index(key) {
                let array = Array::from(&container);
                if index < array.length() {
                    let without = Array::new();
                    for at in 0..array.length() {
                        if at != index {
                            without.push(&array.get(at));
                        }
                    }
                    return Ok(without.into());
                }
            }
        } else {
            Reflect::delete_property(&Object::from(container.clone()), &JsValue::from_str(key))?;
        }
        return Ok(container);
    }
    let child = get_child(current, key)
        .ok_or_else(|| js_sys::Error::new("schema-form: deletePath intermediate disappeared"))?;
    let child = delete_node(&child, &path[1..])?;
    set_child(&container, key, &child)?;
    Ok(container)
}

fn clone_container(value: &JsValue, key: &str) -> JsValue {
    if Array::is_array(value) {
        return Array::from(value)
            .slice(0, Array::from(value).length())
            .into();
    }
    if value.is_object() && !value.is_null() {
        return Object::assign(&Object::new(), &Object::from(value.clone())).into();
    }
    materialized_container(key)
}

fn materialized_container(next_key: &str) -> JsValue {
    if decimal_index(next_key) {
        Array::new().into()
    } else {
        Object::new().into()
    }
}

fn get_child(value: &JsValue, key: &str) -> Option<JsValue> {
    let property = if Array::is_array(value) {
        numeric_key(key)
    } else {
        JsValue::from_str(key)
    };
    Reflect::get(value, &property).ok()
}

fn set_child(container: &JsValue, key: &str, value: &JsValue) -> Result<(), JsValue> {
    let property = if Array::is_array(container) {
        numeric_key(key)
    } else {
        JsValue::from_str(key)
    };
    if Reflect::set(container, &property, value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new("schema-form: path assignment failed").into())
    }
}

fn string_path(path: &Array) -> Result<Vec<String>, JsValue> {
    path.iter()
        .map(|value| {
            value.as_string().ok_or_else(|| {
                js_sys::Error::new("schema-form: path entries must be strings").into()
            })
        })
        .collect()
}

fn numeric_key(key: &str) -> JsValue {
    JsValue::from_f64(numeric_value(key))
}

fn numeric_index(key: &str) -> Option<u32> {
    let value = numeric_value(key);
    if value.is_finite() && value >= 0.0 && value.fract() == 0.0 && value <= f64::from(u32::MAX) {
        value.to_string().parse().ok()
    } else {
        None
    }
}

fn numeric_value(key: &str) -> f64 {
    if key.is_empty() {
        0.0
    } else {
        key.parse().unwrap_or(f64::NAN)
    }
}

fn decimal_index(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(|byte| byte.is_ascii_digit())
}

fn error_text(error: &JsValue) -> String {
    if let Ok(message) = Reflect::get(error, &JsValue::from_str("message"))
        && let Some(message) = message.as_string()
    {
        return message;
    }
    js_sys::JsString::from(error.clone())
        .as_string()
        .unwrap_or_else(|| format!("{error:?}"))
}
