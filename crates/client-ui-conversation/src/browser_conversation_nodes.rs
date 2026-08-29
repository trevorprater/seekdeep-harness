//! Browser registry installation for compiled Chat conversation definitions.

use js_sys::{Array, Function, Reflect};
use wasm_bindgen::{JsCast as _, JsValue};

use crate::{conversation_simple_definitions, conversation_unknown_fallback_definition};

/// Registers the self-contained compiled Chat definitions and append-surface fallback.
///
/// # Errors
///
/// Returns missing-service, native-definition conversion, or registry failures.
#[allow(clippy::needless_pass_by_value)]
pub fn register_conversation_simple_nodes_browser(context: JsValue) -> Result<(), JsValue> {
    let events = required(&context, "conversationEvents", "ui-conversation context")?;
    for definition in conversation_simple_definitions() {
        call_method(
            &events,
            "register",
            &[seekdeep_client_runtime::native_conversation_node_definition_to_js(definition)?],
        )?;
    }
    call_method(
        &events,
        "registerFallback",
        &[
            seekdeep_client_runtime::native_conversation_node_definition_to_js(
                conversation_unknown_fallback_definition(),
            )?,
        ],
    )?;
    Ok(())
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = required(value, name, "conversationEvents")?.dyn_into::<Function>()?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    method.apply(value, &args)
}

fn required(value: &JsValue, key: &str, owner: &str) -> Result<JsValue, JsValue> {
    let property = Reflect::get(value, &JsValue::from_str(key))?;
    if property.is_null() || property.is_undefined() {
        Err(js_sys::Error::new(&format!("{owner} omitted required property {key:?}")).into())
    } else {
        Ok(property)
    }
}
