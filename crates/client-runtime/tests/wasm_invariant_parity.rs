//! Live browser Slot mutation-before-emission invariant parity.

#![cfg(target_arch = "wasm32")]

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_runtime::apply_client_runtime_invariant;
use wasm_bindgen::{JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::*;

fn set(object: &Object, key: &str, value: &JsValue) {
    assert!(Reflect::set(object, &JsValue::from_str(key), value).unwrap());
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn invariant_accepts_foreign_and_committed_events_but_rejects_invalid_or_early_slots() {
    let services = Object::new();
    let root = Object::new();
    let services_for_get = services.clone();
    let get = Closure::wrap(Box::new(move |name: String| {
        Reflect::get(&services_for_get, &JsValue::from_str(&name)).unwrap()
    }) as Box<dyn FnMut(String) -> JsValue>);
    set(&root, "get", &get.into_js_value());
    let listener = Rc::new(RefCell::new(None::<Function>));
    let observed_listener = listener.clone();
    let on = Closure::wrap(Box::new(
        move |_event: String, installed: Function, options: JsValue| -> Function {
            assert_eq!(
                Reflect::get(&options, &JsValue::from_str("global"))
                    .unwrap()
                    .as_bool(),
                Some(true)
            );
            *observed_listener.borrow_mut() = Some(installed);
            Function::new_no_args("")
        },
    )
        as Box<dyn FnMut(String, Function, JsValue) -> Function>);
    set(&root, "on", &on.into_js_value());
    let version = Rc::new(RefCell::new(0.0));
    let observed_version = version.clone();
    let get_version =
        Closure::wrap(Box::new(move |_key: String| *observed_version.borrow())
            as Box<dyn FnMut(String) -> f64>);
    let slots = Object::new();
    set(&slots, "getVersion", &get_version.into_js_value());
    set(&services, "slots", &slots);

    let root_for_register: JsValue = root.clone().into();
    let register = Closure::wrap(Box::new(move |_package: String, install: Function| {
        let fail = Closure::wrap(Box::new(move |message: String| -> Result<(), JsValue> {
            Err(js_sys::Error::new(&message).into())
        }) as Box<dyn FnMut(String) -> Result<(), JsValue>>);
        install
            .call2(
                &JsValue::UNDEFINED,
                &root_for_register,
                &fail.into_js_value(),
            )
            .unwrap()
    }) as Box<dyn FnMut(String, Function) -> JsValue>);
    let invariants = Object::new();
    set(&invariants, "register", &register.into_js_value());
    set(&services, "invariants", &invariants);
    let registration = apply_client_runtime_invariant(root.clone().into()).unwrap();
    JsFuture::from(registration).await.unwrap();
    let listener = listener.borrow().as_ref().unwrap().clone();

    let unrelated = Array::new();
    unrelated.push(&JsValue::from_str("x"));
    listener
        .call3(
            &JsValue::UNDEFINED,
            &JsValue::from_str("emit"),
            &JsValue::from_str("unrelated/event"),
            &unrelated,
        )
        .unwrap();

    let missing = Array::new();
    let error = listener
        .call3(
            &JsValue::UNDEFINED,
            &JsValue::from_str("emit"),
            &JsValue::from_str("slots/changed"),
            &missing,
        )
        .unwrap_err();
    assert!(
        error
            .as_string()
            .or_else(|| Reflect::get(&error, &JsValue::from_str("message"))
                .ok()?
                .as_string())
            .is_some_and(|message| message.contains("without a slot key"))
    );

    let key = Array::new();
    key.push(&JsValue::from_str("root"));
    let error = listener
        .call3(
            &JsValue::UNDEFINED,
            &JsValue::from_str("emit"),
            &JsValue::from_str("slots/changed"),
            &key,
        )
        .unwrap_err();
    assert!(
        Reflect::get(&error, &JsValue::from_str("message"))
            .unwrap()
            .as_string()
            .is_some_and(|message| message.contains("before any mutation"))
    );
    *version.borrow_mut() = 1.0;
    listener
        .call3(
            &JsValue::UNDEFINED,
            &JsValue::from_str("emit"),
            &JsValue::from_str("slots/changed"),
            &key,
        )
        .unwrap();

    assert!(Reflect::delete_property(&services, &JsValue::from_str("slots")).unwrap());
    listener
        .call3(
            &JsValue::UNDEFINED,
            &JsValue::from_str("emit"),
            &JsValue::from_str("slots/changed"),
            &key,
        )
        .unwrap();
}
