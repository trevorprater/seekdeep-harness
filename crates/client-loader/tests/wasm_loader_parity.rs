//! Live compiled Cordis + browser Loader dependency and teardown parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Promise};
use seekdeep_client_loader::client_loader_plugin;
use seekdeep_cordis::{configure_context_wrapper, create_context};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function loaderContextWrapper() {
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

export function loaderInstall(root, plugin) { return root.plugin(plugin).await() }
export function loaderService(root) { return root.get('loader') }
export function loaderModules() {
  const modules = new Map([
    ['provider', {
      name: 'provider',
      apply(ctx) { ctx.provide('dep', { value: 41 }) },
    }],
    ['consumer', {
      name: 'consumer',
      inject: ['dep'],
      apply(ctx) { ctx.provide('answer', { value: ctx.get('dep').value + 1 }) },
    }],
  ])
  return { import(name) { return Promise.resolve(modules.get(name)) } }
}
export function loaderSetInternal(loader, internal) { loader.internal = internal }
export function loaderCreate(loader, id, name) { return loader.create({ id, name }) }
export function loaderCreateDisabled(loader) { return loader.create({ id: 'disabled', name: 'unavailable', disabled: true }) }
export function loaderWait(loader) { return loader.await() }
export function loaderRemove(loader, id) { return loader.remove(id) }
export function loaderEntries(loader) { return loader.entries() }
export function loaderEntryState(loader, id) { return loader.resolve(id).fiber.state }
export function loaderGet(root, name) { return root.get(name) }
export function loaderField(value, name) { return value?.[name] }
"#)]
extern "C" {
    fn loaderContextWrapper() -> JsValue;
    fn loaderInstall(root: &JsValue, plugin: &JsValue) -> Promise;
    fn loaderService(root: &JsValue) -> JsValue;
    fn loaderModules() -> JsValue;
    fn loaderSetInternal(loader: &JsValue, internal: &JsValue);
    fn loaderCreate(loader: &JsValue, id: &str, name: &str) -> Promise;
    fn loaderCreateDisabled(loader: &JsValue) -> Promise;
    fn loaderWait(loader: &JsValue) -> Promise;
    fn loaderRemove(loader: &JsValue, id: &str) -> Promise;
    fn loaderEntries(loader: &JsValue) -> Array;
    fn loaderEntryState(loader: &JsValue, id: &str) -> u8;
    fn loaderGet(root: &JsValue, name: &str) -> JsValue;
    fn loaderField(value: &JsValue, name: &str) -> JsValue;
}

#[wasm_bindgen_test(async)]
async fn disabled_entry_requires_no_importer_or_fiber() {
    configure_context_wrapper(loaderContextWrapper()).unwrap();
    let root = create_context().unwrap();
    JsFuture::from(loaderInstall(&root, &client_loader_plugin().unwrap()))
        .await
        .unwrap();
    let loader = loaderService(&root);
    let id = JsFuture::from(loaderCreateDisabled(&loader)).await.unwrap();
    assert_eq!(id.as_string().as_deref(), Some("disabled"));
    let entries = loaderEntries(&loader);
    assert_eq!(entries.length(), 1);
    assert!(loaderField(&entries.get(0), "fiber").is_undefined());
    JsFuture::from(loaderWait(&loader)).await.unwrap();
    JsFuture::from(loaderRemove(&loader, "disabled"))
        .await
        .unwrap();
    assert_eq!(loaderEntries(&loader).length(), 0);
}

#[wasm_bindgen_test(async)]
async fn loader_activates_dependency_graph_and_reverses_provider_loss() {
    configure_context_wrapper(loaderContextWrapper()).unwrap();
    let root = create_context().unwrap();
    JsFuture::from(loaderInstall(&root, &client_loader_plugin().unwrap()))
        .await
        .unwrap();
    let loader = loaderService(&root);
    loaderSetInternal(&loader, &loaderModules());

    let creates = Array::new();
    creates.push(&loaderCreate(&loader, "consumer", "consumer"));
    creates.push(&loaderCreate(&loader, "provider", "provider"));
    JsFuture::from(Promise::all(&creates)).await.unwrap();
    JsFuture::from(loaderWait(&loader)).await.unwrap();

    assert_eq!(loaderEntries(&loader).length(), 2);
    assert_eq!(loaderEntryState(&loader, "consumer"), 2);
    assert_eq!(loaderEntryState(&loader, "provider"), 2);
    assert_eq!(
        loaderField(&loaderGet(&root, "answer"), "value").as_f64(),
        Some(42.0)
    );

    JsFuture::from(loaderRemove(&loader, "provider"))
        .await
        .unwrap();
    JsFuture::from(loaderWait(&loader)).await.unwrap();
    assert_eq!(loaderEntryState(&loader, "consumer"), 0);
    assert!(loaderGet(&root, "answer").is_undefined());

    JsFuture::from(loaderRemove(&loader, "consumer"))
        .await
        .unwrap();
    assert_eq!(loaderEntries(&loader).length(), 0);
}
