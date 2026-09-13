//! Shared browser bindings for Agent preset stores.

use js_sys::{Array, Function, JSON, Object, Promise, Reflect};
use serde::{Serialize, de::DeserializeOwned};
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;

pub(crate) const PACKAGE_ID: &str = "@seekdeep-ai/seekdeep-client-ui-agent-preset";

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

pub(crate) fn create_element(
    react: &JsValue,
    kind: &JsValue,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    let args = Array::new();
    args.push(kind);
    args.push(props.map_or(&JsValue::NULL, Object::as_ref));
    for child in children {
        args.push(child);
    }
    required_function(react, "createElement", "React")?.apply(react, &args)
}

pub(crate) fn tag(
    react: &JsValue,
    name: &str,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    create_element(react, &JsValue::from_str(name), props, children)
}

pub(crate) fn translated(translate: &Function, key: &str) -> Result<JsValue, JsValue> {
    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
}

pub(crate) fn css(name: &str) -> String {
    format!("seekdeep-agent-preset-{name}")
}

pub(crate) fn class(name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(&css(name)))])
}

pub(crate) fn inject_prefixed_style(name: &str, source: &str) -> Result<(), JsValue> {
    let css = prefix_css_classes(source, "seekdeep-agent-preset-");
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let identity = format!("{PACKAGE_ID}/{name}.module.css");
    if let Ok(query) = Reflect::get(&document, &JsValue::from_str("querySelector"))
        .and_then(wasm_bindgen::JsCast::dyn_into::<Function>)
        && !query
            .call1(
                &document,
                &JsValue::from_str(&format!("[data-plugin-css=\"{identity}\"]")),
            )?
            .is_null()
    {
        return Ok(());
    }
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    for (attribute, value) in [
        ("data-plugin-css", identity.as_str()),
        ("data-plugin", PACKAGE_ID),
    ] {
        call_method(
            &style,
            "setAttribute",
            &[JsValue::from_str(attribute), JsValue::from_str(value)],
        )?;
    }
    Reflect::set(
        &style,
        &JsValue::from_str("textContent"),
        &JsValue::from_str(&css),
    )?;
    call_method(
        &required(&document, "head", "document")?,
        "appendChild",
        &[style],
    )?;
    Ok(())
}

fn prefix_css_classes(source: &str, prefix: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = Vec::with_capacity(source.len() + 256);
    let mut offset = 0;
    let mut quote = None;
    let mut comment = false;
    while offset < bytes.len() {
        if comment {
            output.push(bytes[offset]);
            if bytes[offset] == b'*' && bytes.get(offset + 1) == Some(&b'/') {
                output.push(b'/');
                offset += 2;
                comment = false;
            } else {
                offset += 1;
            }
            continue;
        }
        if let Some(delimiter) = quote {
            output.push(bytes[offset]);
            if bytes[offset] == b'\\' && offset + 1 < bytes.len() {
                output.push(bytes[offset + 1]);
                offset += 2;
            } else {
                if bytes[offset] == delimiter {
                    quote = None;
                }
                offset += 1;
            }
            continue;
        }
        if bytes[offset] == b'/' && bytes.get(offset + 1) == Some(&b'*') {
            output.extend_from_slice(b"/*");
            offset += 2;
            comment = true;
            continue;
        }
        if matches!(bytes[offset], b'\'' | b'"') {
            quote = Some(bytes[offset]);
            output.push(bytes[offset]);
            offset += 1;
            continue;
        }
        if bytes[offset] == b'.'
            && bytes
                .get(offset + 1)
                .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(*byte, b'_' | b'-'))
        {
            output.push(b'.');
            output.extend_from_slice(prefix.as_bytes());
            offset += 1;
            while offset < bytes.len()
                && (bytes[offset].is_ascii_alphanumeric() || matches!(bytes[offset], b'_' | b'-'))
            {
                output.push(bytes[offset]);
                offset += 1;
            }
            continue;
        }
        output.push(bytes[offset]);
        offset += 1;
    }
    String::from_utf8(output).expect("CSS prefixing preserves UTF-8")
}
