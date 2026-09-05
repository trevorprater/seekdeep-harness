//! Live Rust/WASM plugin-inventory Settings component and Client plugin parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Promise, Reflect};
use seekdeep_client_ui_settings_plugin_inventory::{
    apply_client_ui_settings_plugin_inventory, configure_client_ui_settings_plugin_inventory,
    exported_plugin_inventory_settings_tab_component, settings_plugin_inventory_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
if (typeof globalThis.document === 'undefined') {
  const nodes = []
  const selected = selector => {
    const match = /^style\[data-plugin=(.+)\]$/.exec(selector)
    if (match === null) return []
    const plugin = JSON.parse(match[1])
    return nodes.filter(node => node.kind === 'style' && node.attributes['data-plugin'] === plugin)
  }
  globalThis.document = {
    querySelector(selector) { return selected(selector)[0] ?? null },
    querySelectorAll(selector) { return selected(selector) },
    createElement(kind) {
      return {
        kind, attributes: {}, textContent: '',
        setAttribute(name, value) { this.attributes[name] = value },
      }
    },
    head: { appendChild(node) { nodes.push(node); return node } },
  }
}

function hooks() {
  const slots = []
  let cursor = 0
  let pending = []
  const React = {
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    useId() {
      const index = cursor++
      if (slots[index] === undefined) slots[index] = { kind: 'id', value: `:inventory-${index}:` }
      return slots[index].value
    },
    useState(initial) {
      const index = cursor++
      if (slots[index] === undefined) {
        const slot = { kind: 'state', value: typeof initial === 'function' ? initial() : initial }
        slot.set = value => { slot.value = typeof value === 'function' ? value(slot.value) : value }
        slots[index] = slot
      }
      return [slots[index].value, slots[index].set]
    },
    useEffect(effect, dependencies) {
      const index = cursor++
      if (slots[index] === undefined) slots[index] = { kind: 'effect', deps: undefined, cleanup: undefined }
      const slot = slots[index]
      const deps = Array.from(dependencies)
      const changed = slot.deps === undefined || slot.deps.length !== deps.length
        || slot.deps.some((value, at) => !Object.is(value, deps[at]))
      if (changed) pending.push({ slot, deps, effect })
    },
  }
  return {
    React,
    render(component, props) {
      cursor = 0
      pending = []
      const tree = component(props)
      for (const entry of pending) {
        if (typeof entry.slot.cleanup === 'function') entry.slot.cleanup()
        entry.slot.deps = entry.deps
        entry.slot.cleanup = entry.effect()
      }
      return tree
    },
    unmount() {
      for (const slot of [...slots].reverse()) {
        if (slot?.kind === 'effect' && typeof slot.cleanup === 'function') slot.cleanup()
      }
    },
  }
}

function textOf(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(textOf).join('')
  return (node.children ?? []).map(textOf).join('')
}
function find(node, predicate) {
  if (node === null || node === undefined || node === false) return undefined
  if (typeof node === 'string' || typeof node === 'number') return undefined
  if (!Array.isArray(node) && predicate(node)) return node
  for (const child of Array.isArray(node) ? node : node.children ?? []) {
    const found = find(child, predicate)
    if (found !== undefined) return found
  }
  return undefined
}
function findAll(node, predicate, result = []) {
  if (node === null || node === undefined || node === false
    || typeof node === 'string' || typeof node === 'number') return result
  if (!Array.isArray(node) && predicate(node)) result.push(node)
  for (const child of Array.isArray(node) ? node : node.children ?? []) findAll(child, predicate, result)
  return result
}
const en = {
  tab: 'Plugin list', loading: 'Reading plugins…', error: 'Plugins are temporarily unavailable.',
  retry: 'Retry', search: 'Search plugins', catalog: 'Plugin list', empty: 'No plugins are available.',
  emptySearch: 'No matching plugins.', enabledTag: 'Enabled', disabledTag: 'Disabled',
  configuration: 'Configuration', cordis: 'Cordis status', unobserved: 'Not mounted',
  pending: 'Waiting for dependencies', loadingPhase: 'Loading', active: 'Mounted',
  failed: 'Mount failed', unloading: 'Unloading',
}
const SNAPSHOT = { entries: [
  { entryId: '8a1b2c3d', moduleName: '@seekdeep-ai/cordis-plugin-hmr', enabled: true, fiberPhase: 'active' },
  { entryId: 'pending', moduleName: 'cordis:pending-name', enabled: true, fiberPhase: 'pending' },
  { entryId: 'loading', moduleName: '@fixture/loading-name', enabled: true, fiberPhase: 'loading' },
  { entryId: 'failed', moduleName: '@fixture/failed-name', enabled: true, fiberPhase: 'failed' },
  { entryId: 'unloading', moduleName: '@fixture/unloading-name', enabled: true, fiberPhase: 'unloading' },
  { entryId: 'unobserved', moduleName: '@fixture/unobserved-name', enabled: true, fiberPhase: null },
  { entryId: 'disabled-entry', moduleName: '@seekdeep-ai/seekdeep-host-directory-picker-native', enabled: false, fiberPhase: null },
] }

export function makeInventoryBench() {
  const hookState = hooks()
  const bench = {
    hooks: hookState, React: hookState.React,
    primitives: { IconChevronDownOutline14: 'IconChevronDownOutline14', IconSearchOutline16: 'IconSearchOutline16' },
    mode: 'pending', value: SNAPSHOT, calls: 0, resolve: undefined, reject: undefined,
  }
  bench.props = {
    list() {
      bench.calls += 1
      if (bench.mode === 'sync') throw new Error('namespace unavailable')
      if (bench.mode === 'reject') return Promise.reject(new Error('private transport detail'))
      if (bench.mode === 'resolved') return Promise.resolve(bench.value)
      return new Promise((resolve, reject) => { bench.resolve = resolve; bench.reject = reject })
    },
    t(key) { return en[key] ?? key },
  }
  return bench
}
export function inventoryRender(bench, component) { bench.tree = bench.hooks.render(component, bench.props); return bench.tree }
export function inventorySetMode(bench, mode, value) { bench.mode = mode; if (value !== undefined) bench.value = value }
export function inventoryResolve(bench, value) { bench.resolve?.(value) }
export function inventoryReject(bench, value) { bench.reject?.(value) }
export function inventoryCalls(bench) { return bench.calls }
export function inventoryUnmount(bench) { bench.hooks.unmount() }
export function inventoryText(tree) { return textOf(tree) }
export function inventoryFindAria(tree, label) { return find(tree, node => node.props?.['aria-label'] === label) }
export function inventoryFindRole(tree, role) { return find(tree, node => node.props?.role === role) }
export function inventoryFindKind(tree, kind) { return find(tree, node => node.kind === kind) }
export function inventoryEntries(tree) { return findAll(tree, node => node.props?.['data-plugin-entry'] !== undefined) }
export function inventoryDots(tree) { return findAll(tree, node => node.props?.role === 'img') }
export function inventoryCount(tree) { return find(tree, node => node.props?.['data-plugin-count'] !== undefined) }
export function inventoryLoaderEntry(tree) { return find(tree, node => node.props?.['data-loader-entry'] !== undefined) }
export function inventoryClick(node) { return node.props.onClick() }
export function inventoryChange(node, value) { return node.props.onChange({ currentTarget: { value } }) }
export function inventoryTick() { return Promise.resolve().then(() => Promise.resolve()).then(() => Promise.resolve()) }
export function inventoryStyleCount() {
  return document.querySelectorAll('style[data-plugin="@seekdeep-ai/seekdeep-client-ui-settings-plugin-inventory"]').length
}

export function makeInventoryPluginBench() {
  const ui = makeInventoryBench()
  const effects = [], entries = [], calls = []
  let result = { ok: true, value: { entries: [] } }
  let language = 'en'
  const pluginInventory = { list() { calls.push('list'); return Promise.resolve(result) } }
  const own = dispose => { effects.push(dispose); return dispose }
  const translate = key => language === 'zh' && key === 'tab' ? '插件列表' : en[key] ?? key
  const ctx = {
    effect(setup) { return own(setup()) },
    locale: { register() { return () => {} }, bind() { return translate } },
    remote: { pluginInventory }, 'remote.pluginInventory': pluginInventory,
  }
  ctx.slots = {
    inject(name, install) { return own(install()) },
    register(options, component) {
      const entry = { options, component }
      entries.push(entry)
      return () => entries.splice(entries.indexOf(entry), 1)
    },
  }
  return {
    ...ui, ctx, effects, entries, calls,
    setResult(value) { result = value }, setLocale(value) { language = value },
  }
}
export function inventoryPluginEntries(bench) { return bench.entries }
export function inventoryPluginInject(bench) { return bench.entries[0].options.inject() }
export function inventoryPluginLabel(bench) { return bench.entries[0].options.label() }
export function inventoryPluginSetLocale(bench, locale) { bench.setLocale(locale) }
export function inventoryPluginSetResult(bench, result) { bench.setResult(result) }
export function inventoryPluginCalls(bench) { return bench.calls }
export function inventoryPluginDispose(bench) {
  for (const dispose of bench.effects.splice(0).reverse()) dispose()
}
export function inventorySnapshot() { return SNAPSHOT }
"#)]
extern "C" {
    fn makeInventoryBench() -> JsValue;
    fn inventoryRender(bench: &JsValue, component: &Function) -> JsValue;
    fn inventorySetMode(bench: &JsValue, mode: &str, value: &JsValue);
    fn inventoryResolve(bench: &JsValue, value: &JsValue);
    fn inventoryReject(bench: &JsValue, value: &JsValue);
    fn inventoryCalls(bench: &JsValue) -> u32;
    fn inventoryUnmount(bench: &JsValue);
    fn inventoryText(tree: &JsValue) -> String;
    fn inventoryFindAria(tree: &JsValue, label: &str) -> JsValue;
    fn inventoryFindRole(tree: &JsValue, role: &str) -> JsValue;
    fn inventoryFindKind(tree: &JsValue, kind: &str) -> JsValue;
    fn inventoryEntries(tree: &JsValue) -> Array;
    fn inventoryDots(tree: &JsValue) -> Array;
    fn inventoryCount(tree: &JsValue) -> JsValue;
    fn inventoryLoaderEntry(tree: &JsValue) -> JsValue;
    fn inventoryClick(node: &JsValue) -> JsValue;
    fn inventoryChange(node: &JsValue, value: &str) -> JsValue;
    fn inventoryTick() -> Promise;
    fn inventoryStyleCount() -> u32;
    fn makeInventoryPluginBench() -> JsValue;
    fn inventoryPluginEntries(bench: &JsValue) -> Array;
    fn inventoryPluginInject(bench: &JsValue) -> JsValue;
    fn inventoryPluginLabel(bench: &JsValue) -> String;
    fn inventoryPluginSetLocale(bench: &JsValue, locale: &str);
    fn inventoryPluginSetResult(bench: &JsValue, result: &JsValue);
    fn inventoryPluginCalls(bench: &JsValue) -> Array;
    fn inventoryPluginDispose(bench: &JsValue);
    fn inventorySnapshot() -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    let direct = Reflect::get(value, &JsValue::from_str(key)).unwrap();
    if !direct.is_undefined() {
        return direct;
    }
    let props = Reflect::get(value, &JsValue::from_str("props")).unwrap_or(JsValue::UNDEFINED);
    Reflect::get(&props, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

fn component(bench: &JsValue) -> Function {
    configure_client_ui_settings_plugin_inventory(
        property(bench, "React"),
        property(bench, "primitives"),
    )
    .unwrap();
    exported_plugin_inventory_settings_tab_component()
        .unwrap()
        .dyn_into()
        .unwrap()
}

#[wasm_bindgen_test(async)]
async fn loading_catalog_filter_expansion_and_disabled_rows_are_live() {
    let bench = makeInventoryBench();
    let inventory_component = component(&bench);
    let loading = inventoryRender(&bench, &inventory_component);
    assert!(inventoryText(&loading).contains("Reading plugins…"));
    JsFuture::from(inventoryTick()).await.unwrap();
    assert_eq!(inventoryCalls(&bench), 1);
    inventoryResolve(&bench, &inventorySnapshot());
    JsFuture::from(inventoryTick()).await.unwrap();
    let ready = inventoryRender(&bench, &inventory_component);
    assert_eq!(inventoryEntries(&ready).length(), 7);
    assert_eq!(inventoryDots(&ready).length(), 6);
    assert_eq!(
        property(&inventoryCount(&ready), "data-plugin-count").as_f64(),
        Some(7.0)
    );
    assert_eq!(inventoryStyleCount(), 1);
    let active = inventoryFindAria(&ready, "hmr, Mounted, Enabled");
    assert_eq!(property(&active, "aria-expanded").as_bool(), Some(false));
    inventoryClick(&active);
    let expanded = inventoryRender(&bench, &inventory_component);
    assert_eq!(inventoryText(&inventoryLoaderEntry(&expanded)), "8a1b2c3d");
    assert!(inventoryText(&expanded).contains("Cordis status"));

    let search = inventoryFindKind(&expanded, "input");
    inventoryChange(&search, "disabled-entry");
    let disabled_only = inventoryRender(&bench, &inventory_component);
    assert_eq!(inventoryEntries(&disabled_only).length(), 1);
    assert!(inventoryLoaderEntry(&disabled_only).is_undefined());
    let disabled = inventoryFindAria(&disabled_only, "directory-picker-native, Disabled");
    inventoryClick(&disabled);
    let disabled_open = inventoryRender(&bench, &inventory_component);
    assert!(!inventoryText(&disabled_open).contains("Cordis status"));
    assert!(!inventoryText(&disabled_open).contains("Not mounted"));

    let disabled_search = inventoryFindKind(&disabled_open, "input");
    inventoryChange(&disabled_search, "cordis-plugin-hmr");
    let module_match = inventoryRender(&bench, &inventory_component);
    assert_eq!(inventoryEntries(&module_match).length(), 1);
    assert!(inventoryText(&module_match).contains("hmr"));
    inventoryChange(&inventoryFindKind(&module_match, "input"), "not-a-plugin");
    let empty_search = inventoryRender(&bench, &inventory_component);
    assert_eq!(inventoryEntries(&empty_search).length(), 0);
    assert!(inventoryText(&empty_search).contains("No matching plugins."));
}

#[wasm_bindgen_test(async)]
async fn generic_failure_retry_sync_throw_and_unmount_guards_are_live() {
    let bench = makeInventoryBench();
    let inventory_component = component(&bench);
    inventorySetMode(&bench, "reject", &JsValue::UNDEFINED);
    inventoryRender(&bench, &inventory_component);
    JsFuture::from(inventoryTick()).await.unwrap();
    let failed = inventoryRender(&bench, &inventory_component);
    assert_eq!(
        inventoryText(&inventoryFindRole(&failed, "alert")),
        "Plugins are temporarily unavailable."
    );
    assert!(!inventoryText(&failed).contains("private transport detail"));
    inventorySetMode(
        &bench,
        "resolved",
        &js_sys::JSON::parse(r#"{"entries":[]}"#).unwrap(),
    );
    let retry = inventoryFindKind(&failed, "button");
    inventoryClick(&retry);
    let retrying = inventoryRender(&bench, &inventory_component);
    assert!(inventoryText(&retrying).contains("Reading plugins…"));
    JsFuture::from(inventoryTick()).await.unwrap();
    let empty = inventoryRender(&bench, &inventory_component);
    assert!(inventoryText(&empty).contains("No plugins are available."));
    assert_eq!(inventoryCalls(&bench), 2);

    let sync = makeInventoryBench();
    let sync_component = component(&sync);
    inventorySetMode(&sync, "sync", &JsValue::UNDEFINED);
    inventoryRender(&sync, &sync_component);
    JsFuture::from(inventoryTick()).await.unwrap();
    assert!(inventoryFindRole(&inventoryRender(&sync, &sync_component), "alert").is_object());

    let pending = makeInventoryBench();
    let pending_component = component(&pending);
    inventoryRender(&pending, &pending_component);
    JsFuture::from(inventoryTick()).await.unwrap();
    inventoryUnmount(&pending);
    inventoryResolve(&pending, &inventorySnapshot());
    JsFuture::from(inventoryTick()).await.unwrap();

    let rejected = makeInventoryBench();
    let rejected_component = component(&rejected);
    inventoryRender(&rejected, &rejected_component);
    JsFuture::from(inventoryTick()).await.unwrap();
    inventoryUnmount(&rejected);
    inventoryReject(&rejected, &js_sys::Error::new("late failure").into());
    JsFuture::from(inventoryTick()).await.unwrap();
}

#[wasm_bindgen_test(async)]
async fn plugin_folds_remote_results_labels_and_reversible_slot_ownership() {
    let bench = makeInventoryPluginBench();
    configure_client_ui_settings_plugin_inventory(
        property(&bench, "React"),
        property(&bench, "primitives"),
    )
    .unwrap();
    apply_client_ui_settings_plugin_inventory(property(&bench, "ctx")).unwrap();
    assert_eq!(settings_plugin_inventory_inject().length(), 4);
    assert_eq!(inventoryPluginEntries(&bench).length(), 1);
    let options = property(&inventoryPluginEntries(&bench).get(0), "options");
    assert_eq!(
        property(&options, "name").as_string().as_deref(),
        Some("settings.plugins.tab")
    );
    assert_eq!(property(&options, "id").as_string().as_deref(), Some("all"));
    assert_eq!(property(&options, "order").as_f64(), Some(10.0));
    assert_eq!(inventoryPluginLabel(&bench), "Plugin list");
    inventoryPluginSetLocale(&bench, "zh");
    assert_eq!(inventoryPluginLabel(&bench), "插件列表");
    let injected = inventoryPluginInject(&bench);
    let list = property(&injected, "list").dyn_into::<Function>().unwrap();
    let snapshot = JsFuture::from(Promise::resolve(&list.call0(&injected).unwrap()))
        .await
        .unwrap();
    assert_eq!(Array::from(&property(&snapshot, "entries")).length(), 0);
    assert_eq!(inventoryPluginCalls(&bench).length(), 1);
    inventoryPluginSetResult(
        &bench,
        &js_sys::JSON::parse(
            r#"{"ok":false,"error":{"code":"REMOTE_ERROR","message":"unavailable"}}"#,
        )
        .unwrap(),
    );
    let error = JsFuture::from(Promise::resolve(&list.call0(&injected).unwrap()))
        .await
        .unwrap_err();
    assert!(
        property(&error, "message")
            .as_string()
            .is_some_and(|message| message
                .contains("pluginInventory.list failed: REMOTE_ERROR: unavailable"))
    );
    inventoryPluginDispose(&bench);
    assert_eq!(inventoryPluginEntries(&bench).length(), 0);
    apply_client_ui_settings_plugin_inventory(property(&bench, "ctx")).unwrap();
    assert_eq!(inventoryPluginEntries(&bench).length(), 1);
    inventoryPluginDispose(&bench);
    assert_eq!(inventoryPluginEntries(&bench).length(), 0);
}
