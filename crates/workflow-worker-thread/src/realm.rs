//! Materializes values leaving the script realm into plain JSON before they
//! cross the worker boundary, and renders thrown script values without
//! rejecting the run. Property accessors run normally under the same trust
//! premise as the source.

use std::collections::HashSet;

use boa_engine::{Context, JsObject, JsValue, js_string, property::PropertyKey, value::JsVariant};
use serde_json::{Map, Number, Value};
use thiserror::Error;

/// Thrown by materialization; the caller wraps it into the right error code.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("{path}: {reason}")]
pub struct MaterializeError {
    /// The offending path.
    pub path: String,
    /// Why the value was rejected.
    pub reason: String,
}

/// Render a thrown value to failure text without ever throwing: prefer the
/// stack, fall back to message, then the string coercion.
#[must_use]
pub fn render_thrown(value: &JsValue, context: &mut Context) -> String {
    let attempt = (|| -> Result<String, ()> {
        if let Some(object) = value.as_object() {
            if let Some(stack) = string_field(&object, "stack", context)? {
                return Ok(stack);
            }
            if let Some(message) = string_field(&object, "message", context)? {
                return Ok(message);
            }
        }
        value
            .to_string(context)
            .map(|text| text.to_std_string_escaped())
            .map_err(|_| ())
    })();
    attempt.unwrap_or_else(|()| "[unrenderable thrown value]".to_owned())
}

fn string_field(object: &JsObject, key: &str, context: &mut Context) -> Result<Option<String>, ()> {
    let value = object
        .get(PropertyKey::from(js_string!(key)), context)
        .map_err(|_| ())?;
    Ok(value
        .as_string()
        .map(|text| text.to_std_string_escaped())
        .filter(|text| !text.is_empty()))
}

/// Copy a realm value into plain host JSON data. Root `undefined` is returned
/// unchanged as `None`; a nested undefined is rejected.
///
/// # Errors
///
/// Returns a `MaterializeError` for unsupported values, cycles, sparse
/// arrays, exotic prototypes, or property reads that throw.
pub fn materialize_from_realm(
    value: &JsValue,
    context: &mut Context,
    root: &str,
) -> Result<Option<Value>, MaterializeError> {
    if value.is_undefined() {
        return Ok(None);
    }
    materialize(value, context, root, &mut HashSet::new()).map(Some)
}

fn materialize(
    value: &JsValue,
    context: &mut Context,
    path: &str,
    seen: &mut HashSet<JsObject>,
) -> Result<Value, MaterializeError> {
    match value.variant() {
        JsVariant::Null => Ok(Value::Null),
        JsVariant::Boolean(value) => Ok(Value::Bool(value)),
        JsVariant::String(value) => Ok(Value::String(value.to_std_string_escaped())),
        JsVariant::Integer32(value) => Ok(Value::Number(Number::from(value))),
        JsVariant::Float64(value) => {
            if value.is_finite() && !(value == 0.0 && value.is_sign_negative()) {
                Number::from_f64(value)
                    .map_or_else(|| Err(non_finite(path)), |number| Ok(Value::Number(number)))
            } else {
                Err(non_finite(path))
            }
        }
        JsVariant::BigInt(_) => Err(MaterializeError {
            path: path.to_owned(),
            reason: "bigints are not JSON data".to_owned(),
        }),
        JsVariant::Symbol(_) => Err(MaterializeError {
            path: path.to_owned(),
            reason: "symbols are not plain JSON data".to_owned(),
        }),
        JsVariant::Undefined => Err(MaterializeError {
            path: path.to_owned(),
            reason: "undefined is not JSON data".to_owned(),
        }),
        JsVariant::Object(object) => {
            if object.is_callable() {
                return Err(MaterializeError {
                    path: path.to_owned(),
                    reason: "functions are not plain JSON data".to_owned(),
                });
            }
            if !seen.insert(object.clone()) {
                return Err(MaterializeError {
                    path: path.to_owned(),
                    reason: "circular references are not JSON data".to_owned(),
                });
            }
            let result = if object.is_array() {
                materialize_array(&object, context, path, seen)
            } else {
                materialize_object(&object, context, path, seen)
            };
            seen.remove(&object);
            result
        }
    }
}

