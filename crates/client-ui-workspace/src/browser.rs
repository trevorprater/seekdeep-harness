//! Shared browser bindings for Workspace surfaces.

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue};

pub(crate) const PACKAGE_ID: &str = "@seekdeep-ai/seekdeep-client-ui-workspace";

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

pub(crate) fn function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    required(value, key, owner)?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new(&format!("{owner} {key} must be a function")).into())
}

pub(crate) fn call(
    target: &JsValue,
    name: &str,
    arguments: &[JsValue],
) -> Result<JsValue, JsValue> {
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    function(target, name, "object")?.apply(target, &args)
}

pub(crate) fn element(
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
    function(react, "createElement", "React")?.apply(react, &args)
}

pub(crate) fn tag(
    react: &JsValue,
    name: &str,
    props: Option<&Object>,
    children: &[JsValue],
) -> Result<JsValue, JsValue> {
    element(react, &JsValue::from_str(name), props, children)
}

pub(crate) fn css(name: &str) -> String {
    format!("seekdeep-workspace-{name}")
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

pub(crate) fn inject_style(name: &str, source: &str) -> Result<(), JsValue> {
    let css = prefix_css_classes(source, "seekdeep-workspace-");
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
    let style = call(&document, "createElement", &[JsValue::from_str("style")])?;
    for (key, value) in [
        ("data-plugin-css", identity.as_str()),
        ("data-plugin", PACKAGE_ID),
    ] {
        call(
            &style,
            "setAttribute",
            &[JsValue::from_str(key), JsValue::from_str(value)],
        )?;
    }
    Reflect::set(
        &style,
        &JsValue::from_str("textContent"),
        &JsValue::from_str(&css),
    )?;
    call(
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
