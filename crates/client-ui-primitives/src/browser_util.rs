//! Browser utilities shared by compiled UI primitives.

use js_sys::{Array, Function, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, future_to_promise};

/// Writes exact text through the async Clipboard API or the textarea fallback.
#[wasm_bindgen(js_name = writeClipboard)]
pub fn write_clipboard(text: String) -> Promise {
    future_to_promise(async move { write_clipboard_inner(&text).await.map(JsValue::from_bool) })
}

async fn write_clipboard_inner(text: &str) -> Result<bool, JsValue> {
    let global = js_sys::global();
    let navigator = Reflect::get(&global, &JsValue::from_str("navigator"))?;
    let clipboard = Reflect::get(&navigator, &JsValue::from_str("clipboard"))?;
    if !clipboard.is_null() && !clipboard.is_undefined() {
        let write_text = Reflect::get(&clipboard, &JsValue::from_str("writeText"))?;
        if write_text.is_truthy() {
            let Ok(write_text) = write_text.dyn_into::<Function>() else {
                return Ok(false);
            };
            let Ok(pending) = write_text.call1(&clipboard, &JsValue::from_str(text)) else {
                return Ok(false);
            };
            return Ok(JsFuture::from(Promise::resolve(&pending)).await.is_ok());
        }
    }

    let document = required_property(&global, "document", "global")?;
    let exec = Reflect::get(&document, &JsValue::from_str("execCommand"))?;
    let Ok(exec) = exec.dyn_into::<Function>() else {
        return Ok(false);
    };
    let textarea = call_method(&document, "createElement", &[JsValue::from_str("textarea")])?;
    Reflect::set(
        &textarea,
        &JsValue::from_str("value"),
        &JsValue::from_str(text),
    )?;
    call_method(
        &textarea,
        "setAttribute",
        &[JsValue::from_str("readonly"), JsValue::from_str("")],
    )?;
    let style = required_property(&textarea, "style", "textarea")?;
    Reflect::set(
        &style,
        &JsValue::from_str("position"),
        &JsValue::from_str("fixed"),
    )?;
    Reflect::set(
        &style,
        &JsValue::from_str("left"),
        &JsValue::from_str("-9999px"),
    )?;
    let body = required_property(&document, "body", "document")?;
    call_method(&body, "appendChild", std::slice::from_ref(&textarea))?;
    call_method(&textarea, "select", &[])?;
    let accepted = exec.call1(&document, &JsValue::from_str("copy"));
    call_method(&textarea, "remove", &[])?;
    Ok(accepted
        .ok()
        .and_then(|value| value.as_bool())
        .unwrap_or(false))
}

fn required_property(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Err(js_sys::Error::new(&format!(
            "client-ui-primitives: {owner} omitted required property {key:?}"
        ))
        .into())
    } else {
        Ok(property)
    }
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().collect();
    method.apply(value, &arguments)
}