fn non_finite(path: &str) -> MaterializeError {
    MaterializeError {
        path: path.to_owned(),
        reason: "non-finite numbers are not JSON data".to_owned(),
    }
}

fn materialize_array(
    array: &JsObject,
    context: &mut Context,
    path: &str,
    seen: &mut HashSet<JsObject>,
) -> Result<Value, MaterializeError> {
    let length_key = PropertyKey::from(js_string!("length"));
    let length = {
        let object = array.borrow();
        let descriptor = object
            .properties()
            .get(&length_key)
            .ok_or_else(|| MaterializeError {
                path: path.to_owned(),
                reason: "arrays are not JSON data".to_owned(),
            })?;
        descriptor
            .value()
            .and_then(JsValue::as_number)
            .ok_or_else(|| MaterializeError {
                path: path.to_owned(),
                reason: "arrays are not JSON data".to_owned(),
            })?
    };
    if !length.is_finite() || length < 0.0 || length.fract() != 0.0 {
        return Err(MaterializeError {
            path: path.to_owned(),
            reason: "arrays are not JSON data".to_owned(),
        });
    }
    let length = ryu_js::Buffer::new()
        .format(length)
        .parse::<usize>()
        .map_err(|_| MaterializeError {
            path: path.to_owned(),
            reason: "arrays are not JSON data".to_owned(),
        })?;
    let mut out = Vec::with_capacity(length);
    for index in 0..length {
        let key = PropertyKey::from(u32::try_from(index).map_err(|_| MaterializeError {
            path: path.to_owned(),
            reason: "arrays are not JSON data".to_owned(),
        })?);
        let child_path = format!("{path}[{index}]");
        if array.borrow().properties().get(&key).is_none() {
            return Err(MaterializeError {
                path: child_path,
                reason: "sparse arrays are not JSON data".to_owned(),
            });
        }
        let child = array.get(key, context).map_err(|error| MaterializeError {
            path: child_path.clone(),
            reason: format!("reading the value threw: {error}"),
        })?;
        out.push(materialize(&child, context, &child_path, seen)?);
    }
    for key in array
        .own_property_keys(context)
        .map_err(|error| MaterializeError {
            path: path.to_owned(),
            reason: format!("reading the value threw: {error}"),
        })?
    {
        match &key {
            PropertyKey::Index(index) if (index.get() as usize) < length => {}
            PropertyKey::String(text) if text.to_std_string_escaped() == "length" => {}
            PropertyKey::String(text) => {
                return Err(MaterializeError {
                    path: format!("{path}.{}", text.to_std_string_escaped()),
                    reason: "arrays with non-index properties are not JSON data".to_owned(),
                });
            }
            PropertyKey::Index(_) => {
                return Err(MaterializeError {
                    path: path.to_owned(),
                    reason: "arrays with non-index properties are not JSON data".to_owned(),
                });
            }
            PropertyKey::Symbol(_) => {
                return Err(MaterializeError {
                    path: path.to_owned(),
                    reason: "symbol-keyed properties are not plain JSON data".to_owned(),
                });
            }
        }
    }
    Ok(Value::Array(out))
}

fn materialize_object(
    object: &JsObject,
    context: &mut Context,
    path: &str,
    seen: &mut HashSet<JsObject>,
) -> Result<Value, MaterializeError> {
    if !has_plain_prototype(object) {
        return Err(MaterializeError {
            path: path.to_owned(),
            reason: "only plain objects and arrays are JSON data (exotic prototype)".to_owned(),
        });
    }
    let keys = object
        .own_property_keys(context)
        .map_err(|error| MaterializeError {
            path: path.to_owned(),
            reason: format!("reading the value threw: {error}"),
        })?;
    let mut out = Map::with_capacity(keys.len());
    for key in keys {
        let PropertyKey::String(text) = &key else {
            return Err(MaterializeError {
                path: path.to_owned(),
                reason: "symbol-keyed properties are not plain JSON data".to_owned(),
            });
        };
        // Object.keys (and therefore JSON.stringify) selects only own enumerable
        // string keys; non-enumerable own properties never reach JSON output.
        if object
            .borrow()
            .properties()
            .get(&key)
            .and_then(|descriptor| descriptor.enumerable())
            != Some(true)
        {
            continue;
        }
        let key_string = text.to_std_string_escaped();
        let child_path = format!("{path}.{key_string}");
        let child = object.get(key, context).map_err(|error| MaterializeError {
            path: child_path.clone(),
            reason: format!("reading the value threw: {error}"),
        })?;
        out.insert(key_string, materialize(&child, context, &child_path, seen)?);
    }
    Ok(Value::Object(out))
}

