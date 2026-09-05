//! Live Rust/WASM model directory, resolver, dual-entry plugin, and composer selector parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_ui_model_selection::{
    apply_client_ui_model_selection, configure_client_ui_model_selection,
    exported_model_select_component, model_selection_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
class FakeNode {
  constructor(kind, props, children) {
    this.kind = kind; this.props = props ?? {}; this.children = children.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false); this.parentElement = null
    for (const child of this.children) if (child instanceof FakeNode) child.parentElement = this
  }
  focus() { document.activeElement = this }
  contains(target) { return target === this || this.children.some(child => child instanceof FakeNode && child.contains(target)) }
  closest() { return this }
}
globalThis.Node = FakeNode
const styles = [], documentListeners = new Map()
if (typeof globalThis.document === 'undefined') globalThis.document = {}
Object.assign(document, {
  currentScript: null, activeElement: null, body: new FakeNode('body', {}, []),
  querySelector(selector) { const match = /^style\[data-plugin=(.+)\]$/.exec(selector); if (!match) return null; const plugin = JSON.parse(match[1]); return styles.find(node => node.attributes['data-plugin'] === plugin) ?? null },
  querySelectorAll(selector) { const node = this.querySelector(selector); return node === null ? [] : [node] },
  createElement(kind) { return { kind, attributes: {}, textContent: '', setAttribute(name, value) { this.attributes[name] = value } } },
  head: { appendChild(node) { styles.push(node); return node } },
  addEventListener(name, listener) { const rows = documentListeners.get(name) ?? new Set(); rows.add(listener); documentListeners.set(name, rows) },
  removeEventListener(name, listener) { documentListeners.get(name)?.delete(listener) },
})
document.activeElement = document.body

const ZH = {
  'command.description': '选择本会话使用的模型', 'option.loadError': '目录加载失败：{message}',
  'trigger.fallback': '选择模型', 'trigger.selectAria': '选择模型', 'trigger.aria': '选择模型，当前 {model}',
  'trigger.ariaEffort': '选择模型，当前 {model}，推理等级 {effort}', 'menu.aria': '模型与推理等级',
  'menu.model': '模型', 'menu.effort': '推理等级', 'effort.providerDefault': 'Default',
  'status.loading': '正在刷新模型列表…', 'error.action': '模型操作失败：{message}', 'action.reload': '重新加载',
  'warning.groupLoad': '{name} 加载失败：{message}', 'empty.models': '没有可用的模型。',
  'blocked.composer': '当前模型不可用，请先选择模型', 'empty.efforts': '当前模型未提供推理等级。', retry: '重试',
}
function t(key, values = {}) { return (ZH[key] ?? key).replace(/\{([^}]+)\}/g, (_, field) => String(values[field])) }

const GROUPS = [{ id: 'deepseek-official', name: 'DeepSeek', models: [
  { id: 'deepseek-v4-flash', name: 'DeepSeek-V4-Flash', reasoning: { efforts: [{ id: 'off', name: 'Off' }, { id: 'high', name: 'High' }, { id: 'max', name: 'Max', description: 'Largest budget' }], defaultEffort: 'high' } },
  { id: 'deepseek-v4-pro', name: 'DeepSeek-V4-Pro', reasoning: { efforts: [{ id: 'off', name: 'Off' }, { id: 'high', name: 'High' }, { id: 'max', name: 'Max' }], defaultEffort: 'high' } },
] }]
function selection(model = 'deepseek-v4-flash', effort) { return { provider: 'deepseek-official', model, ...(effort === undefined ? {} : { reasoningEffort: effort }) } }
function state(overrides = {}) { return { current: selection(), routable: true, groups: GROUPS, failures: [], status: 'ready', error: null, ...overrides } }

