//! Structural JSON ⇄ JavaScript conversion that retains unchanged subtrees and appended text.
//!
//! A Definition or view builder compiled into another module exchanges JSON values with the
//! engine through JavaScript objects. Re-marshalling a whole snapshot through JSON text on every
//! streamed chunk costs the full text each time; the source keeps one growing JavaScript string
//! per streamed block. These converters walk the value structurally, hand back the previous
//! JavaScript subtree wherever the value is unchanged, and extend a retained string by its
//! appended suffix, so a streaming append costs the delta plus one structural comparison.
//!
//! A subtree with nothing to reuse crosses as JSON text: one call into the engine's native
//! `JSON` and one string copy replace a call per key and per string, which dominated a history
//! load (tens of thousands of fresh events, each walked key by key from every Definition
//! module). The structural walk is reserved for subtrees that can share their predecessor.

use std::cell::RefCell;

use js_sys::{Array, Function, JSON, JsString, Map as JsMap, Number, Object, Reflect, Symbol};
use serde_json::{Map, Value};
use wasm_bindgen::{JsCast as _, JsValue};

use crate::wasm_session::{js_to_json, json_to_js};

/// Strings at least this long ride as one retained JavaScript string that grows by its suffix.
const RETAINED_TEXT_BYTES: usize = 1024;

thread_local! {
    /// `JSON.stringify` replacer giving the deserializer's readings of the shapes JSON text
    /// would otherwise lose: an `undefined` member is `null`, a `Map` is its entries, and a
    /// safe bigint is a number.
    static ENGINE_REPLACER: RefCell<Option<Function>> = const { RefCell::new(None) };
}

fn engine_replacer() -> Function {
    ENGINE_REPLACER.with(|slot| {
        slot.borrow_mut()
            .get_or_insert_with(|| {
                Function::new_with_args(
                    "key, value",
                    "if (value === undefined) return null;\
                     if (value instanceof Map) return Object.fromEntries(value);\
                     if (typeof value === 'bigint') return Number(value);\
                     return value;",
                )
            })
            .clone()
    })
}

/// Parses a fresh JavaScript object graph through JSON text: one native `JSON.stringify`, one
/// string crossing, one `serde_json` parse.
fn fresh_js_to_value(value: &JsValue) -> Result<Value, JsValue> {
    let text = JSON::stringify_with_replacer(value, &engine_replacer())?;
    let Some(text) = text.as_string() else {
        return js_to_json(value);
    };
    serde_json::from_str(&text).map_err(|error| js_sys::Error::new(&error.to_string()).into())
}

/// Converts one JSON value into plain JavaScript data without JSON text.
///
/// Numbers become JavaScript numbers, objects keep their insertion order, and shapes JSON text
/// would encode differently (an object with a `__proto__` key, an out-of-range number) take the
/// JSON path so both converters agree.
///
/// # Errors
///
/// Returns the JavaScript error raised while building the object graph.
pub fn value_to_js(value: &Value) -> Result<JsValue, JsValue> {
    Ok(match value {
        Value::Null => JsValue::NULL,
        Value::Bool(flag) => JsValue::from_bool(*flag),
        Value::Number(number) => match number.as_f64() {
            Some(number) => JsValue::from_f64(number),
            None => return json_to_js(value),
        },
        Value::String(text) => JsValue::from_str(text),
        Value::Array(entries) if entries.is_empty() => Array::new().into(),
        Value::Object(map) if map.is_empty() => Object::new().into(),
        // A fresh graph crosses as one JSON text and one native parse.
        Value::Array(_) | Value::Object(_) => return json_to_js(value),
    })
}

/// Converts `next` into JavaScript, reusing `previous_js` (the face of `previous`) wherever the
/// value is unchanged and growing a retained string by its appended suffix.
///
/// # Errors
///
/// Returns the JavaScript error raised while building the object graph.
pub fn value_to_js_reusing(
    previous: &Value,
    previous_js: &JsValue,
    next: &Value,
) -> Result<JsValue, JsValue> {
    if previous == next {
        return Ok(previous_js.clone());
    }
    match (previous, next) {
        (Value::String(previous), Value::String(next))
            if previous.len() >= RETAINED_TEXT_BYTES
                && next.len() > previous.len()
                && next.starts_with(previous.as_str())
                && previous_js.is_string() =>
        {
            let retained: &JsString = previous_js.unchecked_ref();
            Ok(retained
                .concat(&JsValue::from_str(&next[previous.len()..]))
                .into())
        }
        (Value::Array(previous), Value::Array(next)) => {
            let previous_js = previous_js.dyn_ref::<Array>();
            let array = Array::new();
            for (index, entry) in next.iter().enumerate() {
                let converted = match (previous.get(index), previous_js) {
                    (Some(previous_entry), Some(previous_array)) => value_to_js_reusing(
                        previous_entry,
                        &previous_array.get(u32::try_from(index).unwrap_or(u32::MAX)),
                        entry,
                    )?,
                    _ => value_to_js(entry)?,
                };
                array.push(&converted);
            }
            Ok(array.into())
        }
        (Value::Object(previous), Value::Object(next)) if previous_js.is_object() => {
            if next.contains_key("__proto__") {
                return json_to_js(&Value::Object(next.clone()));
            }
            let object = Object::new();
            for (key, entry) in next {
                let key_js = JsValue::from_str(key);
                let converted = match previous.get(key) {
                    Some(previous_entry) => value_to_js_reusing(
                        previous_entry,
                        &Reflect::get(previous_js, &key_js)?,
                        entry,
                    )?,
                    None => value_to_js(entry)?,
                };
                Reflect::set(&object, &key_js, &converted)?;
            }
            Ok(object.into())
        }
        _ => value_to_js(next),
    }
}

