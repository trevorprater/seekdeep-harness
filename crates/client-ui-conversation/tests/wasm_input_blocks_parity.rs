//! Live WASM coverage for browser composer-block stores.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_conversation::{
    BrowserComposerBlockRegistry, composer_block_registry_browser,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let notifications = 0
export function blockObject(entries) { return Object.fromEntries(entries) }
export function blockListener() { return () => { notifications += 1 } }
export function blockMutator(reason) { return draft => { draft.reason = reason } }
export function blockNotifications() { return notifications }
export function blockReset() { notifications = 0 }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = blockObject)]
    fn block_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = blockListener)]
    fn block_listener() -> Function;
    #[wasm_bindgen(js_name = blockMutator)]
    fn block_mutator(reason: &str) -> Function;
    #[wasm_bindgen(js_name = blockNotifications)]
    fn block_notifications() -> u32;
    #[wasm_bindgen(js_name = blockReset)]
    fn block_reset();
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    block_object(&array).unchecked_into()
}

fn call(registry: &JsValue, method: &str, arguments: &[JsValue]) -> JsValue {
    let function = property(registry, method).dyn_into::<Function>().unwrap();
    let arguments: Array = arguments.iter().collect();
    function.apply(registry, &arguments).unwrap()
}

fn snapshot(store: &JsValue) -> JsValue {
    call(store, "getSnapshot", &[])
}

#[wasm_bindgen_test]
fn browser_registry_preserves_store_identity_notifications_isolation_and_rebirth() {
    block_reset();
    let registry = composer_block_registry_browser().unwrap();
    let one = JsValue::from_str("s1");
    let two = JsValue::from_str("s2");
    let store = call(&registry, "storeFor", std::slice::from_ref(&one));
    assert!(Object::is(
        &store,
        &call(&registry, "storeFor", std::slice::from_ref(&one))
    ));
    assert!(!Object::is(
        &store,
        &call(&registry, "storeFor", std::slice::from_ref(&two))
    ));
    let unsubscribe = call(&store, "subscribe", &[block_listener().into()])
        .dyn_into::<Function>()
        .unwrap();

    let first = object(&[("reason", JsValue::from_str("choose model"))]);
    call(&registry, "set", &[one.clone(), first.clone().into()]);
    assert_eq!(block_notifications(), 1);
    let duplicate = object(&[("reason", JsValue::from_str("choose model"))]);
    call(&registry, "set", &[one.clone(), duplicate.into()]);
    assert_eq!(block_notifications(), 1);
    assert!(Object::is(&snapshot(&store), first.as_ref()));

    let second = object(&[("reason", JsValue::from_str("connect workspace"))]);
    call(&registry, "set", &[one.clone(), second.clone().into()]);
    assert_eq!(block_notifications(), 2);
    assert!(Object::is(&snapshot(&store), second.as_ref()));
    call(
        &store,
        "update",
        &[block_mutator("updated directly").into()],
    );
    assert_eq!(block_notifications(), 3);
    assert_eq!(
        property(&snapshot(&store), "reason").as_string().as_deref(),
        Some("updated directly")
    );
    call(&registry, "set", &[one.clone(), JsValue::UNDEFINED]);
    call(&registry, "set", &[one.clone(), JsValue::UNDEFINED]);
    assert_eq!(block_notifications(), 4);
    assert!(snapshot(&store).is_undefined());

    unsubscribe.call0(&JsValue::UNDEFINED).unwrap();
    let old = object(&[("reason", JsValue::from_str("old handle"))]);
    call(&registry, "set", &[one.clone(), old.clone().into()]);
    assert_eq!(block_notifications(), 4);
    call(&registry, "forget", std::slice::from_ref(&one));
    let reborn = call(&registry, "storeFor", std::slice::from_ref(&one));
    assert!(!Object::is(&store, &reborn));
    assert!(Object::is(&snapshot(&store), old.as_ref()));
    assert!(snapshot(&reborn).is_undefined());
}

#[wasm_bindgen_test]
fn clearing_absent_creates_store_and_invalid_session_fails_loudly() {
    let registry = composer_block_registry_browser().unwrap();
    let session = JsValue::from_str("s3");
    call(&registry, "set", &[session.clone(), JsValue::UNDEFINED]);
    let store = call(&registry, "storeFor", std::slice::from_ref(&session));
    assert!(snapshot(&store).is_undefined());
    let error = property(&registry, "storeFor")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&registry, &JsValue::from_f64(3.0))
        .unwrap_err();
    assert_eq!(
        property(&error, "message").as_string().as_deref(),
        Some("composer block session id must be a string")
    );
}

#[wasm_bindgen_test]
fn constructible_class_exports_the_same_store_lifecycle() {
    let registry = BrowserComposerBlockRegistry::new();
    let session = JsValue::from_str("class-session");
    let store = registry.store_for_browser_method(session.clone()).unwrap();
    let block = object(&[("reason", JsValue::from_str("class block"))]);
    registry
        .set_browser(session.clone(), block.clone().into())
        .unwrap();
    assert!(Object::is(&snapshot(&store), block.as_ref()));
    registry.forget_browser(session.clone()).unwrap();
    let reborn = registry.store_for_browser_method(session).unwrap();
    assert!(!Object::is(&store, &reborn));
    assert!(snapshot(&reborn).is_undefined());
}
