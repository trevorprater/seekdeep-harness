//! Live WASM `AppWebEntry` two-stage success, fail-loud, and malformed-manifest parity.

#![cfg(target_arch = "wasm32")]

use js_sys::Reflect;
use seekdeep_client_web::{WasmAppWebEntry, configure_client_web};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function bootBench(manifest) {
  const roots = []
  const React = { Fragment: 'Fragment', createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } } }
  const reactDomClient = { createRoot(element) { const root = { element, node: undefined, render(node) { this.node = node }, unmount() { this.node = undefined } }; roots.push(root); return root } }
  const slots = { installs: [], install(renderer) { this.installs.push(renderer) }, renderSlot() { return 'ROOT' } }
  const sessions = { list: { getSnapshot: () => ({ current: undefined, byId: {} }), subscribe: () => () => {} } }
  const services = { slots, sessions, layout: {} }
  class Context {
    constructor() {
      this.services = services
      this.reflect = { provide: (name, value) => { this.services[name] = value; return () => { delete this.services[name] } } }
      this.listeners = new Map()
    }
    get(name) { return this.services[name] }
    plugin(plugin) { this.loader = plugin.make(this); return Promise.resolve() }
    on(name, listener) { this.listeners.set(name, listener); return () => this.listeners.delete(name) }
  }
  class ClientModuleSystem {
    constructor(options) { this.options = options; this.statics = new Map(); this.prefetched = [] }
    registerStatic(id, value) { this.statics.set(id, value) }
    prefetch(id) { this.prefetched.push(id); return Promise.resolve() }
    import(id) {
      if (this.statics.has(id)) return Promise.resolve(this.statics.get(id))
      const row = (manifest.plugins ?? []).find(row => row.id === id)
      if (row?.fail) return Promise.reject(new Error('import:' + id))
      return Promise.resolve(row?.module ?? {})
    }
  }
  const loaderPlugin = {
    make(ctx) {
      const entries = []
      let internal
      return {
        get internal() { return internal }, set internal(value) { internal = value },
        async create(options) {
          const entry = { options, fiber: undefined }
          entries.push(entry)
          const module = await internal.import(options.name)
          if (typeof module.apply === 'function') module.apply(ctx)
          entry.fiber = { state: 2, inject: {} }
          ctx.listeners.get('internal/status')?.({ entry })
          return options.name
        },
        resolve(id) { return entries.find(entry => entry.options.name === id) },
        await() { return Promise.resolve() }, entries() { return entries },
      }
    },
  }
  const clientModules = { parseBootManifest(raw) { if (!raw || raw.invalid) throw new Error('malformed manifest'); return raw }, ClientModuleSystem }
  const modulesClient = { apply() {} }
  const webReact = { createSlotRenderer: () => ({ renderRoot() {} }), bindSnapshotSelector: source => selector => selector(source.getSnapshot()) }
  const staticModules = { react: React }
  const document = { head: { appendChild() {} }, createElement() { return { setAttribute() {}, textContent: '' } } }
  globalThis.document = document
  globalThis.__SEEKDEEP_BOOT__ = manifest
  delete globalThis.__SEEKDEEP_CLIENT_CONTEXT__
  return { React, reactDomClient, cordis: { Context }, loaderPlugin, clientModules, modulesClient, webReact, staticModules, roots, slots }
}
export function bootRoot(bench) { return bench.roots[0] }
export function bootSignalValue(root, name) { return root.node.props[name].getSnapshot() }
export function bootSlots(bench) { return bench.slots }
export function bootElement() { return { id: 'root' } }
export function bootManifestSuccess() { return { modules: [], plugins: [] } }
export function bootManifestFailure() { return { modules: [], plugins: [{ id: 'broken', immediately: true, fail: true }] } }
export function bootManifestInvalid() { return { invalid: true } }
"#)]
extern "C" {
    fn bootBench(manifest: &JsValue) -> JsValue;
    fn bootRoot(bench: &JsValue) -> JsValue;
    fn bootSignalValue(root: &JsValue, name: &str) -> JsValue;
    fn bootSlots(bench: &JsValue) -> JsValue;
    fn bootElement() -> JsValue;
    fn bootManifestSuccess() -> JsValue;
    fn bootManifestFailure() -> JsValue;
    fn bootManifestInvalid() -> JsValue;
}

fn property(value: &JsValue, name: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(name)).unwrap()
}

fn configure(bench: &JsValue) {
    configure_client_web(
        property(bench, "React"),
        property(bench, "reactDomClient"),
        property(bench, "cordis"),
        property(bench, "loaderPlugin"),
        property(bench, "clientModules"),
        property(bench, "modulesClient"),
        property(bench, "webReact"),
        property(bench, "staticModules"),
    )
    .unwrap();
}

async fn run_entry(bench: &JsValue) -> (WasmAppWebEntry, Result<JsValue, JsValue>) {
    configure(bench);
    let entry = WasmAppWebEntry::new(bootElement(), JsValue::UNDEFINED).unwrap();
    let result = JsFuture::from(entry.run()).await;
    (entry, result)
}

#[wasm_bindgen_test(async)]
async fn success_activates_modules_and_app_shell_then_flips_settled() {
    let bench = bootBench(&bootManifestSuccess());
    let (entry, result) = run_entry(&bench).await;
    result.unwrap();
    let root = bootRoot(&bench);
    assert_eq!(bootSignalValue(&root, "settled").as_bool(), Some(true));
    assert!(bootSignalValue(&root, "error").is_undefined());
    assert_eq!(
        property(&bootSlots(&bench), "installs")
            .unchecked_into::<js_sys::Array>()
            .length(),
        1
    );
    entry.dispose();
    assert!(property(&root, "node").is_undefined());
}

#[wasm_bindgen_test(async)]
async fn plugin_failure_resolves_and_stays_on_fail_loud_gate() {
    let bench = bootBench(&bootManifestFailure());
    let (_entry, result) = run_entry(&bench).await;
    result.unwrap();
    let root = bootRoot(&bench);
    assert_eq!(bootSignalValue(&root, "settled").as_bool(), Some(false));
    assert!(
        bootSignalValue(&root, "error")
            .as_string()
            .unwrap()
            .contains("import:broken")
    );
}

#[wasm_bindgen_test(async)]
async fn malformed_manifest_rejects_before_mounting_any_root() {
    let bench = bootBench(&bootManifestInvalid());
    let (_entry, result) = run_entry(&bench).await;
    let error = result.unwrap_err();
    assert!(
        property(&error, "message")
            .as_string()
            .unwrap()
            .contains("malformed manifest")
    );
    assert!(bootRoot(&bench).is_undefined());
}
