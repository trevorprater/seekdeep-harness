//! Live JavaScript Client Agent-scope tag and Cordis filter parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Function, Object, Reflect};
use seekdeep_client_runtime::{create_client_scope, scope_of};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_test::*;

fn set(object: &Object, key: &str, value: &JsValue) {
    assert!(Reflect::set(object, &JsValue::from_str(key), value).unwrap());
}

fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

#[wasm_bindgen_test]
fn scope_tags_context_and_filter_accepts_untagged_and_same_but_rejects_foreign() {
    let filter = Function::new_with_args("description", "return Symbol(description)")
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("Context.filter"))
        .unwrap();
    let constructor = Object::new();
    set(&constructor, "filter", &filter);
    let base = Object::new();
    set(&base, "constructor", &constructor);
    let extend = Function::new_with_args(
        "extension",
        "Object.setPrototypeOf(extension, this); return extension",
    );
    set(&base, "extend", &extend);
    let root = Object::new();
    let base_for_plugin = base.clone();
    let plugin = Closure::wrap(Box::new(move |_plugin: JsValue| {
        let fiber = Object::new();
        set(&fiber, "ctx", &base_for_plugin);
        fiber
    }) as Box<dyn FnMut(JsValue) -> Object>);
    set(&root, "plugin", &plugin.into_js_value());
    let a = create_client_scope(root.clone().into(), "a".to_owned()).unwrap();
    let b = create_client_scope(root.clone().into(), "b".to_owned()).unwrap();
    let a_context = get(&a, "ctx");
    let b_context = get(&b, "ctx");
    assert_eq!(scope_of(a_context.clone()).as_deref(), Some("a"));
    assert_eq!(scope_of(root.clone().into()), None);
    let predicate = Reflect::get(&a_context, &filter)
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    assert_eq!(
        predicate.call1(&a_context, &root).unwrap().as_bool(),
        Some(true)
    );
    assert_eq!(
        predicate.call1(&a_context, &a_context).unwrap().as_bool(),
        Some(true)
    );
    assert_eq!(
        predicate.call1(&a_context, &b_context).unwrap().as_bool(),
        Some(false)
    );
}
