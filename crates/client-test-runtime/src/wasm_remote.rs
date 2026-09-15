//! Browser Cordis face for the event-only Remote test double.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use js_sys::{Array, Function, Object, Promise, Reflect};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};

type BrowserSubscriptions = Rc<RefCell<HashMap<String, Vec<Function>>>>;

/// Constructs and publishes the event-only double as `ctx.remote`.
///
/// # Errors
///
/// Returns malformed Context, argument-list, or synchronous listener failures.
#[wasm_bindgen(js_name = installTestRemote)]
#[allow(clippy::needless_pass_by_value)]
pub fn install_test_remote(context: JsValue) -> Result<JsValue, JsValue> {
    let subscriptions: BrowserSubscriptions = Rc::new(RefCell::new(HashMap::new()));
    let face = Object::new();

    let dispatch_subscriptions = subscriptions.clone();
    let dispatch = Closure::wrap(Box::new(
        move |event: String, arguments: JsValue| -> Result<(), JsValue> {
            if !Array::is_array(&arguments) {
                return Err(js_sys::TypeError::new(
                    "TestRemote: $dispatch arguments must be an array",
                )
                .into());
            }
            let arguments = Array::from(&arguments);
            let listeners = dispatch_subscriptions
                .borrow()
                .get(&event)
                .cloned()
                .unwrap_or_default();
            for listener in listeners {
                listener.apply(&JsValue::UNDEFINED, &arguments)?;
            }
            Ok(())
        },
    )
        as Box<dyn FnMut(String, JsValue) -> Result<(), JsValue>>);
    Reflect::set(
        &face,
        &JsValue::from_str("$dispatch"),
        &dispatch.into_js_value(),
    )?;

    let subscribe_subscriptions = subscriptions;
    let subscribe = Closure::wrap(
        Box::new(move |event: String, listener: Function| -> Function {
            let mut subscriptions = subscribe_subscriptions.borrow_mut();
            let listeners = subscriptions.entry(event.clone()).or_default();
            if !listeners
                .iter()
                .any(|registered| Object::is(registered, &listener))
            {
                listeners.push(listener.clone());
            }
            drop(subscriptions);
            let cleanup_subscriptions = subscribe_subscriptions.clone();
            Closure::wrap(Box::new(move || {
                let mut subscriptions = cleanup_subscriptions.borrow_mut();
                let Some(listeners) = subscriptions.get_mut(&event) else {
                    return;
                };
                listeners.retain(|registered| !Object::is(registered, &listener));
                if listeners.is_empty() {
                    subscriptions.remove(&event);
                }
            }) as Box<dyn FnMut()>)
            .into_js_value()
            .unchecked_into()
        }) as Box<dyn FnMut(String, Function) -> Function>,
    );
    Reflect::set(&face, &JsValue::from_str("$on"), &subscribe.into_js_value())?;

    let mount = Closure::wrap(Box::new(move || -> Promise {
        Promise::reject(&js_sys::Error::new(
            "TestRemote: $mount needs the real Client Remote service",
        ))
    }) as Box<dyn FnMut() -> Promise>);
    Reflect::set(&face, &JsValue::from_str("$mount"), &mount.into_js_value())?;
    call_method(
        &context,
        "provide",
        &[JsValue::from_str("remote"), face.clone().into()],
    )?;
    Ok(face.into())
}

fn call_method(value: &JsValue, name: &str, arguments: &[JsValue]) -> Result<JsValue, JsValue> {
    let function = Reflect::get(value, &JsValue::from_str(name))?.dyn_into::<Function>()?;
    let arguments: Array = arguments.iter().cloned().collect();
    function.apply(value, &arguments)
}