function hookRuntime() {
  const states = [], refs = [], effects = []; let stateCursor = 0, refCursor = 0, effectCursor = 0
  const Fragment = Symbol('Fragment')
  const React = {
    Fragment,
    createElement(kind, props, ...children) {
      const node = new FakeNode(kind, props ?? {}, children)
      if (props?.ref && typeof props.ref === 'object') props.ref.current = node
      else if (typeof props?.ref === 'function') props.ref(node)
      return node
    },
    useState(initial) { const index = stateCursor++; if (!(index in states)) states[index] = typeof initial === 'function' ? initial() : initial; const set = value => { states[index] = typeof value === 'function' ? value(states[index]) : value }; return [states[index], set] },
    useRef(initial) { const index = refCursor++; if (!(index in refs)) refs[index] = { current: initial }; return refs[index] },
    useEffect(run, deps) { const index = effectCursor++; const old = effects[index]; const changed = old === undefined || deps.length !== old.deps.length || deps.some((value, i) => !Object.is(value, old.deps[i])); if (changed) { old?.cleanup?.(); const cleanup = run(); effects[index] = { deps: [...deps], cleanup: typeof cleanup === 'function' ? cleanup : undefined } } },
    useSyncExternalStore(_subscribe, getSnapshot) { return getSnapshot() },
    useId() { return 'model-id' },
  }
  return { React, render(component, props) { stateCursor = 0; refCursor = 0; effectCursor = 0; return component(props) }, dispose() { for (const effect of effects.reverse()) effect?.cleanup?.() } }
}
function textOf(node) { if (node === null || node === undefined || node === false) return ''; if (typeof node === 'string' || typeof node === 'number') return String(node); if (Array.isArray(node)) return node.map(textOf).join(''); return (node.children ?? []).map(textOf).join('') }
function all(node, predicate, rows = []) { if (node === null || node === undefined || node === false || typeof node === 'string' || typeof node === 'number') return rows; if (!Array.isArray(node) && predicate(node)) rows.push(node); for (const child of Array.isArray(node) ? node : node.children ?? []) all(child, predicate, rows); return rows }
function one(node, predicate) { return all(node, predicate)[0] }

function scope() { const effects = []; return { effect(setup) { const dispose = setup(); effects.push(dispose); return dispose }, dispose() { for (const dispose of effects.splice(0).reverse()) dispose() } } }
export function makeModelPluginBench() {
  const hooks = hookRuntime(), rootEffects = [], scopes = new Map(), addressed = new Set(), events = new Map(), entries = [], locales = [], blocks = new Map(), calls = { models: 0, select: 0 }
  let contribution, current = selection(), routable = true, modelFailure, selectFailure
  const services = new Map()
  const sessionsApi = {
    models({ sessionId }) { calls.models++; return Promise.resolve(modelFailure ? { result: { ok: false, error: { code: 'internal', message: modelFailure } } } : { result: { ok: true, value: { current, routable, groups: GROUPS, failures: [] } } }) },
    selectModel(payload) { calls.select++; if (selectFailure) return Promise.resolve({ result: { ok: false, error: { code: 'model-unavailable', message: selectFailure } } }); current = { provider: payload.provider, model: payload.model, ...(payload.reasoningEffort === undefined ? {} : { reasoningEffort: payload.reasoningEffort }) }; return Promise.resolve({ result: { ok: true, value: { selected: current } } }) },
  }
  const own = dispose => { rootEffects.push(dispose); return dispose }
  const ctx = {
    commandUi: { register(value) { contribution = value; return () => { contribution = undefined } } },
    connection: { api: { sessions: sessionsApi } },
    locale: { register(namespace, dictionaries) { const row = { namespace, dictionaries }; locales.push(row); return () => locales.splice(locales.indexOf(row), 1) }, bind() { return t } },
    sessions: { scope(id) { return scopes.get(id) }, subagentAddress(id) { return addressed.has(id) ? { parentSessionId: 'parent', childSessionId: id, mode: 'continuable' } : undefined } },
    slots: { inject(_name, install) { return own(install()) }, register(options, component) { const row = { options, component }; entries.push(row); return () => entries.splice(entries.indexOf(row), 1) } },
    remote: { $on(name, listener) { events.set('remote:' + name, listener); return () => events.delete('remote:' + name) } },
    reflect: { provide(name, value) { services.set(name, value); return () => services.delete(name) } },
    on(name, listener) { events.set(name, listener); return () => events.delete(name) },
    get(name) { return name === 'conversation' ? { blocks: { set(id, value) { blocks.set(id, value) } } } : services.get(name) },
    effect(setup) { return own(setup()) },
  }
  return { hooks, React: hooks.React, primitives: { IconCheckOutline16: 'IconCheckOutline16', IconChevronDownOutline14: 'IconChevronDownOutline14', IconChevronRightOutline14: 'IconChevronRightOutline14', IconWarningOutline16: 'IconWarningOutline16', Toast: 'Toast' }, ctx, rootEffects, scopes, addressed, events, entries, locales, blocks, calls, services,
    get contribution() { return contribution }, mint(id) { const value = scope(); scopes.set(id, value); return value }, setCurrent(value) { current = value }, get current() { return current }, setRoutable(value) { routable = value }, setModelFailure(value) { modelFailure = value }, setSelectFailure(value) { selectFailure = value },
  }
}
export function modelPluginContribution(bench) { return bench.contribution }
export function modelPluginEntries(bench) { return bench.entries }
export function modelPluginCalls(bench) { return bench.calls }
export function modelPluginMint(bench, id) { return bench.mint(id) }
export function modelPluginAddress(bench, id) { bench.addressed.add(id) }
export function modelPluginSetCurrent(bench, value) { bench.setCurrent(value) }
export function modelPluginSetRoutable(bench, value) { bench.setRoutable(value) }
export function modelPluginBlock(bench, id) { return bench.blocks.get(id) }
export function modelPluginDispatch(bench, name, first, second) { bench.events.get(name)?.(first, second) }
export function modelPluginDispose(bench) { bench.hooks.dispose(); for (const dispose of bench.rootEffects.splice(0).reverse()) dispose() }
export function modelScopeDispose(scope) { scope.dispose() }
export function modelSession(id) { return { sessionId: id } }
export function modelSelection(model, effort) { return selection(model, effort) }
export function modelTick() { return Promise.resolve().then(() => Promise.resolve()).then(() => Promise.resolve()) }
export function modelStyleCount() { return styles.filter(node => node.attributes['data-plugin'] === '@seekdeep-ai/seekdeep-client-ui-model-selection').length }