/// Parses plain JavaScript data into JSON with the engine's deserializer semantics: safe
/// integers stay integral, other numbers are floats, nullish values are `null`, and
/// non-plain objects (a `Map`, an iterable) take the deserializer path.
///
/// # Errors
///
/// Returns the JavaScript error raised while reading the object graph, or the deserializer's
/// rejection of an unsupported value.
pub fn js_to_value(value: &JsValue) -> Result<Value, JsValue> {
    if value.is_null() || value.is_undefined() {
        return Ok(Value::Null);
    }
    if let Some(flag) = value.as_bool() {
        return Ok(Value::Bool(flag));
    }
    if let Some(number) = value.as_f64() {
        return Ok(number_value(value, number));
    }
    if let Some(text) = value.as_string() {
        return Ok(Value::String(text));
    }
    if value.is_object() {
        // Arrays, plain objects, and the deserializer's collection shapes (Map, iterables) all
        // take the JSON path; the replacer keeps its readings of `undefined` and `Map`.
        return fresh_js_to_value(value);
    }
    js_to_json(value)
}

/// Parses `next` into JSON, cloning `previous` wherever `next` is the same JavaScript value as
/// `previous_js` and extending a retained string by its appended suffix.
///
/// # Errors
///
/// Returns the JavaScript error raised while reading the object graph.
pub fn js_to_value_reusing(
    previous: &Value,
    previous_js: &JsValue,
    next: &JsValue,
) -> Result<Value, JsValue> {
    if previous_js == next {
        return Ok(previous.clone());
    }
    match previous {
        Value::String(previous_text) if next.is_string() => {
            let next_string: &JsString = next.unchecked_ref();
            if previous_text.len() >= RETAINED_TEXT_BYTES
                && let Some(retained) = previous_js.dyn_ref::<JsString>()
                && next_string.length() > retained.length()
                && JsValue::from(next_string.slice(0, retained.length())) == *previous_js
            {
                let suffix =
                    String::from(next_string.slice(retained.length(), next_string.length()));
                let mut text = String::with_capacity(previous_text.len() + suffix.len());
                text.push_str(previous_text);
                text.push_str(&suffix);
                return Ok(Value::String(text));
            }
            Ok(Value::String(next.as_string().unwrap_or_default()))
        }
        Value::Array(previous_entries) if next.is_array() => {
            let next: &Array = next.unchecked_ref();
            let previous_array = previous_js.dyn_ref::<Array>();
            let mut entries = Vec::with_capacity(next.length() as usize);
            for index in 0..next.length() {
                let entry = next.get(index);
                let converted = match (previous_entries.get(index as usize), previous_array) {
                    (Some(previous_entry), Some(previous_array)) => {
                        js_to_value_reusing(previous_entry, &previous_array.get(index), &entry)?
                    }
                    _ => js_to_value(&entry)?,
                };
                entries.push(converted);
            }
            Ok(Value::Array(entries))
        }
        Value::Object(previous_map) if is_plain_object(next) => {
            let mut map = Map::new();
            for entry in Object::entries(next.unchecked_ref()) {
                let pair: Array = entry.unchecked_into();
                let key_js = pair.get(0);
                let key = key_js
                    .as_string()
                    .ok_or_else(|| js_sys::Error::new("object keys must be strings"))?;
                let entry = pair.get(1);
                let converted = match previous_map.get(&key) {
                    Some(previous_entry) if previous_js.is_object() => js_to_value_reusing(
                        previous_entry,
                        &Reflect::get(previous_js, &key_js)?,
                        &entry,
                    )?,
                    _ => js_to_value(&entry)?,
                };
                map.insert(key, converted);
            }
            Ok(Value::Object(map))
        }
        _ => js_to_value(next),
    }
}

fn number_value(value: &JsValue, number: f64) -> Value {
    if Number::is_safe_integer(value) {
        // A safe integer is exactly representable in i64.
        #[allow(clippy::cast_possible_truncation)]
        return Value::from(number as i64);
    }
    serde_json::Number::from_f64(number).map_or(Value::Null, Value::Number)
}

fn is_plain_object(value: &JsValue) -> bool {
    value.is_object()
        && !value.has_type::<JsMap>()
        && !Array::is_array(value)
        && !Symbol::iterator().js_in(value)
}
