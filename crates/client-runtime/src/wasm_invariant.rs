//! Browser invariant companion for Slot mutation-before-emission ordering.

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};

const PACKAGE_NAME: &str = "@seekdeep-ai/seekdeep-client-runtime";

/// Registers the Client Runtime invariant companion.
///
/// # Errors
///
/// Returns missing invariant-service or Cordis listener faces.
#[wasm_bindgen(js_name = applyClientRuntimeInvariant)]
#[allow(clippy::needless_pass_by_value)]
pub fn apply_client_runtime_invariant(root: JsValue) -> Result<Promise, JsValue> {
    let invariants = context_service(&root, "invariants")?;
    let install = Closure::wrap(Box::new(
        move |context: JsValue, fail: Function| -> Result<Function, JsValue> {
            let listener_context = context.clone();
            let listener = Closure::wrap(Box::new(
                move |_mode: JsValue, event_name: String, args: JsValue| -> Result<(), JsValue> {
                    if event_name != "slots/changed" {
                        return Ok(());
                    }
                    let args = Array::from(&args);
                    let key = args.get(0).as_string();
                    let Some(key) = key.filter(|key| !key.is_empty()) else {
                        fail.call1(
                            &JsValue::UNDEFINED,
                            &JsValue::from_str(
                                "'slots/changed' dispatched without a slot key argument",
                            ),
                        )?;
                        return Ok(());
                    };
                    let Ok(slots) = optional_context_service(&listener_context, "slots") else {
                        return Ok(());
                    };
                    let Some(slots) = slots else {
                        return Ok(());
                    };
                    let version = call_method(&slots, "getVersion", &[JsValue::from_str(&key)])
                        .ok()
                        .and_then(|version| version.as_f64())
                        .unwrap_or(0.0);
                    if version == 0.0 {
                        fail.call1(
                            &JsValue::UNDEFINED,
                            &JsValue::from_str(&format!(
                                "'slots/changed' fired for \"{key}\" before any mutation bumped its version — emission must follow the applied mutation"
                            )),
                        )?;
                    }
                    Ok(())
                },
            )
                as Box<dyn FnMut(JsValue, String, JsValue) -> Result<(), JsValue>>);
            let options = Object::new();
            set(&options, "global", &JsValue::TRUE)?;
            call_method(
                &context,
                "on",
                &[
                    JsValue::from_str("internal/dispatch"),
                    listener.into_js_value(),
                    options.into(),
                ],
            )?
            .dyn_into::<Function>()
        },
    )
        as Box<dyn FnMut(JsValue, Function) -> Result<Function, JsValue>>);
    let registration = call_method(
        &invariants,
        "register",
        &[JsValue::from_str(PACKAGE_NAME), install.into_js_value()],
    )?;
    Ok(Promise::resolve(&registration))
}

fn context_service(root: &JsValue, name: &str) -> Result<JsValue, JsValue> {
    optional_context_service(root, name)?.ok_or_else(|| {
        js_sys::Error::new(&format!("Client Runtime invariant requires {name:?}")).into()
    })
}

fn optional_context_service(root: &JsValue, name: &str) -> Result<Option<JsValue>, JsValue> {
    let get = required_function(root, "get", "Client Context")?;
    let service = get.call1(root, &JsValue::from_str(name))?;
    Ok((!service.is_undefined() && !service.is_null()).then_some(service))
}

fn call_method(value: &JsValue, method: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = required_function(value, method, "Client Runtime invariant face")?;
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    function.apply(value, &args)
}

fn required_function(value: &JsValue, key: &str, owner: &str) -> Result<Function, JsValue> {
    Reflect::get(value, &JsValue::from_str(key))?
        .dyn_into::<Function>()
        .map_err(|_| js_sys::Error::new(&format!("{owner} requires {key:?}")).into())
}

fn set(object: &Object, key: &str, value: &JsValue) -> Result<(), JsValue> {
    if Reflect::set(object, &JsValue::from_str(key), value)? {
        Ok(())
    } else {
        Err(js_sys::Error::new(&format!("failed to set invariant member {key:?}")).into())
    }
}
