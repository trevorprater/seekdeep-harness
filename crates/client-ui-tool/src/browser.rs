//! Shared browser bindings for the compiled Tool presentation package.

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::{JsCast as _, JsValue};

pub(crate) const PACKAGE_ID: &str = "@seekdeep-ai/seekdeep-client-ui-tool";

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

pub(crate) fn required_property(
    value: &JsValue,
    key: &str,
    owner: &str,
) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_undefined() {
        Err(js_sys::TypeError::new(&format!("{owner} is missing {key}")).into())
    } else {
        Ok(property)
    }
}

pub(crate) fn optional_property(value: &JsValue, key: &str) -> Result<Option<JsValue>, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    Ok((!property.is_null() && !property.is_undefined()).then_some(property))
}

pub(crate) fn required_function(
    value: &JsValue,
    key: &str,
    owner: &str,
) -> Result<Function, JsValue> {
    required_property(value, key, owner)?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::TypeError::new(&format!("{owner} {key} must be a function")).into())
}

pub(crate) fn required_string(value: &JsValue, key: &str, owner: &str) -> Result<String, JsValue> {
    required_property(value, key, owner)?
        .as_string()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a string")).into())
}

pub(crate) fn required_bool(value: &JsValue, key: &str, owner: &str) -> Result<bool, JsValue> {
    required_property(value, key, owner)?
        .as_bool()
        .ok_or_else(|| js_sys::TypeError::new(&format!("{owner} {key} must be a boolean")).into())
}

pub(crate) fn call_method(
    target: &JsValue,
    name: &str,
    arguments: &[JsValue],
) -> Result<JsValue, JsValue> {
    let function = required_function(target, name, "object")?;
    function.apply(target, &arguments.iter().cloned().collect::<Array>())
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

pub(crate) fn class_props(class_name: &str) -> Result<Object, JsValue> {
    object(&[("className", JsValue::from_str(class_name))])
}

pub(crate) fn extend_object(
    source: &JsValue,
    entries: &[(&str, JsValue)],
) -> Result<Object, JsValue> {
    let target = Object::new();
    let keys = Reflect::own_keys(source)?;
    for index in 0..keys.length() {
        let key = keys.get(index);
        Reflect::set(&target, &key, &Reflect::get(source, &key)?)?;
    }
    for (key, value) in entries {
        set(&target, key, value)?;
    }
    Ok(target)
}

pub(crate) fn translated(translate: &Function, key: &str) -> Result<JsValue, JsValue> {
    translate.call1(&JsValue::UNDEFINED, &JsValue::from_str(key))
}

pub(crate) fn translated_with(
    translate: &Function,
    key: &str,
    values: &Object,
) -> Result<JsValue, JsValue> {
    translate.call2(
        &JsValue::UNDEFINED,
        &JsValue::from_str(key),
        values.as_ref(),
    )
}

pub(crate) fn bool_or_undefined(value: bool) -> JsValue {
    if value {
        JsValue::TRUE
    } else {
        JsValue::UNDEFINED
    }
}

pub(crate) fn inject_style(
    name: &str,
    source: &str,
    replacements: &[(&str, &str)],
) -> Result<(), JsValue> {
    let document = Reflect::get(&js_sys::global(), &JsValue::from_str("document"))?;
    if document.is_null() || document.is_undefined() {
        return Ok(());
    }
    let tag = format!("{PACKAGE_ID}/{name}.module.css");
    if let Ok(query) = Reflect::get(&document, &JsValue::from_str("querySelector"))
        .and_then(wasm_bindgen::JsCast::dyn_into::<Function>)
        && !query
            .call1(
                &document,
                &JsValue::from_str(&format!("[data-plugin-css=\"{tag}\"]")),
            )?
            .is_null()
    {
        return Ok(());
    }
    let mut css = source.to_owned();
    for (source, target) in replacements {
        css = css.replace(&format!(".{source}"), &format!(".{target}"));
    }
    let style = call_method(&document, "createElement", &[JsValue::from_str("style")])?;
    for (attribute, value) in [
        ("data-plugin-css", tag.as_str()),
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
    let head = required_property(&document, "head", "document")?;
    call_method(&head, "appendChild", &[style])?;
    Ok(())
}
