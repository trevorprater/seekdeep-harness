//! Live compiled Connection, Typert, and API gateway boot contracts.

#![cfg(target_arch = "wasm32")]

use js_sys::{Function, Promise, Reflect};
use seekdeep_client_foundation_wasm::{
    client_api_gateway_plugin, client_connection_plugin, client_typert_registry_plugin,
};
use seekdeep_cordis::{configure_context_wrapper, create_context};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function foundationContextWrapper() {
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

export function foundationInstallFetch() {
  const original = globalThis.fetch
  const calls = []
  globalThis.fetch = async request => {
    const body = await request.json()
    calls.push({ url: new URL(request.url).pathname, body })
    const value = body.method === 'host.describe'
      ? { local: true, canOpenPaths: true }
      : body.method === 'session.list'
        ? []
        : null
    return new Response(JSON.stringify({
      type: 'server-response',
      rpcId: body.rpcId,
      result: { ok: true, value },
    }), { status: 200, headers: { 'content-type': 'application/json' } })
  }
  return { calls, restore() { globalThis.fetch = original } }
}
export function foundationPlugin(root, plugin) { return root.plugin(plugin).await() }
export function foundationGet(root, name) { return root.get(name) }
export function foundationCall(api) { return api.sessions.list({}) }
export function foundationStart(connection, calls) {
  return connection.start({
    onStateChange(state) { calls.push(['state', state]) },
    onConnected(description) { calls.push(['connected', description.local]) },
  })
}
export function foundationCalls() { return [] }
export function foundationValues(values) { return [...values] }
export function foundationFetchCalls(control) { return [...control.calls] }
export function foundationRestore(control) { control.restore() }
export function foundationStop(handle) { handle.stop() }
export function foundationResult(response) { return response.result }
export function foundationFlush() { return new Promise(resolve => setTimeout(resolve, 10)) }
"#)]
extern "C" {
    fn foundationContextWrapper() -> JsValue;
    fn foundationInstallFetch() -> JsValue;
    fn foundationPlugin(root: &JsValue, plugin: &JsValue) -> Promise;
    fn foundationGet(root: &JsValue, name: &str) -> JsValue;
    fn foundationCall(api: &JsValue) -> Promise;
    fn foundationStart(connection: &JsValue, calls: &JsValue) -> JsValue;
    fn foundationCalls() -> JsValue;
    fn foundationValues(values: &JsValue) -> js_sys::Array;
    fn foundationFetchCalls(control: &JsValue) -> js_sys::Array;
    fn foundationRestore(control: &JsValue);
    fn foundationStop(handle: &JsValue);
    fn foundationResult(response: &JsValue) -> JsValue;
    fn foundationFlush() -> Promise;
}

#[wasm_bindgen_test(async)]
async fn foundations_publish_services_and_route_unary_calls() {
    configure_context_wrapper(foundationContextWrapper()).unwrap();
    let fetch = foundationInstallFetch();
    let root = create_context().unwrap();
    for plugin in [
        client_connection_plugin().unwrap(),
        client_typert_registry_plugin().unwrap(),
        client_api_gateway_plugin().unwrap(),
    ] {
        JsFuture::from(foundationPlugin(&root, &plugin))
            .await
            .unwrap();
    }

    let connection = foundationGet(&root, "connection");
    let api = Reflect::get(&connection, &JsValue::from_str("api")).unwrap();
    let response = JsFuture::from(foundationCall(&api)).await.unwrap();
    assert_eq!(
        Reflect::get(&foundationResult(&response), &JsValue::from_str("ok"))
            .unwrap()
            .as_bool(),
        Some(true)
    );
    let fetch_calls = foundationFetchCalls(&fetch);
    assert_eq!(fetch_calls.length(), 1);
    assert_eq!(
        Reflect::get(&fetch_calls.get(0), &JsValue::from_str("url"))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("/api/session.list")
    );

    let callbacks = foundationCalls();
    let handle = foundationStart(&connection, &callbacks);
    JsFuture::from(foundationFlush()).await.unwrap();
    assert_eq!(
        foundationValues(&callbacks)
            .iter()
            .map(|value| js_sys::JSON::stringify(&value)
                .unwrap()
                .as_string()
                .unwrap())
            .collect::<Vec<_>>(),
        ["[\"state\",\"connected\"]", "[\"connected\",true]"]
    );
    assert!(!foundationGet(&root, "remote").is_undefined());
    assert!(!foundationGet(&root, "remote.commands").is_undefined());
    foundationStop(&handle);
    foundationRestore(&fetch);

    assert!(foundationGet(&root, "typert").is_object());
    assert!(foundationGet(&root, "connection").is_object());
    let start = Reflect::get(&connection, &JsValue::from_str("start"))
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    assert!(start.is_function());
}
