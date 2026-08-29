//! Live WASM coverage for the Session queue read face.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_conversation::queue_read_face_of_browser;
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let queue = []
let listeners = new Set()
let subscribeThis
let getThis
let calls = 0
export function installQueueSession() {
  queue = []; listeners = new Set(); subscribeThis = undefined; getThis = undefined; calls = 0
  return {
    getSnapshot() { getThis = this; return { queue } },
    subscribe(listener) {
      subscribeThis = this
      listeners.add(listener)
      return () => { listeners.delete(listener) }
    },
  }
}
export function queueSet(value) { queue = value }
export function queueEmit() { for (const listener of [...listeners]) listener() }
export function queueListener() { return () => { calls += 1 } }
export function queueCalls() { return calls }
export function queueListenerCount() { return listeners.size }
export function queueReceiversMatch(session) { return subscribeThis === session && getThis === session }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installQueueSession)]
    fn install_queue_session() -> JsValue;
    #[wasm_bindgen(js_name = queueSet)]
    fn queue_set(value: &Array);
    #[wasm_bindgen(js_name = queueEmit)]
    fn queue_emit();
    #[wasm_bindgen(js_name = queueListener)]
    fn queue_listener() -> Function;
    #[wasm_bindgen(js_name = queueCalls)]
    fn queue_calls() -> u32;
    #[wasm_bindgen(js_name = queueListenerCount)]
    fn queue_listener_count() -> u32;
    #[wasm_bindgen(js_name = queueReceiversMatch)]
    fn queue_receivers_match(session: &JsValue) -> bool;
}

fn property(value: &JsValue, key: &str) -> Function {
    Reflect::get(value, &JsValue::from_str(key))
        .unwrap()
        .dyn_into()
        .unwrap()
}

#[wasm_bindgen_test]
fn get_snapshot_returns_the_exact_latest_queue_reference() {
    let session = install_queue_session();
    let face = queue_read_face_of_browser(session.clone()).unwrap();
    let get_snapshot = property(&face, "getSnapshot");
    let first = Array::of1(&Object::new());
    queue_set(&first);
    let first_read = get_snapshot.call0(&face).unwrap();
    assert!(Object::is(&first_read, first.as_ref()));
    let second = Array::of2(&JsValue::from_str("a"), &JsValue::from_str("b"));
    queue_set(&second);
    let second_read = get_snapshot.call0(&face).unwrap();
    assert!(Object::is(&second_read, second.as_ref()));
    assert!(!Object::is(&first_read, &second_read));
}

#[wasm_bindgen_test]
fn subscribe_forwards_listener_and_returns_the_original_disposer() {
    let session = install_queue_session();
    let face = queue_read_face_of_browser(session.clone()).unwrap();
    let listener = queue_listener();
    let dispose = property(&face, "subscribe")
        .call1(&face, listener.as_ref())
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    assert_eq!(queue_listener_count(), 1);
    queue_emit();
    assert_eq!(queue_calls(), 1);
    dispose.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(queue_listener_count(), 0);
    queue_emit();
    assert_eq!(queue_calls(), 1);
    let _ = property(&face, "getSnapshot").call0(&face).unwrap();
    assert!(queue_receivers_match(&session));
}

#[wasm_bindgen_test]
fn each_projection_has_distinct_methods_over_the_same_session() {
    let session = install_queue_session();
    let first = queue_read_face_of_browser(session.clone()).unwrap();
    let second = queue_read_face_of_browser(session).unwrap();
    assert!(!Object::is(&first, &second));
    assert!(!Object::is(
        property(&first, "getSnapshot").as_ref(),
        property(&second, "getSnapshot").as_ref()
    ));
    assert!(!Object::is(
        property(&first, "subscribe").as_ref(),
        property(&second, "subscribe").as_ref()
    ));
}

#[wasm_bindgen_test]
fn malformed_session_failure_is_deferred_until_the_projected_method_runs() {
    let face = queue_read_face_of_browser(Object::new().into()).unwrap();
    assert!(property(&face, "getSnapshot").call0(&face).is_err());
    assert!(
        property(&face, "subscribe")
            .call1(&face, queue_listener().as_ref())
            .is_err()
    );
}
