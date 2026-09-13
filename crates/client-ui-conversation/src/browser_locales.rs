//! Embedded `conversation` locale dictionaries compiled from the pinned source.

use std::cell::RefCell;

use js_sys::{JSON, Reflect};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};

const LOCALES_JSON: &str = include_str!("../data/conversation-locales.json");

thread_local! {
    static LOCALES: RefCell<Option<JsValue>> = const { RefCell::new(None) };
}

/// Conversation locale namespace.
pub const CONVERSATION_LOCALE_NAMESPACE: &str = "conversation";

/// Returns the exact locale namespace.
#[must_use]
#[wasm_bindgen(js_name = conversationNamespace)]
pub fn conversation_namespace_browser() -> String {
    CONVERSATION_LOCALE_NAMESPACE.to_owned()
}

/// Returns the stable `{ zh, en }` dictionary object.
///
/// # Errors
///
/// Returns only if the embedded generated artifact is malformed.
#[wasm_bindgen(js_name = conversationLocales)]
pub fn conversation_locales_browser() -> Result<JsValue, JsValue> {
    LOCALES.with(|cached| {
        if let Some(value) = cached.borrow().as_ref() {
            return Ok(value.clone());
        }
        let value = JSON::parse(LOCALES_JSON)?;
        *cached.borrow_mut() = Some(value.clone());
        Ok(value)
    })
}

/// Returns the stable Simplified Chinese dictionary.
///
/// # Errors
///
/// Returns only if the embedded generated artifact is malformed.
#[wasm_bindgen(js_name = conversationZh)]
pub fn conversation_zh_browser() -> Result<JsValue, JsValue> {
    Reflect::get(&conversation_locales_browser()?, &JsValue::from_str("zh"))
}

/// Returns the stable English dictionary.
///
/// # Errors
///
/// Returns only if the embedded generated artifact is malformed.
#[wasm_bindgen(js_name = conversationEn)]
pub fn conversation_en_browser() -> Result<JsValue, JsValue> {
    Reflect::get(&conversation_locales_browser()?, &JsValue::from_str("en"))
}
