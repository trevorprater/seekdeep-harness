//! Live browser Cordis parity for the event-only Remote double.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_test_runtime::install_test_remote;
use seekdeep_cordis::{configure_context_wrapper, create_context};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function testRemoteContextWrapper() {
  return core => {
    let ctx
    ctx = new Proxy(core, {
      get(target, key, receiver) {
        if (key === 'emit') return (name, ...args) => target.emitArgs(name, args)
        if (key === 'parallel') return (name, ...args) => target.parallelArgs(name, args)
        if (key === 'serial') return (name, ...args) => target.serialArgs(name, args)
        if (key === 'bail') return (name, ...args) => target.bailArgs(name, args)
        if (key === 'get') return name => target.get(name)
        if (Reflect.has(target, key)) {
          const value = Reflect.get(target, key, receiver)
          return typeof value === 'function' ? value.bind(target) : value
        }
        const metadata = target.metaGet(key)
        if (metadata !== undefined) return metadata
        return typeof key === 'string' ? target.get(key) : undefined
      },
    })
    return ctx
  }
}
export function testRemoteSeen() { return [] }
export function testRemoteListener(seen) { return value => { seen.push(value) } }
export function testRemoteThrowingListener() { return () => { throw new Error('listener failed') } }
"#)]
extern "C" {
    fn testRemoteContextWrapper() -> JsValue;
    fn testRemoteSeen() -> Array;
    fn testRemoteListener(seen: &Array) -> Function;
    fn testRemoteThrowingListener() -> Function;
}

fn method(value: &JsValue, name: &str) -> Function {
    Reflect::get(value, &JsValue::from_str(name))
        .unwrap()
        .dyn_into()
        .unwrap()
}

#[wasm_bindgen_test(async)]
async fn publishes_delivers_disposes_propagates_and_refuses_mount() {
    configure_context_wrapper(testRemoteContextWrapper()).unwrap();
    let root = create_context().unwrap();
    let remote = install_test_remote(root.clone()).unwrap();
    let published = method(&root, "get")
        .call1(&root, &JsValue::from_str("remote"))
        .unwrap();
    assert!(Object::is(&published, &remote));

    let seen = testRemoteSeen();
    let listener = testRemoteListener(&seen);
    let first = method(&remote, "$on")
        .call2(
            &remote,
            &JsValue::from_str("settings/document-updated"),
            &listener,
        )
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    let _duplicate = method(&remote, "$on")
        .call2(
            &remote,
            &JsValue::from_str("settings/document-updated"),
            &listener,
        )
        .unwrap();
    method(&remote, "$dispatch")
        .call2(
            &remote,
            &JsValue::from_str("settings/document-updated"),
            &Array::of2(&JsValue::from_str("ui-theme"), &JsValue::from_f64(1.0)),
        )
        .unwrap();
    assert_eq!(seen.length(), 1);
    assert_eq!(seen.get(0).as_string().as_deref(), Some("ui-theme"));
    first.call0(&JsValue::UNDEFINED).unwrap();
    first.call0(&JsValue::UNDEFINED).unwrap();
    method(&remote, "$dispatch")
        .call2(
            &remote,
            &JsValue::from_str("settings/document-updated"),
            &Array::of1(&JsValue::from_str("ignored")),
        )
        .unwrap();
    method(&remote, "$dispatch")
        .call2(
            &remote,
            &JsValue::from_str("credentials/updated"),
            &Array::new(),
        )
        .unwrap();
    assert_eq!(seen.length(), 1);

    let throwing = testRemoteThrowingListener();
    method(&remote, "$on")
        .call2(&remote, &JsValue::from_str("failure"), &throwing)
        .unwrap();
    assert!(
        method(&remote, "$dispatch")
            .call2(&remote, &JsValue::from_str("failure"), &Array::new())
            .is_err()
    );
    let rejected = method(&remote, "$mount")
        .call0(&remote)
        .unwrap()
        .dyn_into::<Promise>()
        .unwrap();
    let error = JsFuture::from(rejected).await.unwrap_err();
    assert!(
        Reflect::get(&error, &JsValue::from_str("message"))
            .unwrap()
            .as_string()
            .unwrap()
            .contains("needs the real Client Remote service")
    );

    let fiber = Reflect::get(&root, &JsValue::from_str("fiber")).unwrap();
    let disposal = method(&fiber, "dispose").call0(&fiber).unwrap();
    JsFuture::from(Promise::resolve(&disposal)).await.unwrap();
}
