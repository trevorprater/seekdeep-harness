//! Compiled declaration of the per-session conversation chat store.

use std::cell::RefCell;

use js_sys::{Function, Object, Reflect};
use wasm_bindgen::{JsValue, closure::Closure, prelude::wasm_bindgen};

const PERSISTENCE_KEY: &str = "dsh.conversation.chat";

thread_local! {
    static DEFINE_STORE: RefCell<Option<Function>> = const { RefCell::new(None) };
}

/// Configures the runtime `defineStore` face used by `createChatStore`.
#[wasm_bindgen(js_name = configureClientUiConversationChatStore)]
pub fn configure_client_ui_conversation_chat_store(define_store: Function) {
    DEFINE_STORE.with(|configured| *configured.borrow_mut() = Some(define_store));
}

/// Declares a fresh per-session chat store handle.
///
/// # Errors
///
/// Returns before configuration or when `defineStore` rejects the declaration.
#[wasm_bindgen(js_name = createChatStore)]
pub fn create_chat_store_browser() -> Result<JsValue, JsValue> {
    let init = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        Ok(object(&[
            ("selection", JsValue::NULL),
            ("draft", JsValue::from_str("")),
            ("view", JsValue::NULL),
            ("inspect", JsValue::NULL),
        ])?
        .into())
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>)
    .into_js_value();
    let actions = object(&[
        ("select", field_action("selection")),
        ("setDraft", field_action("draft")),
        ("setView", field_action("view")),
        ("setInspect", field_action("inspect")),
    ])?;
    let declaration = object(&[
        ("init", init),
        ("persist", JsValue::from_str(PERSISTENCE_KEY)),
        ("actions", actions.into()),
    ])?;
    configured_define_store()?.call1(&JsValue::UNDEFINED, declaration.as_ref())
}

fn field_action(field: &'static str) -> JsValue {
    Closure::wrap(Box::new(
        move |draft: JsValue, value: JsValue| -> Result<(), JsValue> {
            if Reflect::set(&draft, &JsValue::from_str(field), &value)? {
                Ok(())
            } else {
                Err(js_sys::TypeError::new(&format!("chat store draft rejected {field}")).into())
            }
        },
    )
        as Box<dyn FnMut(JsValue, JsValue) -> Result<(), JsValue>>)
    .into_js_value()
}

fn configured_define_store() -> Result<Function, JsValue> {
    DEFINE_STORE.with(|configured| {
        configured.borrow().clone().ok_or_else(|| {
            js_sys::Error::new("client-ui-conversation chat store was not configured").into()
        })
    })
}

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}
