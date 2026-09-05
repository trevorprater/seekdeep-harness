//! Live JavaScript Session provide-channel shape, identity, and rollback parity.

#![cfg(target_arch = "wasm32")]

use std::{cell::RefCell, rc::Rc};

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_runtime::WasmSessionProvideChannel;
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_test::*;

fn set(object: &Object, key: &str, value: &JsValue) {
    assert!(Reflect::set(object, &JsValue::from_str(key), value).unwrap());
}

fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn absent_info() -> JsValue {
    let hooks = Object::new();
    set(&hooks, "session", &JsValue::UNDEFINED);
    let value = Object::new();
    set(&value, "sessionId", &JsValue::UNDEFINED);
    set(&value, "hooks", &hooks);
    set(&value, "props", &Object::new());
    value.into()
}

fn binding() -> JsValue {
    let projections = Object::new();
    let session = Object::new();
    set(&session, "projections", &projections);
    let binding = Object::new();
    set(&binding, "sessionId", &JsValue::from_str("s1"));
    set(&binding, "session", &session);
    set(&binding, "ctx", &Object::new());
    binding.into()
}

fn descriptor(source: JsValue, marker: JsValue) -> JsValue {
    let value = Object::new();
    let hooks = Array::new();
    hooks.push(&JsValue::from_str("extra"));
    let props = Array::new();
    props.push(&JsValue::from_str("marker"));
    set(&value, "hooks", &hooks);
    set(&value, "props", &props);
    let resolve = Closure::wrap(Box::new(move |_binding: JsValue| {
        let hooks = Object::new();
        set(&hooks, "extra", &source);
        let props = Object::new();
        set(&props, "marker", &marker);
        let contribution = Object::new();
        set(&contribution, "hooks", &hooks);
        set(&contribution, "props", &props);
        contribution
    }) as Box<dyn FnMut(JsValue) -> Object>);
    set(&value, "resolve", &resolve.into_js_value());
    value.into()
}

fn invalid_descriptor() -> JsValue {
    let value = Object::new();
    let hooks = Array::new();
    hooks.push(&JsValue::from_str("extra"));
    set(&value, "hooks", &hooks);
    let resolve = Closure::wrap(Box::new(move |_binding: JsValue| {
        let hooks = Object::new();
        set(&hooks, "other", &Object::new());
        let contribution = Object::new();
        set(&contribution, "hooks", &hooks);
        contribution
    }) as Box<dyn FnMut(JsValue) -> Object>);
    set(&value, "resolve", &resolve.into_js_value());
    value.into()
}

