//! Shared browser bindings for Agent preset stores.

use js_sys::{Array, Function, JSON, Object, Promise, Reflect};
use serde::{Serialize, de::DeserializeOwned};
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;

pub(crate) fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        Reflect::set(&value, &JsValue::from_str(key), entry)?;
    }
    Ok(value)
}

pub(crate) fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Err(js_sys::TypeError::new(&format!("{owner} is missing {key}")).into())
    } else {
        Ok(property)
    }
}

pub(crate) fn optional(value: &JsValue, key: &str) -> Result<Option<JsValue>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    Ok((!property.is_null() && !property.is_undefined()).then_some(property))
}

pub(crate) fn required_function(
    value: &JsValue,
    key: &str,
    owner: &str,
) -> Result<Function, JsValue> {
    required(value, key, owner)?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new(&format!("{owner} {key} must be a function")).into())
}

pub(crate) fn call_method(
    target: &JsValue,
    name: &str,
    arguments: &[JsValue],
) -> Result<JsValue, JsValue> {
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    required_function(target, name, "object")?.apply(target, &args)
}

pub(crate) async fn call_async(
    target: &JsValue,
    name: &str,
    arguments: &[JsValue],
) -> Result<JsValue, JsValue> {
    JsFuture::from(Promise::resolve(&call_method(target, name, arguments)?)).await
}

pub(crate) fn rejection_text(reason: &JsValue) -> String {
    if reason.is_instance_of::<js_sys::Error>() {
        return Reflect::get(reason, &JsValue::from_str("message"))
            .ok()
            .and_then(|message| message.as_string())
            .unwrap_or_default();
    }
    Reflect::get(&js_sys::global(), &JsValue::from_str("String"))
        .ok()
        .and_then(|string| string.dyn_into::<Function>().ok())
        .and_then(|string| string.call1(&JsValue::UNDEFINED, reason).ok())
        .and_then(|value| value.as_string())
        .unwrap_or_default()
}

pub(crate) fn rpc_value(response: &JsValue) -> Result<JsValue, String> {
    let result =
        required(response, "result", "RPC response").map_err(|error| rejection_text(&error))?;
    if required(&result, "ok", "RPC result")
        .map_err(|error| rejection_text(&error))?
        .as_bool()
        == Some(true)
    {
        required(&result, "value", "RPC result").map_err(|error| rejection_text(&error))
    } else {
        let error =
            required(&result, "error", "RPC result").map_err(|error| rejection_text(&error))?;
        Err(required(&error, "message", "RPC error")
            .map_err(|error| rejection_text(&error))?
            .as_string()
            .unwrap_or_default())
    }
}

pub(crate) fn from_js<T: DeserializeOwned>(value: JsValue) -> Result<T, String> {
    serde_wasm_bindgen::from_value(value).map_err(|error| error.to_string())
}

pub(crate) fn to_js(value: &impl Serialize) -> Result<JsValue, JsValue> {
    JSON::parse(
        &serde_json::to_string(value)
            .map_err(|error| js_sys::TypeError::new(&error.to_string()))?,
    )
}
