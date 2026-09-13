//! Live Rust/WASM subagent source, slots, catalog, and read-only composer parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Promise, Reflect};
use seekdeep_client_ui_subagent::{
    apply_client_ui_subagent, configure_client_ui_subagent,
    exported_subagent_catalog_action_component, exported_subagent_read_only_composer_component,
    subagent_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
class FakeNode {
  constructor(kind, props, children) {
    this.kind = kind
    this.props = props ?? {}
    this.children = children.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false)
    this.parentElement = null
    for (const child of this.children) {
      if (child instanceof FakeNode) child.parentElement = this
    }
  }
  focus() { document.activeElement = this }
  contains(target) {
    if (target === this) return true
    return this.children.some(child => child instanceof FakeNode && child.contains(target))
  }
  querySelectorAll() {
    const rows = []
    const visit = node => {
      if (!(node instanceof FakeNode)) return
      if (node.props.role === 'treeitem' && node.props['aria-disabled'] !== true) rows.push(node)
      for (const child of node.children) visit(child)
    }
    visit(this)
    return rows
  }
}
globalThis.Node = FakeNode

const styles = []
const documentListeners = new Map()
if (typeof globalThis.document === 'undefined') globalThis.document = {}
Object.assign(globalThis.document, {
  currentScript: null,
  activeElement: null,
  body: new FakeNode('body', {}, []),
  querySelector(selector) {
    const match = /^style\[data-plugin=(.+)\]$/.exec(selector)
    if (match === null) return null
    const plugin = JSON.parse(match[1])
    return styles.find(node => node.attributes['data-plugin'] === plugin) ?? null
  },
  querySelectorAll(selector) {
    const node = this.querySelector(selector)
    return node === null ? [] : [node]
  },
  createElement(kind) {
    return { kind, attributes: {}, textContent: '', setAttribute(name, value) { this.attributes[name] = value } }
  },
  head: { appendChild(node) { styles.push(node); return node } },
  addEventListener(name, listener) {
    const values = documentListeners.get(name) ?? new Set(); values.add(listener); documentListeners.set(name, values)
  },
  removeEventListener(name, listener) { documentListeners.get(name)?.delete(listener) },
})
document.activeElement = document.body

let fakeNow = 2_000_000_000_000
const originalDateNow = Date.now
Date.now = () => fakeNow
const timers = new Map()
let nextTimer = 1
globalThis.setInterval = callback => { const id = nextTimer++; timers.set(id, callback); return id }
globalThis.clearInterval = id => { timers.delete(id) }

const ZH = {
  'diagnostic.corrupt': '会话记录损坏', 'diagnostic.unsupported': '子代理记录版本不受支持',
  'diagnostic.unavailable': '会话记录暂不可用', 'duration.seconds': '{seconds}秒',
  'duration.minutes': '{minutes}分{seconds}秒', 'duration.hours': '{hours}小时{minutes}分{seconds}秒',
  'duration.days': '{days}天', 'duration.daysHours': '{days}天{hours}小时',
  'duration.months': '约{months}个月', 'duration.monthsDays': '约{months}个月{days}天',
  'duration.years': '约{years}年', 'duration.yearsMonths': '约{years}年{months}个月',
  'duration.exactDays': '{days}天{hours}小时{minutes}分{seconds}秒',
  'duration.exactTitle': '总活跃耗时：{duration}', 'loading.label': '正在加载子代理…',
  'loading.aria': '正在加载子代理', 'load.error': '无法加载子代理', 'retry': '重试',
  'mode.oneShot': '一次性', 'mode.continuable': '可继续', 'activity.running': '正在运行',
  'activity.inactive': '当前未运行', 'branch.collapse': '收起 {label} 的下级子代理',
  'branch.expand': '展开 {label} 的下级子代理', 'count.total.one': '{count} 个子代理',
  'count.total.other': '{count} 个子代理', 'count.running.one': '{count} 个子代理，正在运行',
  'count.running.other': '{count} 个子代理，正在运行', 'tree.aria': '子代理会话',
  'readonly.oneShot.title': '一次性子代理记录', 'readonly.title': '此子代理暂时只读',
  'readonly.oneShot.body': '一次性任务不支持后续消息，可在这里查看完整执行记录。',
  'readonly.body': '父会话当前不在线，重新打开父会话后即可继续发送消息。',
}
function t(key, values = {}) {
  return (ZH[key] ?? key).replace(/\{([^}]+)\}/g, (_, field) => String(values[field]))
}