/// Whether an object's prototype chain represents a plain data object: null,
/// or a prototype whose own prototype is null.
fn has_plain_prototype(object: &JsObject) -> bool {
    let Some(prototype) = object.prototype() else {
        return true;
    };
    prototype.prototype().is_none()
}

#[cfg(test)]
mod tests {
    use boa_engine::{Source, context::ContextBuilder};
    use serde_json::json;

    use super::*;

    fn evaluated(source: &str) -> (Context, JsValue) {
        let mut context = ContextBuilder::new().build().unwrap();
        let value = context
            .eval(Source::from_bytes(&format!("({source})")))
            .unwrap();
        (context, value)
    }

    fn materialize(source: &str) -> Value {
        let (mut context, value) = evaluated(source);
        materialize_from_realm(&value, &mut context, "value")
            .unwrap()
            .unwrap()
    }

    fn rejection(source: &str) -> String {
        let (mut context, value) = evaluated(source);
        materialize_from_realm(&value, &mut context, "value")
            .expect_err("expected rejection")
            .to_string()
    }

    #[test]
    fn copies_realm_data_deeply() {
        assert_eq!(
            materialize("{ a: 1, b: 'x', c: true, d: null, list: [1, [2, { deep: 'y' }]] }"),
            json!({ "a": 1, "b": "x", "c": true, "d": null, "list": [1, [2, { "deep": "y" }]] })
        );
    }

    #[test]
    fn accepts_undefined_only_at_the_root() {
        let (mut context, value) = evaluated("undefined");
        assert_eq!(
            materialize_from_realm(&value, &mut context, "value").unwrap(),
            None
        );
        assert!(rejection("{ a: undefined }").contains("value.a"));
    }

    #[test]
    fn rejects_non_json_values_with_paths() {
        assert!(rejection("{ fn: () => 1 }").contains("value.fn"));
        assert!(rejection("{ s: Symbol('v') }").contains("value.s"));
        assert!(rejection("{ big: 1n }").contains("value.big"));
        assert!(rejection("{ n: NaN }").contains("non-finite"));
        assert!(rejection("[Infinity]").contains("non-finite"));
    }

    #[test]
    fn rejects_exotic_prototypes_but_accepts_null_prototype() {
        assert!(rejection("{ d: new Date(0) }").contains("exotic prototype"));
        assert!(rejection("new Map()").contains("exotic prototype"));
        assert_eq!(
            materialize("Object.assign(Object.create(null), { a: 1 })"),
            json!({ "a": 1 })
        );
    }

    #[test]
    fn rejects_cycles_and_accepts_dags() {
        assert!(rejection("(() => { const o = {}; o.self = o; return o })()").contains("circular"));
        assert_eq!(
            materialize("(() => { const leaf = { v: 1 }; return { a: leaf, b: leaf } })()"),
            json!({ "a": { "v": 1 }, "b": { "v": 1 } })
        );
    }

    #[test]
    fn rejects_sparse_and_decorated_arrays() {
        assert!(rejection("[1, , 3]").contains("sparse"));
        assert!(
            rejection("(() => { const a = [1]; a.total = 3; return a })()").contains("non-index")
        );
    }

    #[test]
    fn skips_non_enumerable_properties() {
        assert_eq!(
            materialize(
                "(() => { const o = { visible: 1 }; Object.defineProperty(o, 'hidden', { value: () => 1, enumerable: false }); return o })()"
            ),
            json!({ "visible": 1 })
        );
    }

    #[test]
    fn render_thrown_prefers_message_then_string() {
        let (mut context, value) = evaluated("new Error('host failure')");
        assert!(render_thrown(&value, &mut context).contains("host failure"));
        let (mut context, value) = evaluated("{ code: 42 }");
        assert_eq!(render_thrown(&value, &mut context), "[object Object]");
        let (mut context, value) = evaluated("'plain'");
        assert_eq!(render_thrown(&value, &mut context), "plain");
        let (mut context, value) = evaluated("null");
        assert_eq!(render_thrown(&value, &mut context), "null");
    }
}
