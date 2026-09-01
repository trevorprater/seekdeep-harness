//! Shared browser bindings for the plugin Settings package.

use js_sys::{Array, Function, JSON, Object, Promise, Reflect};
use serde::Serialize;
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;

pub(crate) const PACKAGE_ID: &str = "@seekdeep-ai/seekdeep-client-ui-settings-plugins";

pub(crate) fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let value = Object::new();
    for (key, entry) in entries {
        set(&value, key, entry)?;
    }
    Ok(value)
}

pub(crate) fn set(target: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    Reflect::set(target, &JsValue::from_str(key), value).map(|_| ())
}

pub(crate) fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() {
        Err(js_sys::TypeError::new(&format!("{owner} is missing {key}")).into())
    } else {
        Ok(property)
    }
}

pub(crate) fn optional(value: &JsValue, key: &str) -> Result<Option<JsValue>, JsValue> {
    if value.is_null() || value.is_undefined() {
        return Ok(None);
    }
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
    required_function(target, name, "object")?
        .apply(target, &arguments.iter().cloned().collect::<Array>())
}

pub(crate) async fn call_async(
    target: &JsValue,
    name: &str,
    arguments: &[JsValue],
) -> Result<JsValue, JsValue> {
    let returned = call_method(target, name, arguments)?;
    JsFuture::from(Promise::resolve(&returned)).await
}

pub(crate) fn create_element(
    react: &JsValue,
    kind: &JsValue,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    let arguments = Array::new();
    arguments.push(kind);
    arguments.push(props.map_or(&JsValue::NULL, Object::as_ref));
    for child in children {
        arguments.push(child);
    }
    required_function(react, "createElement", "React")?.apply(react, &arguments)
}

pub(crate) fn tag(
    react: &JsValue,
    name: &str,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    create_element(react, &JsValue::from_str(name), props, children)
}

pub(crate) fn class(name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(&css(name)))])
}

pub(crate) fn translated(translate: &Function, key: &str) -> Result<JsValue, JsValue> {
    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
}

pub(crate) fn event_value(event: &JsValue) -> Result<String, JsValue> {
    required(
        &required(event, "target", "input event")?,
        "value",
        "input target",
    )?
    .as_string()
    .ok_or_else(|| js_sys::TypeError::new("input target value must be a string").into())
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

pub(crate) fn to_js(value: &impl Serialize) -> Result<JsValue, JsValue> {
    JSON::parse(
        &serde_json::to_string(value)
            .map_err(|error| js_sys::TypeError::new(&error.to_string()))?,
    )
}

pub(crate) fn css(name: &str) -> String {
    format!("seekdeep-settings-plugins-{name}")
}

pub(crate) fn inject_prefixed_style(name: &str, source: &str) -> Result<(), JsValue> {
    let source = source.replace("  composes: input;\n", "");
    let css = prefix_css_classes(&source, "seekdeep-settings-plugins-");
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