#[wasm_bindgen_test]
fn browser_channel_preserves_rosters_bundle_identity_publication_and_rollback() {
    let current = Rc::new(RefCell::new(absent_info()));
    let channel_slot = Rc::new(RefCell::new(None::<Rc<WasmSessionProvideChannel>>));
    let binding = binding();
    let host = Object::new();
    let current_for_rebuild = current.clone();
    let channel_for_rebuild = channel_slot.clone();
    let binding_for_rebuild = binding.clone();
    let rebuild = Closure::wrap(Box::new(move || -> Result<JsValue, JsValue> {
        if let Some(channel) = channel_for_rebuild.borrow().as_ref() {
            *current_for_rebuild.borrow_mut() =
                channel.materialize_info(binding_for_rebuild.clone())?;
        }
        Ok(JsValue::UNDEFINED)
    }) as Box<dyn FnMut() -> Result<JsValue, JsValue>>);
    set(&host, "rebuildBundles", &rebuild.into_js_value());
    let current_for_resolve = current.clone();
    let resolve = Closure::wrap(
        Box::new(move || current_for_resolve.borrow().clone()) as Box<dyn FnMut() -> JsValue>
    );
    set(&host, "resolveCurrent", &resolve.into_js_value());

    let channel = Rc::new(WasmSessionProvideChannel::new(host.into()).unwrap());
    *channel_slot.borrow_mut() = Some(channel.clone());
    let absent = channel.maybe_info();
    *current.borrow_mut() = absent.clone();
    assert!(get(&absent, "sessionId").is_undefined());
    assert!(Reflect::has(&get(&absent, "hooks"), &JsValue::from_str("session")).unwrap());

    let definite = channel.materialize_info(binding).unwrap();
    assert_eq!(
        get(&definite, "sessionId").as_string().as_deref(),
        Some("s1")
    );
    assert!(get(&get(&definite, "hooks"), "session").is_object());
    *current.borrow_mut() = definite;

    let ticks = Rc::new(std::cell::Cell::new(0));
    let observed = ticks.clone();
    let listener =
        Closure::wrap(Box::new(move || observed.set(observed.get() + 1)) as Box<dyn FnMut()>);
    let current_face = channel.current_provide_info();
    let subscribe = get(&current_face, "subscribe")
        .dyn_into::<Function>()
        .unwrap();
    let _off = subscribe
        .call1(&current_face, &listener.into_js_value())
        .unwrap();
    channel.publish_current().unwrap();
    assert_eq!(ticks.get(), 1);
    let first = get(&current_face, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&current_face)
        .unwrap();
    channel.publish_current().unwrap();
    assert_eq!(ticks.get(), 1);
    assert!(Object::is(
        &first,
        &get(&current_face, "getSnapshot")
            .dyn_into::<Function>()
            .unwrap()
            .call0(&current_face)
            .unwrap()
    ));

    let source: JsValue = Object::new().into();
    let registration = channel
        .provide(descriptor(source.clone(), JsValue::from_f64(7.0)))
        .unwrap();
    let added = get(&current_face, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&current_face)
        .unwrap();
    assert!(Object::is(&get(&get(&added, "hooks"), "extra"), &source));
    assert_eq!(get(&get(&added, "props"), "marker").as_f64(), Some(7.0));
    assert_eq!(ticks.get(), 2);
    let absent_with_roster = channel.maybe_info();
    assert!(get(&get(&absent_with_roster, "hooks"), "extra").is_undefined());

    registration.call0(&JsValue::UNDEFINED).unwrap();
    let removed = get(&current_face, "getSnapshot")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&current_face)
        .unwrap();
    assert!(!Reflect::has(&get(&removed, "hooks"), &JsValue::from_str("extra")).unwrap());
    assert_eq!(ticks.get(), 3);

    let error = channel.provide(invalid_descriptor()).unwrap_err();
    assert!(
        get(&error, "message")
            .as_string()
            .is_some_and(|message| message.contains("undeclared hook \"other\""))
    );
    assert!(
        !Reflect::has(
            &get(&channel.maybe_info(), "hooks"),
            &JsValue::from_str("extra")
        )
        .unwrap()
    );
}

#[wasm_bindgen_test]
fn throwing_current_subscriber_does_not_starve_later_subscribers() {
    let current = Rc::new(RefCell::new(absent_info()));
    let host = Object::new();
    set(
        &host,
        "rebuildBundles",
        &Function::new_no_args("return undefined"),
    );
    let current_for_resolve = current.clone();
    let resolve = Closure::wrap(
        Box::new(move || current_for_resolve.borrow().clone()) as Box<dyn FnMut() -> JsValue>
    );
    set(&host, "resolveCurrent", &resolve.into_js_value());
    let channel = WasmSessionProvideChannel::new(host.into()).unwrap();
    let face = channel.current_provide_info();
    let subscribe = get(&face, "subscribe").dyn_into::<Function>().unwrap();
    subscribe
        .call1(
            &face,
            &Function::new_no_args("throw new Error('listener boom')"),
        )
        .unwrap();
    let ticks = Rc::new(std::cell::Cell::new(0));
    let observed = ticks.clone();
    let listener =
        Closure::wrap(Box::new(move || observed.set(observed.get() + 1)) as Box<dyn FnMut()>);
    subscribe.call1(&face, &listener.into_js_value()).unwrap();

    *current.borrow_mut() = {
        let value = Object::new();
        set(&value, "sessionId", &JsValue::from_str("s1"));
        set(&value, "hooks", &Object::new());
        set(&value, "props", &Object::new());
        value.into()
    };
    channel.publish_current().unwrap();
    assert_eq!(ticks.get(), 1);
}