function hookRuntime() {
  const state = [], refs = [], effects = []
  let stateCursor = 0, refCursor = 0, effectCursor = 0
  const Fragment = Symbol('Fragment')
  const React = {
    Fragment,
    createElement(kind, props, ...children) {
      if (kind === 'StateDot') kind = 'span', props = { ...props, 'data-state': props.state }
      if (typeof kind === 'string' && kind.startsWith('Icon')) kind = 'svg'
      const node = new FakeNode(kind, props ?? {}, children)
      if (props?.ref && typeof props.ref === 'object') props.ref.current = node
      return node
    },
    useState(initial) {
      const index = stateCursor++
      if (!(index in state)) state[index] = typeof initial === 'function' ? initial() : initial
      const set = value => { state[index] = typeof value === 'function' ? value(state[index]) : value }
      return [state[index], set]
    },
    useRef(initial) {
      const index = refCursor++
      if (!(index in refs)) refs[index] = { current: initial }
      return refs[index]
    },
    useEffect(run, deps) {
      const index = effectCursor++
      const previous = effects[index]
      const changed = previous === undefined || deps.length !== previous.deps.length
        || deps.some((value, i) => !Object.is(value, previous.deps[i]))
      if (changed) {
        previous?.cleanup?.()
        const cleanup = run()
        effects[index] = { deps: [...deps], cleanup: typeof cleanup === 'function' ? cleanup : undefined }
      }
    },
  }
  return {
    React,
    render(component, props) { stateCursor = 0; refCursor = 0; effectCursor = 0; return component(props) },
    dispose() { for (const effect of effects.reverse()) effect?.cleanup?.() },
  }
}
function textOf(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(textOf).join('')
  return (node.children ?? []).map(textOf).join('')
}
function all(node, predicate, rows = []) {
  if (node === null || node === undefined || node === false) return rows
  if (typeof node === 'string' || typeof node === 'number') return rows
  if (!Array.isArray(node) && predicate(node)) rows.push(node)
  for (const child of Array.isArray(node) ? node : node.children ?? []) all(child, predicate, rows)
  return rows
}
function one(node, predicate) { return all(node, predicate)[0] }

function familyState() {
  const byId = {
    parent: { id: 'parent', displayTitle: 'parent', running: true, blank: false, updatedAt: 0 },
    child: { id: 'child', title: '正在扫描项目文件', displayTitle: 'worker', parentId: 'parent', origin: 'subagent', running: true, blank: false, updatedAt: fakeNow },
    'child-2': { id: 'child-2', displayTitle: 'worker-2', parentId: 'parent', origin: 'subagent', running: false, blank: false, updatedAt: fakeNow },
    stopped: { id: 'stopped', displayTitle: 'worker-stopped', parentId: 'parent', running: false, blank: false, updatedAt: 0 },
    foreign: { id: 'foreign', displayTitle: 'worker-foreign', parentId: 'other', origin: 'subagent', running: true, blank: false, updatedAt: 0 },
    scout: { id: 'scout', displayTitle: 'scout', parentId: 'parent', running: true, blank: false, updatedAt: 0 },
    candidate2: { id: 'candidate2', displayTitle: 'worker-2', parentId: 'parent', running: true, blank: false, updatedAt: 0 },
  }
  const root = {
    entries: [
      { kind: 'child', id: 'child', mode: 'continuable', label: 'worker', activity: 'running', hasChildren: true },
      { kind: 'child', id: 'child-2', mode: 'one-shot', label: 'reviewer', activity: 'inactive', hasChildren: false },
      { kind: 'diagnostic', id: 'bad', reason: 'corrupt' },
    ], parentAvailable: true, state: 'ready', error: null,
  }
  return { ids: Object.keys(byId), byId, current: 'parent', phase: 'ready', subagentsByParent: { parent: root }, jobsBySession: {}, currentAddress: undefined }
}

