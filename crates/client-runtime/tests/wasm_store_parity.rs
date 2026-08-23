//! Live JavaScript Store engine, actions, persistence, freezing, and equality parity.

#![cfg(target_arch = "wasm32")]

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use js_sys::{Function, Object, Reflect};
use seekdeep_client_runtime::{
    create_snapshot_store, define_store, install_store_produce, shallow_equal,
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_test::*;

fn set(object: &Object, key: &str, value: &JsValue) {
    assert!(Reflect::set(object, &JsValue::from_str(key), value).unwrap());
}

fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn call(value: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let method = get(value, method).dyn_into::<Function>()?;
    let arguments = js_sys::Array::new();
    for arg in args {
        arguments.push(arg);
    }
    method.apply(value, &arguments)
}

fn install_test_produce() {
    install_store_produce(Function::new_with_args(
        "base, recipe",
        "const next = Array.isArray(base) ? base.slice() : { ...base }; recipe(next); return next",
    ));
}

fn options(flush: Option<&str>, persist: Option<&str>) -> JsValue {
    let options = Object::new();
    if let Some(flush) = flush {
        set(&options, "flush", &JsValue::from_str(flush));
    }
    if let Some(name) = persist {
        let persistence = Object::new();
        set(&persistence, "name", &JsValue::from_str(name));
        set(&options, "persist", &persistence);
    }
    options.into()
}

#[wasm_bindgen_test]
fn snapshot_updates_preserve_untouched_identity_notify_sync_and_freeze_set() {
    install_test_produce();
    let initial = Object::new();
    let a = Object::new();
    set(&a, "n", &JsValue::from_f64(1.0));
    let b = Object::new();
    set(&b, "label", &JsValue::from_str("same"));
    set(&initial, "a", &a);
    set(&initial, "b", &b);
    let store = create_snapshot_store(initial.into(), options(None, None)).unwrap();
    let calls = Rc::new(RefCell::new(0_usize));
    let observed = calls.clone();
    let listener = Closure::wrap(Box::new(move || {
        *observed.borrow_mut() += 1;
    }) as Box<dyn FnMut()>);
    call(&store, "subscribe", &[listener.into_js_value()]).unwrap();
    let update = Closure::wrap(Box::new(move |draft: JsValue| {
        let next = Object::new();
        set(&next, "n", &JsValue::from_f64(2.0));
        Reflect::set(&draft, &JsValue::from_str("a"), &next).unwrap();
    }) as Box<dyn FnMut(JsValue)>);
    call(&store, "update", &[update.into_js_value()]).unwrap();
    let snapshot = call(&store, "getSnapshot", &[]).unwrap();
    assert_eq!(get(&get(&snapshot, "a"), "n").as_f64(), Some(2.0));
    assert!(Object::is(&get(&snapshot, "b"), &b));
    assert_eq!(*calls.borrow(), 1);

    let replacement = Object::new();
    let nested = Object::new();
    set(&nested, "n", &JsValue::from_f64(3.0));
    set(&replacement, "a", &nested);
    call(&store, "set", &[replacement.clone().into()]).unwrap();
    assert!(Object::is_frozen(&replacement));
    assert!(Object::is_frozen(&nested));
}

#[wasm_bindgen_test]
fn frame_mode_coalesces_until_the_injected_animation_frame() {
    install_test_produce();
    let frames = Rc::new(RefCell::new(Vec::<Function>::new()));
    let observed_frames = frames.clone();
    let request = Closure::wrap(Box::new(move |callback: Function| -> f64 {
        observed_frames.borrow_mut().push(callback);
        observed_frames.borrow().len().to_string().parse().unwrap()
    }) as Box<dyn FnMut(Function) -> f64>);
    let global = js_sys::global();
    set(&global, "requestAnimationFrame", &request.into_js_value());
    let store = create_snapshot_store(Object::new().into(), options(Some("raf"), None)).unwrap();
    let calls = Rc::new(RefCell::new(0_usize));
    let observed = calls.clone();
    let listener = Closure::wrap(Box::new(move || {
        *observed.borrow_mut() += 1;
    }) as Box<dyn FnMut()>);
    let off = call(&store, "subscribe", &[listener.into_js_value()])
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    for value in [1.0, 2.0, 3.0] {
        let next = Object::new();
        set(&next, "n", &JsValue::from_f64(value));
        call(&store, "set", &[next.into()]).unwrap();
    }
    assert_eq!(frames.borrow().len(), 1);
    assert_eq!(*calls.borrow(), 0);
    frames
        .borrow_mut()
        .remove(0)
        .call1(&JsValue::UNDEFINED, &JsValue::from_f64(0.0))
        .unwrap();
    assert_eq!(*calls.borrow(), 1);
    let next = Object::new();
    call(&store, "set", &[next.into()]).unwrap();
    off.call0(&JsValue::UNDEFINED).unwrap();
    frames
        .borrow_mut()
        .remove(0)
        .call1(&JsValue::UNDEFINED, &JsValue::from_f64(1.0))
        .unwrap();
    assert_eq!(*calls.borrow(), 1);
    Reflect::delete_property(&global, &JsValue::from_str("requestAnimationFrame")).unwrap();
}

fn install_storage() -> (Rc<RefCell<HashMap<String, String>>>, Object) {
    let values = Rc::new(RefCell::new(HashMap::<String, String>::new()));
    let storage = Object::new();
    let read_values = values.clone();
    let get_item = Closure::wrap(Box::new(move |key: String| -> JsValue {
        read_values
            .borrow()
            .get(&key)
            .map_or(JsValue::NULL, |value| JsValue::from_str(value))
    }) as Box<dyn FnMut(String) -> JsValue>);
    set(&storage, "getItem", &get_item.into_js_value());
    let write_values = values.clone();
    let set_item = Closure::wrap(Box::new(move |key: String, value: String| {
        write_values.borrow_mut().insert(key, value);
    }) as Box<dyn FnMut(String, String)>);
    set(&storage, "setItem", &set_item.into_js_value());
    let remove_values = values.clone();
    let remove_item = Closure::wrap(Box::new(move |key: String| {
        remove_values.borrow_mut().remove(&key);
    }) as Box<dyn FnMut(String)>);
    set(&storage, "removeItem", &remove_item.into_js_value());
    set(&js_sys::global(), "localStorage", &storage);
    (values, storage)
}

#[wasm_bindgen_test]
fn primitive_persistence_and_declarative_actions_use_whole_values_and_scoped_keys() {
    install_test_produce();
    let (values, _storage) = install_storage();
    let store = create_snapshot_store(JsValue::from_str(""), options(None, Some("draft"))).unwrap();
    call(&store, "set", &[JsValue::from_str("hello")]).unwrap();
    let revived =
        create_snapshot_store(JsValue::from_str(""), options(None, Some("draft"))).unwrap();
    assert_eq!(
        call(&revived, "getSnapshot", &[])
            .unwrap()
            .as_string()
            .as_deref(),
        Some("hello")
    );

    let declaration = Object::new();
    let init = Function::new_no_args("return { selection: null, draft: '' }");
    set(&declaration, "init", &init);
    set(&declaration, "persist", &JsValue::from_str("chat"));
    let actions = Object::new();
    let set_draft = Function::new_with_args("draft, text", "draft.draft = text");
    set(&actions, "setDraft", &set_draft);
    set(&declaration, "actions", &actions);
    let handle = define_store(declaration.into()).unwrap();
    let first = call(&handle, "create", &[JsValue::from_str("s1")]).unwrap();
    let second = call(&handle, "create", &[JsValue::from_str("s2")]).unwrap();
    call(
        &get(&first, "actions"),
        "setDraft",
        &[JsValue::from_str("one")],
    )
    .unwrap();
    call(
        &get(&second, "actions"),
        "setDraft",
        &[JsValue::from_str("two")],
    )
    .unwrap();
    assert!(values.borrow().contains_key("chat.s1"));
    assert!(values.borrow().contains_key("chat.s2"));
    assert_eq!(
        get(&call(&first, "getSnapshot", &[]).unwrap(), "draft")
            .as_string()
            .as_deref(),
        Some("one")
    );
    call(&first, "clearPersisted", &[]).unwrap();
    assert!(!values.borrow().contains_key("chat.s1"));
    assert!(values.borrow().contains_key("chat.s2"));
    Reflect::delete_property(&js_sys::global(), &JsValue::from_str("localStorage")).unwrap();
}

#[wasm_bindgen_test]
fn shallow_equality_preserves_one_level_reference_semantics() {
    let leaf = Object::new();
    let left = Object::new();
    set(&left, "x", &JsValue::from_f64(1.0));
    set(&left, "y", &leaf);
    let right = Object::new();
    set(&right, "x", &JsValue::from_f64(1.0));
    set(&right, "y", &leaf);
    assert!(shallow_equal(left.into(), right.into()));
    let drift = Object::new();
    set(&drift, "x", &JsValue::from_f64(1.0));
    set(&drift, "y", &Object::new());
    let baseline = Object::new();
    set(&baseline, "x", &JsValue::from_f64(1.0));
    set(&baseline, "y", &Object::new());
    assert!(!shallow_equal(drift.into(), baseline.into()));
    assert!(shallow_equal(
        js_sys::Array::of2(&JsValue::from_f64(1.0), &JsValue::from_f64(2.0)).into(),
        js_sys::Array::of2(&JsValue::from_f64(1.0), &JsValue::from_f64(2.0)).into(),
    ));
}
