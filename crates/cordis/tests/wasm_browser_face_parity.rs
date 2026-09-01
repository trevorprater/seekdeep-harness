//! Live Rust/WASM browser Context, plugin, service, event, and effect parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Promise, Reflect};
use seekdeep_cordis::{configure_context_wrapper, create_context};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function cordisContextWrapper() {
  return core => new Proxy(core, {
    get(target, key, receiver) {
      if (key === 'emit') return (name, ...args) => target.emitArgs(name, args)
      if (key === 'parallel') return (name, ...args) => target.parallelArgs(name, args)
      if (key === 'serial') return (name, ...args) => target.serialArgs(name, args)
      if (key === 'bail') return (name, ...args) => target.bailArgs(name, args)
      if (Reflect.has(target, key)) {
        const value = Reflect.get(target, key, receiver)
        return typeof value === 'function' ? value.bind(target) : value
      }
      const metadata = target.metaGet(key)
      if (metadata !== undefined) return metadata
      return typeof key === 'string' ? target.get(key) : undefined
    },
  })
}

export function cordisConsumer(log) {
  return {
    name: 'consumer',
    inject: ['dep'],
    apply(ctx) {
      log.push('apply:' + ctx.dep.value)
      ctx.provide('answer', { value: ctx.get('dep').value + 1 })
      ctx.effect(() => () => log.push('cleanup'), 'consumer cleanup')
    },
  }
}

export function cordisPlugin(root, plugin) { return root.plugin(plugin) }
export function cordisProvide(root, name, value) { return root.provide(name, value) }
export function cordisGet(root, name) { return root.get(name) }
export function cordisFiberState(fiber) { return fiber.state }
export function cordisFiberWait(fiber) { return fiber.await() }
export function cordisFiberDispose(fiber) { return fiber.dispose() }
export function cordisDispose(dispose) { return dispose() }
export function cordisValue(value) { return { value } }
export function cordisField(value, key) { return value?.[key] }
export function cordisLog() { return [] }
export function cordisLogValues(log) { return [...log] }
export function cordisOn(root, name, listener) { return root.on(name, listener) }
export function cordisListener(value) { return () => value }
export function cordisBail(root, name) { return root.bail(name) }
"#)]
extern "C" {
    fn cordisContextWrapper() -> JsValue;
    fn cordisConsumer(log: &JsValue) -> JsValue;
    fn cordisPlugin(root: &JsValue, plugin: &JsValue) -> JsValue;
    fn cordisProvide(root: &JsValue, name: &str, value: &JsValue) -> Function;
    fn cordisGet(root: &JsValue, name: &str) -> JsValue;
    fn cordisFiberState(fiber: &JsValue) -> u8;
    fn cordisFiberWait(fiber: &JsValue) -> Promise;
    fn cordisFiberDispose(fiber: &JsValue) -> Promise;
    fn cordisDispose(dispose: &Function) -> Promise;
    fn cordisValue(value: u32) -> JsValue;
    fn cordisField(value: &JsValue, key: &str) -> JsValue;
    fn cordisLog() -> JsValue;
    fn cordisLogValues(log: &JsValue) -> Array;
    fn cordisOn(root: &JsValue, name: &str, listener: &Function) -> Function;
    fn cordisListener(value: &JsValue) -> Function;
    fn cordisBail(root: &JsValue, name: &str) -> JsValue;
}

#[wasm_bindgen_test(async)]
async fn browser_context_keeps_plugin_services_events_and_cleanup_in_rust() {
    configure_context_wrapper(cordisContextWrapper()).unwrap();
    let root = create_context().unwrap();
    let log = cordisLog();
    let fiber = cordisPlugin(&root, &cordisConsumer(&log));
    assert_eq!(cordisFiberState(&fiber), 0);
    assert!(cordisGet(&root, "answer").is_undefined());

    let dep = cordisProvide(&root, "dep", &cordisValue(41));
    JsFuture::from(cordisFiberWait(&fiber)).await.unwrap();
    assert_eq!(cordisFiberState(&fiber), 2);
    assert_eq!(
        cordisField(&cordisGet(&root, "answer"), "value").as_f64(),
        Some(42.0)
    );
    assert_eq!(
        cordisLogValues(&log)
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        ["apply:41"]
    );

    let event_value = JsValue::from_str("accepted");
    let listener = cordisListener(&event_value);
    let off = cordisOn(&root, "decision", &listener);
    let reply = cordisBail(&root, "decision");
    let reply = if reply.is_instance_of::<Promise>() {
        JsFuture::from(reply.unchecked_into::<Promise>())
            .await
            .unwrap()
    } else {
        reply
    };
    assert_eq!(reply.as_string().as_deref(), Some("accepted"));
    JsFuture::from(cordisDispose(&off)).await.unwrap();

    JsFuture::from(cordisFiberDispose(&fiber)).await.unwrap();
    assert_eq!(cordisFiberState(&fiber), 4);
    assert!(cordisGet(&root, "answer").is_undefined());
    assert_eq!(
        cordisLogValues(&log)
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        ["apply:41", "cleanup"]
    );
    JsFuture::from(cordisDispose(&dep)).await.unwrap();
    assert!(cordisGet(&root, "dep").is_undefined());

    assert!(
        Reflect::get(&root, &JsValue::from_str("__seekdeepContext"))
            .unwrap()
            .is_object()
    );
}