export function makeSubagentBench() {
  document.activeElement = document.body
  const hooks = hookRuntime(), effects = [], entries = [], locale = [], calls = [], translations = [], listeners = new Set()
  let source
  const state = familyState()
  const sessions = {
    list: { getSnapshot: () => state, subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener) } },
    openSubagent(address) { calls.push({ method: 'openSubagent', args: [address] }) },
    refreshSubagents(parent) { calls.push({ method: 'refreshSubagents', args: [parent] }); return Promise.resolve() },
    setSubagentCatalogOpen(parent, open) { calls.push({ method: 'setSubagentCatalogOpen', args: [parent, open] }) },
  }
  const own = dispose => { effects.push(dispose); return dispose }
  const ctx = {
    inputTriggers: { registerSource(value) { if (source !== undefined) throw new Error('already registered'); source = value; return () => { source = undefined } } },
    sessions,
    locale: {
      register(namespace, dictionaries) { const row = { namespace, dictionaries }; locale.push(row); return () => locale.splice(locale.indexOf(row), 1) },
    },
    slots: {
      inject(name, install) { return own(install()) },
      register(options, component) { const row = { options, component }; entries.push(row); return () => entries.splice(entries.indexOf(row), 1) },
    },
    effect(setup) { return own(setup()) },
  }
  const props = {
    sessionId: 'parent', useSessions: select => select(state),
    openChild: address => sessions.openSubagent(address),
    refresh: parent => sessions.refreshSubagents(parent),
    setCatalogOpen: (parent, open) => sessions.setSubagentCatalogOpen(parent, open),
    t(key, values = {}) { translations.push({ key, values }); return t(key, values) },
  }
  return { hooks, React: hooks.React, primitives: {
    IconChevronDownOutline14: 'IconChevronDownOutline14', IconChevronRightOutline14: 'IconChevronRightOutline14',
    IconRefreshOutline14: 'IconRefreshOutline14', StateDot: 'StateDot',
  }, ctx, state, sessions, props, effects, entries, locale, calls, translations, listeners,
    get source() { return source },
  }
}
export function subagentSource(bench) { return bench.source }
export function subagentEntries(bench) { return bench.entries }
export function subagentLocale(bench) { return bench.locale }
export function subagentCalls(bench) { return bench.calls }
export function subagentTranslations(bench) { return bench.translations }
export function subagentState(bench) { return bench.state }
export function subagentProps(bench) { return bench.props }
export function subagentSession(id) { return { sessionId: id } }
export function subagentRequest(query) { return { query, position: 'inline', signal: new AbortController().signal } }
export function subagentNotify(bench) { for (const listener of [...bench.listeners]) listener() }
export function subagentListenerCount(bench) { return bench.listeners.size }
export function subagentDispose(bench) { bench.hooks.dispose(); for (const dispose of bench.effects.splice(0).reverse()) dispose() }
export function subagentRender(bench, component) { return bench.hooks.render(component, bench.props) }
export function subagentRenderWith(component, props) { const hooks = hookRuntime(); return { hooks, tree: hooks.render(component, props) } }
export function subagentRerender(rendered, component, props) { rendered.tree = rendered.hooks.render(component, props); return rendered.tree }
export function subagentUnmount(rendered) { rendered.hooks.dispose() }
export function subagentText(tree) { return textOf(tree) }
export function subagentTrigger(tree) { return one(tree, node => node.kind === 'button' && node.props['aria-haspopup'] === 'tree') }
export function subagentTree(tree) { return one(tree, node => node.props.role === 'tree') }
export function subagentTreeItems(tree) { return all(tree, node => node.props.role === 'treeitem') }
export function subagentTreeItem(tree, label) { return one(tree, node => node.props.role === 'treeitem' && String(node.props['aria-label']).includes(label)) }
export function subagentButton(tree, label) { return one(tree, node => node.kind === 'button' && String(node.props['aria-label'] ?? textOf(node)).includes(label)) }
export function subagentTextNode(tree, text) { return one(tree, node => textOf(node) === text) }
export function subagentGroups(tree) { return all(tree, node => node.props.role === 'group') }
export function subagentChildCount(node) { return node?.children?.length ?? 0 }
export function subagentClick(node) { return node.props.onClick({ preventDefault() {}, stopPropagation() {} }) }
export function subagentKey(node, key) {
  let stopped = false
  const event = { key, preventDefault() {}, stopPropagation() { stopped = true } }
  let current = node
  while (current !== null && current !== undefined && !stopped) {
    current.props?.onKeyDown?.(event)
    current = current.parentElement
  }
}
export function subagentProperty(node, key) { return node?.props?.[key] }
export function subagentSetNow(value) { fakeNow = value }
export function subagentTickTimers() { for (const callback of [...timers.values()]) callback() }
export function subagentDispatchPointer(target) { for (const listener of [...(documentListeners.get('pointerdown') ?? [])]) listener({ target }) }
export function subagentStyleCount() { return styles.filter(node => node.attributes['data-plugin'] === '@seekdeep-ai/seekdeep-client-ui-subagent').length }
export function subagentActiveElement() { return document.activeElement }
export function subagentSame(left, right) { return left === right }
export function subagentPromise(value) { return Promise.resolve(value) }
export function subagentBody() { return document.body }
"#)]
extern "C" {
    fn makeSubagentBench() -> JsValue;
    fn subagentSource(bench: &JsValue) -> JsValue;
    fn subagentEntries(bench: &JsValue) -> Array;
    fn subagentLocale(bench: &JsValue) -> Array;
    fn subagentCalls(bench: &JsValue) -> Array;
    fn subagentTranslations(bench: &JsValue) -> Array;
    fn subagentState(bench: &JsValue) -> JsValue;
    fn subagentProps(bench: &JsValue) -> JsValue;
    fn subagentSession(id: &str) -> JsValue;
    fn subagentRequest(query: &str) -> JsValue;
    fn subagentNotify(bench: &JsValue);
    fn subagentListenerCount(bench: &JsValue) -> u32;
    fn subagentDispose(bench: &JsValue);
    fn subagentRender(bench: &JsValue, component: &Function) -> JsValue;
    fn subagentRenderWith(component: &Function, props: &JsValue) -> JsValue;
    fn subagentRerender(rendered: &JsValue, component: &Function, props: &JsValue) -> JsValue;
    fn subagentUnmount(rendered: &JsValue);
    fn subagentText(tree: &JsValue) -> String;
    fn subagentTrigger(tree: &JsValue) -> JsValue;
    fn subagentTree(tree: &JsValue) -> JsValue;
    fn subagentTreeItems(tree: &JsValue) -> Array;
    fn subagentTreeItem(tree: &JsValue, label: &str) -> JsValue;
    fn subagentButton(tree: &JsValue, label: &str) -> JsValue;
    fn subagentTextNode(tree: &JsValue, text: &str) -> JsValue;
    fn subagentGroups(tree: &JsValue) -> Array;
    fn subagentChildCount(node: &JsValue) -> u32;
    fn subagentClick(node: &JsValue) -> JsValue;
    fn subagentKey(node: &JsValue, key: &str) -> JsValue;
    fn subagentProperty(node: &JsValue, key: &str) -> JsValue;
    fn subagentSetNow(value: f64);
    fn subagentTickTimers();
    fn subagentDispatchPointer(target: &JsValue);
    fn subagentStyleCount() -> u32;
    fn subagentActiveElement() -> JsValue;
    fn subagentSame(left: &JsValue, right: &JsValue) -> bool;
    fn subagentPromise(value: &JsValue) -> Promise;
    fn subagentBody() -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn call(value: &JsValue, key: &str) -> Function {
    property(value, key).dyn_into().unwrap()
}

fn configure(bench: &JsValue) {
    configure_client_ui_subagent(property(bench, "React"), property(bench, "primitives")).unwrap();
}

fn json(value: &str) -> JsValue {
    js_sys::JSON::parse(value).unwrap()
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn plugin_source_slots_actions_selection_and_cleanup_are_live() {
    let bench = makeSubagentBench();
    configure(&bench);
    apply_client_ui_subagent(property(&bench, "ctx")).unwrap();
    assert_eq!(
        subagent_inject()
            .iter()
            .map(|value| value.as_string().unwrap())
            .collect::<Vec<_>>(),
        ["inputTriggers", "sessions", "slots", "locale"]
    );
    assert_eq!(subagentStyleCount(), 1);
    assert_eq!(subagentLocale(&bench).length(), 1);
    assert_eq!(subagentEntries(&bench).length(), 2);

    let source = subagentSource(&bench);
    let candidates = call(&source, "candidates");
    let rows = JsFuture::from(Promise::resolve(
        &candidates
            .call2(&source, &subagentSession("parent"), &subagentRequest("ork"))
            .unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(Array::from(&rows).length(), 2);
    assert_eq!(
        property(&Array::from(&rows).get(0), "name")
            .as_string()
            .as_deref(),
        Some("worker")
    );
    let lexicon = call(&source, "lexicon")
        .call1(&source, &subagentSession("parent"))
        .unwrap();
    assert_eq!(Array::from(&lexicon).length(), 3);
    let childless = JsFuture::from(Promise::resolve(
        &candidates
            .call2(&source, &subagentSession("childless"), &subagentRequest(""))
            .unwrap(),
    ))
    .await
    .unwrap();
    assert_eq!(Array::from(&childless).length(), 0);
    let notifications = Array::new();
    let values = notifications.clone();
    let listener = Closure::wrap(Box::new(move || {
        values.push(&JsValue::TRUE);
    }) as Box<dyn FnMut()>);
    let off = call(&source, "subscribeLexicon")
        .call2(
            &source,
            &subagentSession("parent"),
            &listener.into_js_value(),
        )
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    assert_eq!(subagentListenerCount(&bench), 1);
    subagentNotify(&bench);
    assert_eq!(notifications.length(), 1);
    off.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(subagentListenerCount(&bench), 0);

    let picked = call(&source, "onPick")
        .call1(&source, &json(r#"{"candidate":{"name":"worker"}}"#))
        .unwrap();
    assert_eq!(
        property(&picked, "text").as_string().as_deref(),
        Some("@worker ")
    );
    let codec = property(&source, "codec");
    assert_eq!(
        call(&codec, "clipboardText")
            .call1(&codec, &JsValue::from_str("worker"))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("@worker")
    );
    let serialized = call(&codec, "serialize")
        .call2(&codec, &JsValue::from_str("worker"), &JsValue::UNDEFINED)
        .unwrap();
    assert_eq!(
        JsFuture::from(Promise::resolve(&serialized))
            .await
            .unwrap()
            .as_string()
            .as_deref(),
        Some("@worker")
    );
    assert!(property(&source, "matchSpace").is_undefined());
    assert!(property(&source, "matchEnter").is_undefined());

    let entries = subagentEntries(&bench);
    let catalog = entries.get(0);
    let catalog_options = property(&catalog, "options");
    assert_eq!(
        property(&catalog_options, "id").as_string().as_deref(),
        Some("subagent-catalog")
    );
    let actions = call(&catalog_options, "inject")
        .call1(&catalog_options, &JsValue::from_str("parent"))
        .unwrap();
    call(&actions, "openChild")
        .call1(
            &actions,
            &json(r#"{"parentSessionId":"parent","childSessionId":"child","mode":"continuable"}"#),
        )
        .unwrap();
    call(&actions, "refresh")
        .call1(&actions, &JsValue::from_str("parent"))
        .unwrap();
    call(&actions, "setCatalogOpen")
        .call2(&actions, &JsValue::from_str("parent"), &JsValue::TRUE)
        .unwrap();
    assert_eq!(subagentCalls(&bench).length(), 3);

    let composer = entries.get(1);
    let select = call(&property(&composer, "options"), "select");
    assert!(
        select
            .call1(&JsValue::UNDEFINED, &json(r"{}"))
            .unwrap()
            .is_null()
    );
    let one_shot = select
        .call1(
            &JsValue::UNDEFINED,
            &json(r#"{"session":{"running":true,"subagent":{"address":{"mode":"one-shot"},"parentAvailable":true}}}"#),
        )
        .unwrap();
    assert_eq!(
        property(&one_shot, "reason").as_string().as_deref(),
        Some("one-shot")
    );
    assert!(
        select
            .call1(
                &JsValue::UNDEFINED,
                &json(r#"{"session":{"running":true,"subagent":{"address":{"mode":"continuable"},"parentAvailable":false}}}"#),
            )
            .unwrap()
            .is_null()
    );
    let unavailable = select
        .call1(
            &JsValue::UNDEFINED,
            &json(r#"{"session":{"running":false,"subagent":{"address":{"mode":"continuable"},"parentAvailable":false}}}"#),
        )
        .unwrap();
    assert_eq!(
        property(&unavailable, "reason").as_string().as_deref(),
        Some("parent-unavailable")
    );

    subagentDispose(&bench);
    assert!(subagentSource(&bench).is_undefined());
    assert_eq!(subagentEntries(&bench).length(), 0);
    assert_eq!(subagentLocale(&bench).length(), 0);
    apply_client_ui_subagent(property(&bench, "ctx")).unwrap();
    assert!(!subagentSource(&bench).is_undefined());
    subagentDispose(&bench);
}

#[wasm_bindgen_test]
fn catalog_counts_rows_diagnostics_navigation_and_read_only_copy_are_live() {
    let bench = makeSubagentBench();
    configure(&bench);
    let component = exported_subagent_catalog_action_component()
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    let compact = subagentRender(&bench, &component);
    let trigger = subagentTrigger(&compact);
    assert_eq!(subagentText(&trigger), "2 个子代理");
    assert_eq!(
        subagentProperty(&trigger, "aria-label")
            .as_string()
            .as_deref(),
        Some("1 个子代理，正在运行")
    );
    subagentClick(&trigger);
    let open = subagentRender(&bench, &component);
    assert!(!subagentTree(&open).is_undefined());
    assert_eq!(subagentTreeItems(&open).length(), 3);
    assert!(subagentText(&open).contains("正在扫描项目文件 · 可继续 · 正在运行"));
    assert!(subagentText(&open).contains("一次性 · 当前未运行"));
    let diagnostic = subagentTreeItem(&open, "会话记录损坏");
    assert_eq!(
        subagentProperty(&diagnostic, "aria-disabled").as_bool(),
        Some(true)
    );
    assert!(!subagentButton(&open, "展开 worker").is_undefined());
    assert!(subagentButton(&open, "展开 reviewer").is_undefined());
    subagentClick(&subagentTreeItem(&open, "worker"));
    let calls = subagentCalls(&bench);
    assert_eq!(
        property(&calls.get(calls.length() - 1), "method")
            .as_string()
            .as_deref(),
        Some("setSubagentCatalogOpen")
    );

    let read_only = exported_subagent_read_only_composer_component()
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    let one_shot = read_only
        .call1(
            &JsValue::UNDEFINED,
            &json(r#"{"matched":{"reason":"one-shot"}}"#),
        )
        .unwrap_err();
    assert!(one_shot.is_object());
    let props = json(r#"{"matched":{"reason":"one-shot"}}"#);
    Reflect::set(
        &props,
        &JsValue::from_str("t"),
        &property(&subagentProps(&bench), "t"),
    )
    .unwrap();
    let tree = read_only.call1(&JsValue::UNDEFINED, &props).unwrap();
    assert!(subagentText(&tree).contains("一次性任务不支持后续消息"));
    let props = json(r#"{"matched":{"reason":"parent-unavailable"}}"#);
    Reflect::set(
        &props,
        &JsValue::from_str("t"),
        &property(&subagentProps(&bench), "t"),
    )
    .unwrap();
    let tree = read_only.call1(&JsValue::UNDEFINED, &props).unwrap();
    assert!(subagentText(&tree).contains("父会话当前不在线"));
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)]
fn singular_branchless_diagnostic_fallback_and_error_states_are_live() {
    let bench = makeSubagentBench();
    configure(&bench);
    let state = subagentState(&bench);
    let catalogs = property(&state, "subagentsByParent");
    Reflect::set(
        &catalogs,
        &JsValue::from_str("parent"),
        &json(r#"{"entries":[{"kind":"child","id":"only","mode":"continuable","label":"only","activity":"running","hasChildren":false}],"parentAvailable":true,"state":"ready","error":null}"#),
    )
    .unwrap();
    Reflect::set(
        &state,
        &JsValue::from_str("byId"),
        &json(r#"{"only":{"id":"only","displayTitle":"only","parentId":"parent","origin":"subagent","running":true,"blank":false,"updatedAt":0}}"#),
    )
    .unwrap();
    let component = exported_subagent_catalog_action_component()
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    let mut tree = subagentRender(&bench, &component);
    let trigger = subagentTrigger(&tree);
    assert_eq!(subagentText(&trigger), "1 个子代理");
    assert_eq!(
        subagentProperty(&trigger, "aria-label")
            .as_string()
            .as_deref(),
        Some("1 个子代理，正在运行")
    );
    let translations = subagentTranslations(&bench);
    let running = translations
        .iter()
        .find(|row| property(row, "key").as_string().as_deref() == Some("count.running.one"))
        .unwrap();
    assert_eq!(
        property(&property(&running, "values"), "count").as_f64(),
        Some(1.0)
    );
    subagentClick(&trigger);
    tree = subagentRender(&bench, &component);
    assert_eq!(subagentChildCount(&subagentTreeItem(&tree, "only")), 1);

    let root = property(&catalogs, "parent");
    Reflect::set(
        &root,
        &JsValue::from_str("entries"),
        &json(r#"[{"kind":"diagnostic","id":"unsupported","reason":"unsupported"},{"kind":"diagnostic","id":"unavailable","reason":"unavailable"},{"kind":"child","id":"unlabeled","mode":"one-shot","activity":"inactive","hasChildren":false}]"#),
    )
    .unwrap();
    Reflect::set(&state, &JsValue::from_str("byId"), &json(r"{}")).unwrap();
    tree = subagentRender(&bench, &component);
    assert!(!subagentTreeItem(&tree, "子代理记录版本不受支持").is_undefined());
    assert!(!subagentTreeItem(&tree, "会话记录暂不可用").is_undefined());
    subagentKey(&subagentTreeItem(&tree, "unlabeled"), " ");
    let open = subagentCalls(&bench)
        .iter()
        .find(|row| property(row, "method").as_string().as_deref() == Some("openSubagent"))
        .unwrap();
    let address = Array::from(&property(&open, "args")).get(0);
    assert_eq!(
        property(&address, "childSessionId").as_string().as_deref(),
        Some("unlabeled")
    );
    assert_eq!(
        property(&address, "mode").as_string().as_deref(),
        Some("one-shot")
    );

    Reflect::set(&root, &JsValue::from_str("entries"), &Array::new()).unwrap();
    Reflect::set(
        &root,
        &JsValue::from_str("state"),
        &JsValue::from_str("error"),
    )
    .unwrap();
    Reflect::set(&root, &JsValue::from_str("error"), &JsValue::NULL).unwrap();
    tree = subagentRender(&bench, &component);
    subagentClick(&subagentTrigger(&tree));
    tree = subagentRender(&bench, &component);
    assert!(subagentText(&tree).contains("无法加载子代理"));
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)]
fn descendant_loading_expansion_metrics_errors_and_teardown_are_live() {
    let bench = makeSubagentBench();
    configure(&bench);
    let state = subagentState(&bench);
    let by_id = property(&state, "byId");
    Reflect::set(
        &by_id,
        &JsValue::from_str("grandchild"),
        &json(r#"{"id":"grandchild","displayTitle":"indexer","parentId":"child","origin":"subagent","running":true,"blank":false,"updatedAt":0}"#),
    )
    .unwrap();
    let child = property(&by_id, "child");
    Reflect::set(
        &child,
        &JsValue::from_str("projectionValues"),
        &json(r#"{"tokenUsage":{"uncachedInputTokens":1000,"outputTokens":200,"cacheReadTokens":3000,"cacheWriteTokens":400},"subagentTiming":{"settledMs":65000,"active":{"since":1999999995000,"through":1999999999000}}}"#),
    )
    .unwrap();
    let component = exported_subagent_catalog_action_component()
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    let mut tree = subagentRender(&bench, &component);
    subagentClick(&subagentTrigger(&tree));
    tree = subagentRender(&bench, &component);
    assert!(subagentText(&tree).contains("4.6K tok"));
    assert!(subagentText(&tree).contains("1分10秒"));
    assert_eq!(
        subagentProperty(&subagentTextNode(&tree, "1分10秒"), "title")
            .as_string()
            .as_deref(),
        Some("总活跃耗时：1分10秒")
    );
    let duration_call = subagentTranslations(&bench)
        .iter()
        .find(|row| property(row, "key").as_string().as_deref() == Some("duration.minutes"))
        .unwrap();
    let duration_values = property(&duration_call, "values");
    assert_eq!(property(&duration_values, "minutes").as_f64(), Some(1.0));
    assert_eq!(
        property(&duration_values, "seconds").as_string().as_deref(),
        Some("10")
    );
    subagentClick(&subagentButton(&tree, "展开 worker"));
    tree = subagentRender(&bench, &component);
    assert_eq!(subagentGroups(&tree).length(), 1);
    assert_eq!(
        subagentProperty(&subagentGroups(&tree).get(0), "aria-busy").as_bool(),
        Some(true)
    );
    assert!(subagentText(&tree).contains("正在加载子代理"));

    let catalogs = property(&state, "subagentsByParent");
    Reflect::set(
        &catalogs,
        &JsValue::from_str("child"),
        &json(r#"{"entries":[{"kind":"child","id":"grandchild","mode":"continuable","label":"indexer","activity":"running","hasChildren":true}],"parentAvailable":true,"state":"ready","error":null}"#),
    )
    .unwrap();
    tree = subagentRender(&bench, &component);
    let nested = subagentTreeItem(&tree, "indexer");
    assert_eq!(subagentProperty(&nested, "aria-level").as_f64(), Some(2.0));
    subagentClick(&subagentButton(&tree, "展开 indexer"));
    tree = subagentRender(&bench, &component);
    subagentKey(&subagentTreeItem(&tree, "worker"), "ArrowLeft");
    tree = subagentRender(&bench, &component);
    assert!(subagentTreeItem(&tree, "indexer").is_undefined());
    assert!(subagentCalls(&bench).iter().any(|row| {
        if property(&row, "method").as_string().as_deref() != Some("setSubagentCatalogOpen") {
            return false;
        }
        let args = Array::from(&property(&row, "args"));
        args.get(0).as_string().as_deref() == Some("grandchild")
            && args.get(1).as_bool() == Some(false)
    }));
    subagentKey(&subagentTreeItem(&tree, "worker"), "ArrowRight");
    tree = subagentRender(&bench, &component);
    subagentClick(&subagentTreeItem(&tree, "indexer"));
    let open = subagentCalls(&bench)
        .iter()
        .rev()
        .find(|row| property(row, "method").as_string().as_deref() == Some("openSubagent"))
        .unwrap();
    let address = Array::from(&property(&open, "args")).get(0);
    assert_eq!(
        property(&address, "parentSessionId").as_string().as_deref(),
        Some("child")
    );
    tree = subagentRender(&bench, &component);
    subagentClick(&subagentTrigger(&tree));
    let _reopened = subagentRender(&bench, &component);

    subagentSetNow(2_000_000_001_000.0);
    subagentTickTimers();
    tree = subagentRender(&bench, &component);
    assert!(subagentText(&tree).contains("1分11秒"));

    let root = property(&catalogs, "parent");
    Reflect::set(&root, &JsValue::from_str("entries"), &Array::new()).unwrap();
    Reflect::set(
        &root,
        &JsValue::from_str("state"),
        &JsValue::from_str("error"),
    )
    .unwrap();
    Reflect::set(
        &root,
        &JsValue::from_str("error"),
        &json(r#"{"code":"internal","message":"index down","details":{}}"#),
    )
    .unwrap();
    tree = subagentRender(&bench, &component);
    assert!(subagentText(&tree).contains("index down"));
    subagentClick(&subagentButton(&tree, "重试"));
    assert!(
        subagentCalls(&bench)
            .iter()
            .any(|row| property(&row, "method").as_string().as_deref() == Some("refreshSubagents"))
    );
    subagentDispose(&bench);
    let calls = subagentCalls(&bench);
    let last = calls.get(calls.length() - 1);
    assert_eq!(
        property(&last, "method").as_string().as_deref(),
        Some("setSubagentCatalogOpen")
    );
    assert_eq!(
        Array::from(&property(&last, "args")).get(1).as_bool(),
        Some(false)
    );
}

#[wasm_bindgen_test(async)]
async fn keyboard_focus_outside_close_and_empty_visibility_are_live() {
    let bench = makeSubagentBench();
    configure(&bench);
    let component = exported_subagent_catalog_action_component()
        .unwrap()
        .dyn_into::<Function>()
        .unwrap();
    let mut tree = subagentRender(&bench, &component);
    let trigger = subagentTrigger(&tree);
    subagentKey(&trigger, "ArrowDown");
    tree = subagentRender(&bench, &component);
    JsFuture::from(subagentPromise(&JsValue::UNDEFINED))
        .await
        .unwrap();
    let worker = subagentTreeItem(&tree, "worker");
    assert!(subagentSame(&subagentActiveElement(), &worker));
    subagentKey(&worker, "End");
    assert!(subagentSame(
        &subagentActiveElement(),
        &subagentTreeItem(&tree, "reviewer")
    ));
    subagentKey(&subagentActiveElement(), "Home");
    assert!(subagentSame(
        &subagentActiveElement(),
        &subagentTreeItem(&tree, "worker")
    ));
    subagentKey(&subagentActiveElement(), "ArrowUp");
    assert!(subagentSame(
        &subagentActiveElement(),
        &subagentTreeItem(&tree, "reviewer")
    ));
    subagentKey(&subagentActiveElement(), "Escape");
    JsFuture::from(subagentPromise(&JsValue::UNDEFINED))
        .await
        .unwrap();
    assert!(subagentSame(
        &subagentActiveElement(),
        &subagentTrigger(&tree)
    ));
    tree = subagentRender(&bench, &component);
    assert!(subagentTree(&tree).is_undefined());

    subagentClick(&subagentTrigger(&tree));
    tree = subagentRender(&bench, &component);
    subagentDispatchPointer(&subagentTree(&tree));
    assert!(!subagentTree(&subagentRender(&bench, &component)).is_undefined());
    subagentDispatchPointer(&subagentBody());
    assert!(subagentTree(&subagentRender(&bench, &component)).is_undefined());

    let state = subagentState(&bench);
    let catalogs = property(&state, "subagentsByParent");
    Reflect::set(
        &catalogs,
        &JsValue::from_str("parent"),
        &json(r#"{"entries":[],"parentAvailable":true,"state":"loading","error":null}"#),
    )
    .unwrap();
    Reflect::set(&state, &JsValue::from_str("byId"), &json(r"{}")).unwrap();
    let tree = subagentRender(&bench, &component);
    assert!(subagentTrigger(&tree).is_undefined());
}