export function makeModelSelectBench() {
  const hooks = hookRuntime(), loads = [], selections = [], listeners = new Set(); let accepted = true
  const snapshot = state()
  const store = { getSnapshot() { return snapshot }, subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener) } }
  const props = { locked: false, available: true, directory: store, load() { loads.push(true) }, select(value) { selections.push(value); if (accepted) snapshot.current = value; else { snapshot.status = 'error'; snapshot.error = 'model-unavailable: session already contains images' } return Promise.resolve(accepted) }, t }
  return { hooks, React: hooks.React, primitives: { IconCheckOutline16: 'IconCheckOutline16', IconChevronDownOutline14: 'IconChevronDownOutline14', IconChevronRightOutline14: 'IconChevronRightOutline14', IconWarningOutline16: 'IconWarningOutline16', Toast: 'Toast' }, snapshot, props, loads, selections, setAccepted(value) { accepted = value } }
}
export function modelSelectRender(bench, component) { return bench.hooks.render(component, bench.props) }
export function modelSelectSnapshot(bench) { return bench.snapshot }
export function modelSelectLoads(bench) { return bench.loads }
export function modelSelectSelections(bench) { return bench.selections }
export function modelSelectSetAccepted(bench, value) { bench.setAccepted(value) }
export function modelSelectTrigger(tree) { return one(tree, node => node.kind === 'button' && node.props?.['aria-haspopup'] === 'menu') }
export function modelSelectMenu(tree) { return one(tree, node => node.props?.role === 'menu') }
export function modelSelectItem(tree, text) { return one(tree, node => (node.props?.role === 'menuitem' || node.props?.role === 'menuitemradio') && textOf(node).includes(text)) }
export function modelSelectButton(tree, text) { return one(tree, node => node.kind === 'button' && textOf(node).includes(text)) }
export function modelSelectRadios(tree) { return all(tree, node => node.props?.role === 'menuitemradio') }
export function modelSelectToast(tree) { return one(tree, node => node.kind === 'Toast') }
export function modelSelectText(tree) { return textOf(tree) }
export function modelSelectClick(node) { return node.props.onClick() }
export function modelSelectProperty(node, key) { return node?.props?.[key] }
"#)]
extern "C" {
    fn makeModelPluginBench() -> JsValue;
    fn modelPluginContribution(bench: &JsValue) -> JsValue;
    fn modelPluginEntries(bench: &JsValue) -> Array;
    fn modelPluginCalls(bench: &JsValue) -> JsValue;
    fn modelPluginMint(bench: &JsValue, id: &str) -> JsValue;
    fn modelPluginAddress(bench: &JsValue, id: &str);
    fn modelPluginSetCurrent(bench: &JsValue, value: &JsValue);
    fn modelPluginSetRoutable(bench: &JsValue, value: bool);
    fn modelPluginBlock(bench: &JsValue, id: &str) -> JsValue;
    fn modelPluginDispatch(bench: &JsValue, name: &str, first: &JsValue, second: &JsValue);
    fn modelPluginDispose(bench: &JsValue);
    fn modelScopeDispose(scope: &JsValue);
    fn modelSession(id: &str) -> JsValue;
    fn modelSelection(model: &str, effort: &JsValue) -> JsValue;
    fn modelTick() -> Promise;
    fn modelStyleCount() -> u32;
    fn makeModelSelectBench() -> JsValue;
    fn modelSelectRender(bench: &JsValue, component: &Function) -> JsValue;
    fn modelSelectSnapshot(bench: &JsValue) -> JsValue;
    fn modelSelectLoads(bench: &JsValue) -> Array;
    fn modelSelectSelections(bench: &JsValue) -> Array;
    fn modelSelectSetAccepted(bench: &JsValue, value: bool);
    fn modelSelectTrigger(tree: &JsValue) -> JsValue;
    fn modelSelectMenu(tree: &JsValue) -> JsValue;
    fn modelSelectItem(tree: &JsValue, text: &str) -> JsValue;
    fn modelSelectButton(tree: &JsValue, text: &str) -> JsValue;
    fn modelSelectRadios(tree: &JsValue) -> Array;
    fn modelSelectToast(tree: &JsValue) -> JsValue;
    fn modelSelectText(tree: &JsValue) -> String;
    fn modelSelectClick(node: &JsValue) -> JsValue;
    fn modelSelectProperty(node: &JsValue, key: &str) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    let direct = Reflect::get(value, &JsValue::from_str(key)).unwrap();
    if !direct.is_undefined() {
        return direct;
    }
    let props = Reflect::get(value, &JsValue::from_str("props")).unwrap_or(JsValue::UNDEFINED);
    Reflect::get(&props, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}
fn call(value: &JsValue, key: &str) -> Function {
    property(value, key).dyn_into().unwrap()
}
fn configure(bench: &JsValue) {
    configure_client_ui_model_selection(property(bench, "React"), property(bench, "primitives"))
        .unwrap();
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn resolver_command_seat_shared_state_blocks_scopes_and_addressing_are_live() {
    let bench = makeModelPluginBench();
    configure(&bench);
    apply_client_ui_model_selection(property(&bench, "ctx")).unwrap();
    assert_eq!(
        model_selection_inject()
            .iter()
            .map(|v| v.as_string().unwrap())
            .collect::<Vec<_>>(),
        [
            "commandUi",
            "connection",
            "locale",
            "sessions",
            "slots",
            "remote"
        ]
    );
    assert_eq!(modelStyleCount(), 1);
    assert_eq!(modelPluginEntries(&bench).length(), 1);
    let scope = modelPluginMint(&bench, "s1");
    let seat = modelPluginEntries(&bench).get(0);
    let options = property(&seat, "options");
    let face = call(&options, "inject")
        .call1(&options, &JsValue::from_str("s1"))
        .unwrap();
    let face2 = call(&options, "inject")
        .call1(&options, &JsValue::from_str("s1"))
        .unwrap();
    assert!(Object::is(
        &property(&face, "directory"),
        &property(&face2, "directory")
    ));
    modelPluginMint(&bench, "s2");
    let other = call(&options, "inject")
        .call1(&options, &JsValue::from_str("s2"))
        .unwrap();
    assert!(!Object::is(
        &property(&face, "directory"),
        &property(&other, "directory")
    ));
    let selected = call(&face, "select")
        .call1(
            &face,
            &modelSelection("deepseek-v4-pro", &JsValue::from_str("max")),
        )
        .unwrap();
    assert_eq!(
        JsFuture::from(Promise::resolve(&selected))
            .await
            .unwrap()
            .as_bool(),
        Some(true)
    );
    let current = property(
        &call(&property(&face, "directory"), "getSnapshot")
            .call0(&property(&face, "directory"))
            .unwrap(),
        "current",
    );
    assert_eq!(
        property(&current, "model").as_string().as_deref(),
        Some("deepseek-v4-pro")
    );
    let contribution = modelPluginContribution(&bench);
    let ui = property(&contribution, "ui");
    let session = modelSession("s1");
    let rows = call(&ui, "options")
        .call2(&ui, &session, &JsValue::UNDEFINED)
        .unwrap();
    let rows = Array::from(&JsFuture::from(Promise::resolve(&rows)).await.unwrap());
    assert_eq!(rows.length(), 2);
    assert_eq!(property(&rows.get(1), "active").as_bool(), Some(true));
    let pick = call(&ui, "onSelect")
        .call2(&ui, &rows.get(0), &session)
        .unwrap();
    JsFuture::from(Promise::resolve(&pick)).await.unwrap();
    let current = property(
        &call(&property(&face, "directory"), "getSnapshot")
            .call0(&property(&face, "directory"))
            .unwrap(),
        "current",
    );
    assert_eq!(
        property(&current, "model").as_string().as_deref(),
        Some("deepseek-v4-flash")
    );
    modelPluginSetCurrent(&bench, &modelSelection("unlisted", &JsValue::UNDEFINED));
    modelPluginDispatch(
        &bench,
        "remote:llm/adapters-updated",
        &JsValue::UNDEFINED,
        &JsValue::UNDEFINED,
    );
    JsFuture::from(modelTick()).await.unwrap();
    assert!(modelPluginBlock(&bench, "s1").is_undefined());
    modelPluginSetRoutable(&bench, false);
    modelPluginDispatch(
        &bench,
        "remote:llm/adapters-updated",
        &JsValue::UNDEFINED,
        &JsValue::UNDEFINED,
    );
    JsFuture::from(modelTick()).await.unwrap();
    assert!(!modelPluginBlock(&bench, "s1").is_undefined());
    modelPluginSetRoutable(&bench, true);
    modelPluginDispatch(
        &bench,
        "remote:settings/document-updated",
        &JsValue::from_str("x"),
        &JsValue::from_f64(1.0),
    );
    JsFuture::from(modelTick()).await.unwrap();
    assert!(modelPluginBlock(&bench, "s1").is_undefined());
    modelPluginSetCurrent(
        &bench,
        &modelSelection("deepseek-v4-pro", &JsValue::UNDEFINED),
    );
    modelPluginDispatch(
        &bench,
        "connection/reset",
        &JsValue::UNDEFINED,
        &JsValue::UNDEFINED,
    );
    JsFuture::from(modelTick()).await.unwrap();
    let current = property(
        &call(&property(&face, "directory"), "getSnapshot")
            .call0(&property(&face, "directory"))
            .unwrap(),
        "current",
    );
    assert_eq!(
        property(&current, "model").as_string().as_deref(),
        Some("deepseek-v4-pro")
    );
    modelScopeDispose(&scope);
    assert!(modelPluginBlock(&bench, "s1").is_undefined());
    modelPluginMint(&bench, "s1");
    let reborn = call(&options, "inject")
        .call1(&options, &JsValue::from_str("s1"))
        .unwrap();
    assert!(!Object::is(
        &property(&face, "directory"),
        &property(&reborn, "directory")
    ));
    modelPluginMint(&bench, "child");
    modelPluginAddress(&bench, "child");
    assert_eq!(
        call(&contribution, "available")
            .call1(&contribution, &modelSession("child"))
            .unwrap()
            .as_bool(),
        Some(false)
    );
    let child = call(&options, "inject")
        .call1(&options, &JsValue::from_str("child"))
        .unwrap();
    assert_eq!(property(&child, "available").as_bool(), Some(false));
    let calls_before = property(&modelPluginCalls(&bench), "models").as_f64();
    call(&child, "load").call0(&child).unwrap();
    let refused = call(&child, "select")
        .call1(
            &child,
            &modelSelection("deepseek-v4-pro", &JsValue::UNDEFINED),
        )
        .unwrap();
    assert_eq!(
        JsFuture::from(Promise::resolve(&refused))
            .await
            .unwrap()
            .as_bool(),
        Some(false)
    );
    JsFuture::from(modelTick()).await.unwrap();
    assert_eq!(
        property(&modelPluginCalls(&bench), "models").as_f64(),
        calls_before
    );
    assert!(
        call(&options, "inject")
            .call1(&options, &JsValue::from_str("ghost"))
            .is_err()
    );
    assert!(
        property(&modelPluginCalls(&bench), "models")
            .as_f64()
            .unwrap()
            >= 3.0
    );
    modelPluginDispose(&bench);
    assert!(modelPluginContribution(&bench).is_undefined());
}

#[wasm_bindgen_test(async)]
async fn model_select_efforts_fallback_availability_and_default_rows_are_live() {
    let bench = makeModelSelectBench();
    configure(&bench);
    let component = exported_model_select_component()
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    let mut tree = modelSelectRender(&bench, &component);
    assert_eq!(modelSelectLoads(&bench).length(), 1);
    let trigger = modelSelectTrigger(&tree);
    assert_eq!(
        modelSelectProperty(&trigger, "aria-label")
            .as_string()
            .as_deref(),
        Some("选择模型，当前 DeepSeek-V4-Flash，推理等级 High")
    );
    modelSelectClick(&trigger);
    tree = modelSelectRender(&bench, &component);
    modelSelectClick(&modelSelectItem(&tree, "推理等级"));
    tree = modelSelectRender(&bench, &component);
    assert_eq!(modelSelectRadios(&tree).length(), 3);
    assert!(modelSelectText(&tree).contains("MaxLargest budget"));
    modelSelectClick(&modelSelectItem(&tree, "Max"));
    JsFuture::from(modelTick()).await.unwrap();
    assert_eq!(
        property(&modelSelectSelections(&bench).get(0), "reasoningEffort")
            .as_string()
            .as_deref(),
        Some("max")
    );
    tree = modelSelectRender(&bench, &component);
    assert!(
        modelSelectProperty(&modelSelectTrigger(&tree), "aria-label")
            .as_string()
            .unwrap()
            .contains("推理等级 Max")
    );
    let snapshot = modelSelectSnapshot(&bench);
    Reflect::set(
        &snapshot,
        &JsValue::from_str("current"),
        &modelSelection("removed-model", &JsValue::UNDEFINED),
    )
    .unwrap();
    tree = modelSelectRender(&bench, &component);
    assert_eq!(
        modelSelectProperty(&modelSelectTrigger(&tree), "aria-label")
            .as_string()
            .as_deref(),
        Some("选择模型")
    );
    Reflect::set(
        &property(&bench, "props"),
        &JsValue::from_str("available"),
        &JsValue::FALSE,
    )
    .unwrap();
    assert!(modelSelectRender(&bench, &component).is_null());
    let unavailable = makeModelSelectBench();
    Reflect::set(
        &property(&unavailable, "props"),
        &JsValue::from_str("available"),
        &JsValue::FALSE,
    )
    .unwrap();
    assert!(modelSelectRender(&unavailable, &component).is_null());
    assert_eq!(modelSelectLoads(&unavailable).length(), 0);
}

#[wasm_bindgen_test(async)]
async fn model_select_rejection_toast_and_provider_default_are_live() {
    let bench = makeModelSelectBench();
    configure(&bench);
    let component = exported_model_select_component()
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    let snapshot = modelSelectSnapshot(&bench);
    Reflect::set(&snapshot,&JsValue::from_str("groups"),&js_sys::JSON::parse(r#"[{"id":"provider","name":"Provider","models":[{"id":"model","name":"Model","reasoning":{"efforts":[{"id":"standard","name":"Standard"}]}}]}]"#).unwrap()).unwrap();
    Reflect::set(
        &snapshot,
        &JsValue::from_str("current"),
        &js_sys::JSON::parse(r#"{"provider":"provider","model":"model"}"#).unwrap(),
    )
    .unwrap();
    let mut tree = modelSelectRender(&bench, &component);
    assert!(
        modelSelectProperty(&modelSelectTrigger(&tree), "aria-label")
            .as_string()
            .unwrap()
            .contains("Default")
    );
    modelSelectClick(&modelSelectTrigger(&tree));
    tree = modelSelectRender(&bench, &component);
    modelSelectClick(&modelSelectItem(&tree, "推理等级"));
    tree = modelSelectRender(&bench, &component);
    assert_eq!(modelSelectRadios(&tree).length(), 2);
    modelSelectSetAccepted(&bench, false);
    modelSelectClick(&modelSelectRadios(&tree).get(1));
    JsFuture::from(modelTick()).await.unwrap();
    tree = modelSelectRender(&bench, &component);
    assert!(!modelSelectToast(&tree).is_undefined());
    assert!(
        modelSelectProperty(&modelSelectToast(&tree), "text")
            .as_string()
            .unwrap()
            .contains("模型操作失败：model-unavailable")
    );
    assert_eq!(modelSelectSelections(&bench).length(), 1);
}

#[wasm_bindgen_test]
fn model_select_load_error_provider_warning_retry_and_empty_states_are_live() {
    let bench = makeModelSelectBench();
    configure(&bench);
    let component = exported_model_select_component()
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    let snapshot = modelSelectSnapshot(&bench);
    Reflect::set(&snapshot, &JsValue::from_str("current"), &JsValue::NULL).unwrap();
    Reflect::set(&snapshot, &JsValue::from_str("groups"), &Array::new()).unwrap();
    Reflect::set(
        &snapshot,
        &JsValue::from_str("status"),
        &JsValue::from_str("loading"),
    )
    .unwrap();
    let mut tree = modelSelectRender(&bench, &component);
    modelSelectClick(&modelSelectTrigger(&tree));
    tree = modelSelectRender(&bench, &component);
    modelSelectClick(&modelSelectItem(&tree, "模型"));
    tree = modelSelectRender(&bench, &component);
    assert!(modelSelectText(&tree).contains("正在刷新模型列表"));
    Reflect::set(
        &snapshot,
        &JsValue::from_str("status"),
        &JsValue::from_str("error"),
    )
    .unwrap();
    Reflect::set(
        &snapshot,
        &JsValue::from_str("error"),
        &JsValue::from_str("offline"),
    )
    .unwrap();
    tree = modelSelectRender(&bench, &component);
    assert!(modelSelectText(&tree).contains("模型操作失败：offline"));
    modelSelectClick(&modelSelectButton(&tree, "重试"));
    assert_eq!(modelSelectLoads(&bench).length(), 3);
    Reflect::set(
        &snapshot,
        &JsValue::from_str("status"),
        &JsValue::from_str("ready"),
    )
    .unwrap();
    Reflect::set(&snapshot, &JsValue::from_str("error"), &JsValue::NULL).unwrap();
    Reflect::set(
        &snapshot,
        &JsValue::from_str("failures"),
        &js_sys::JSON::parse(r#"[{"id":"broken","name":"Broken Provider","message":"down"}]"#)
            .unwrap(),
    )
    .unwrap();
    tree = modelSelectRender(&bench, &component);
    assert!(modelSelectText(&tree).contains("Broken Provider 加载失败：down"));
    assert!(modelSelectText(&tree).contains("没有可用的模型"));
}
