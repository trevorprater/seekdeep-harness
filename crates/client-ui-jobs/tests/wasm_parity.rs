//! Live Rust/WASM background-job popover and Client plugin parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Reflect};
use seekdeep_client_ui_jobs::{
    apply_client_ui_jobs, configure_client_ui_jobs, exported_job_list_action_component, jobs_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
class FakeNode {
  constructor(kind = 'node', props = {}, children = []) {
    this.kind = kind
    this.props = props
    this.children = children
    this.focused = false
  }
  contains(target) {
    if (target === this) return true
    return this.children.some(child => child instanceof FakeNode && child.contains(target))
  }
  focus() { this.focused = true; globalThis.document.activeElement = this }
}
globalThis.Node = FakeNode

const styleNodes = []
const listeners = new Map()
const selectedStyles = selector => {
  const match = /^style\[data-plugin=(.+)\]$/.exec(selector)
  if (match === null) return []
  const plugin = JSON.parse(match[1])
  return styleNodes.filter(node => node.kind === 'style' && node.attributes['data-plugin'] === plugin)
}
globalThis.document = {
  activeElement: null,
  body: new FakeNode('body'),
  querySelector(selector) { return selectedStyles(selector)[0] ?? null },
  querySelectorAll(selector) { return selectedStyles(selector) },
  createElement(kind) {
    return {
      kind, attributes: {}, textContent: '',
      setAttribute(name, value) { this.attributes[name] = value },
    }
  },
  head: { appendChild(node) { styleNodes.push(node); return node } },
  addEventListener(name, listener) {
    if (!listeners.has(name)) listeners.set(name, new Set())
    listeners.get(name).add(listener)
  },
  removeEventListener(name, listener) { listeners.get(name)?.delete(listener) },
}

function hooks(bench) {
  const slots = []
  let cursor = 0
  let pending = []
  const React = {
    createElement(kind, props, ...children) {
      const node = new FakeNode(kind, props ?? {}, children)
      const ref = props?.ref
      if (typeof ref === 'function') ref(node)
      else if (ref !== null && typeof ref === 'object') ref.current = node
      return node
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
    useRef(initial) {
      const index = cursor++
      if (slots[index] === undefined) slots[index] = { kind: 'ref', current: initial }
      return slots[index]
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
const zh = {
  'count.live.one': '{count} 个后台任务运行中', 'count.live.other': '{count} 个后台任务运行中',
  'count.idle.one': '{count} 个后台任务', 'count.idle.other': '{count} 个后台任务',
  'list.aria': '后台任务', 'status.running': '运行中', 'status.stopping': '正在停止',
  'status.completed': '已完成', 'status.killed': '已取消', 'status.failed': '已失败',
  'duration.seconds': '{seconds}秒', 'duration.minutes': '{minutes}分{seconds}秒',
  'duration.hours': '{hours}小时{minutes}分', 'duration.title.live': '已运行 {duration}',
  'duration.title.done': '耗时 {duration}',
}
function translate(key, values = {}) {
  return Object.entries(values).reduce(
    (text, [name, value]) => text.replace(`{${name}}`, String(value)), zh[key] ?? key,
  )
}

export function makeJobsBench() {
  const START = 1700000000000
  const timers = new Map()
  let nextTimer = 1
  const bench = { now: START, jobs: undefined, timers, intervalStarts: 0, START }
  Date.now = () => bench.now
  globalThis.setInterval = (callback, delay) => {
    const id = nextTimer++
    timers.set(id, { callback, delay, next: bench.now + delay })
    bench.intervalStarts += 1
    return id
  }
  globalThis.clearInterval = id => { timers.delete(id) }
  const hookState = hooks(bench)
  bench.hooks = hookState
  bench.React = hookState.React
  bench.primitives = { IconChevronDownOutline14: 'IconChevronDownOutline14', StateDot: 'StateDot' }
  bench.props = {
    sessionId: 'session',
    useSessions(selector) {
      return selector({ jobsBySession: bench.jobs === undefined ? {} : { session: bench.jobs } })
    },
    t: translate,
  }
  return bench
}
export function jobsRender(bench, component) { bench.tree = bench.hooks.render(component, bench.props); return bench.tree }
export function jobsSet(bench, jobs) { bench.jobs = jobs }
export function jobsSetNow(bench, now) { bench.now = now }
export function jobsAdvance(bench, elapsed) {
  const target = bench.now + elapsed
  while (true) {
    let next
    for (const timer of bench.timers.values()) if (next === undefined || timer.next < next) next = timer.next
    if (next === undefined || next > target) break
    bench.now = next
    for (const timer of [...bench.timers.values()]) {
      if (timer.next === next) { timer.next += timer.delay; timer.callback() }
    }
  }
  bench.now = target
}
export function jobsIntervalStarts(bench) { return bench.intervalStarts }
export function jobsButton(tree) { return find(tree, node => node.kind === 'button') }
export function jobsMenu(tree) { return find(tree, node => node.kind === 'ul') }
export function jobsRoot(tree) { return find(tree, node => node.kind === 'div' && node.props?.className === 'seekdeep-jobs-root') }
export function jobsRows(tree) { return jobsMenu(tree)?.children ?? [] }
export function jobsCells(tree) {
  return jobsRows(tree).map(row => row.children.map(textOf).filter(Boolean))
}
export function jobsClick(node) { return node.props.onClick() }
export function jobsKey(tree, key) { return jobsRoot(tree).props.onKeyDown({ key, preventDefault() {} }) }
export function jobsPointer(target) {
  for (const listener of [...(listeners.get('pointerdown') ?? [])]) listener({ target })
}
export function jobsBody() { return document.body }
export function jobsActiveElement() { return document.activeElement }
export function jobsText(tree) { return textOf(tree) }
export function jobsUnmount(bench) { bench.hooks.unmount() }
export function jobsStyleCount() {
  return document.querySelectorAll('style[data-plugin="@seekdeep-ai/seekdeep-client-ui-jobs"]').length
}

export function makeJobsPluginBench() {
  const ui = makeJobsBench()
  const effects = [], entries = []
  const own = dispose => { effects.push(dispose); return dispose }
  const ctx = {
    sessions: {},
    effect(setup) { return own(setup()) },
    locale: { register() { return () => {} } },
  }
  ctx.slots = {
    inject(name, install) { return own(install()) },
    register(options, component) {
      const entry = { options, component }
      entries.push(entry)
      return () => entries.splice(entries.indexOf(entry), 1)
    },
  }
  return { ...ui, ctx, effects, entries }
}
export function jobsPluginEntries(bench) { return bench.entries }
export function jobsPluginDispose(bench) {
  for (const dispose of bench.effects.splice(0).reverse()) dispose()
}
"#)]
extern "C" {
    fn makeJobsBench() -> JsValue;
    fn jobsRender(bench: &JsValue, component: &Function) -> JsValue;
    fn jobsSet(bench: &JsValue, jobs: &JsValue);
    fn jobsSetNow(bench: &JsValue, now: f64);
    fn jobsAdvance(bench: &JsValue, elapsed: f64);
    fn jobsIntervalStarts(bench: &JsValue) -> u32;
    fn jobsButton(tree: &JsValue) -> JsValue;
    fn jobsMenu(tree: &JsValue) -> JsValue;
    fn jobsRows(tree: &JsValue) -> Array;
    fn jobsCells(tree: &JsValue) -> Array;
    fn jobsClick(node: &JsValue) -> JsValue;
    fn jobsKey(tree: &JsValue, key: &str) -> JsValue;
    fn jobsPointer(target: &JsValue);
    fn jobsBody() -> JsValue;
    fn jobsActiveElement() -> JsValue;
    fn jobsText(tree: &JsValue) -> String;
    fn jobsUnmount(bench: &JsValue);
    fn jobsStyleCount() -> u32;
    fn makeJobsPluginBench() -> JsValue;
    fn jobsPluginEntries(bench: &JsValue) -> Array;
    fn jobsPluginDispose(bench: &JsValue);
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
    configure_client_ui_jobs(property(bench, "React"), property(bench, "primitives")).unwrap();
    exported_job_list_action_component()
        .unwrap()
        .dyn_into()
        .unwrap()
}

fn data(value: &str) -> JsValue {
    js_sys::JSON::parse(value).unwrap()
}

fn row_cells(tree: &JsValue, index: u32) -> Array {
    Array::from(&jobsCells(tree).get(index))
}

#[wasm_bindgen_test]
fn visibility_counts_order_statuses_and_wire_tolerance_are_live() {
    let bench = makeJobsBench();
    let jobs_component = component(&bench);
    assert!(jobsRender(&bench, &jobs_component).is_null());
    jobsSet(
        &bench,
        &data(
            r#"[{"id":"1","kind":"bash","label":"one","status":"running","startedAt":1700000000000},{"id":"2","kind":"bash","label":"two","status":"stopping","startedAt":1700000001000}]"#,
        ),
    );
    let counted = jobsRender(&bench, &jobs_component);
    assert_eq!(
        property(&jobsButton(&counted), "aria-label")
            .as_string()
            .as_deref(),
        Some("2 个后台任务运行中")
    );
    assert_eq!(jobsStyleCount(), 1);

    jobsSet(
        &bench,
        &data(
            r#"[{"id":"3","kind":"bash","label":"old done","status":"completed","startedAt":1700000000000,"finishedAt":1700000001000},{"id":"4","kind":"bash","label":"new done","status":"failed","startedAt":1700000000000,"finishedAt":1700000009000},{"id":"2","kind":"bash","label":"later live","status":"running","startedAt":1700000005000},{"id":"1","kind":"bash","label":"earlier live","status":"running","startedAt":1700000000000}]"#,
        ),
    );
    let closed = jobsRender(&bench, &jobs_component);
    assert!(jobsClick(&jobsButton(&closed)).is_undefined());
    let open = jobsRender(&bench, &jobs_component);
    assert_eq!(jobsRows(&open).length(), 4);
    for (index, expected) in [
        (0, "earlier live"),
        (1, "later live"),
        (2, "new done"),
        (3, "old done"),
    ] {
        assert_eq!(
            row_cells(&open, index).get(1).as_string().as_deref(),
            Some(expected)
        );
    }
    assert_eq!(
        row_cells(&open, 2).get(2).as_string().as_deref(),
        Some("已失败")
    );
    assert_eq!(
        row_cells(&open, 2).get(3).as_string().as_deref(),
        Some("9秒")
    );

    jobsSet(
        &bench,
        &data(
            r#"[{"id":"h","kind":"bash","label":"hours","status":"completed","startedAt":1700000000000,"finishedAt":1700007380000},{"id":"m","kind":"bash","label":"minutes","status":"killed","startedAt":1700000000000,"finishedAt":1700000125000,"detail":"signal: SIGTERM"},{"id":"s","kind":"bash","label":"skew","status":"completed","startedAt":1700000005000,"finishedAt":1700000000000}]"#,
        ),
    );
    let mixed = jobsRender(&bench, &jobs_component);
    let cells = jobsCells(&mixed);
    assert_eq!(
        Array::from(&cells.get(0)).get(3).as_string().as_deref(),
        Some("2小时3分")
    );
    assert_eq!(
        Array::from(&cells.get(1)).get(2).as_string().as_deref(),
        Some("signal: SIGTERM")
    );
    assert_eq!(
        Array::from(&cells.get(1)).get(3).as_string().as_deref(),
        Some("2分5秒")
    );
    assert_eq!(
        Array::from(&cells.get(2)).get(3).as_string().as_deref(),
        Some("0秒")
    );
}

#[wasm_bindgen_test]
fn timer_escape_pointer_and_disappearing_jobs_are_live() {
    let bench = makeJobsBench();
    let jobs_component = component(&bench);
    jobsSetNow(&bench, 1_700_000_001_000.0);
    jobsSet(
        &bench,
        &data(
            r#"[{"id":"1","kind":"bash","label":"live","status":"running","startedAt":1700000000000},{"id":"2","kind":"bash","label":"done","status":"completed","startedAt":1700000000000,"finishedAt":1700000004000}]"#,
        ),
    );
    let closed = jobsRender(&bench, &jobs_component);
    assert_eq!(jobsIntervalStarts(&bench), 0);
    let trigger = jobsButton(&closed);
    jobsClick(&trigger);
    let open = jobsRender(&bench, &jobs_component);
    assert_eq!(jobsIntervalStarts(&bench), 1);
    assert_eq!(
        row_cells(&open, 0).get(3).as_string().as_deref(),
        Some("1秒")
    );
    assert_eq!(
        row_cells(&open, 1).get(3).as_string().as_deref(),
        Some("4秒")
    );
    jobsAdvance(&bench, 2_000.0);
    let advanced = jobsRender(&bench, &jobs_component);
    assert_eq!(
        row_cells(&advanced, 0).get(3).as_string().as_deref(),
        Some("3秒")
    );
    assert_eq!(
        row_cells(&advanced, 1).get(3).as_string().as_deref(),
        Some("4秒")
    );

    jobsKey(&advanced, "ArrowDown");
    let after_arrow = jobsRender(&bench, &jobs_component);
    assert_eq!(
        property(&jobsButton(&after_arrow), "aria-expanded").as_bool(),
        Some(true)
    );
    jobsKey(&after_arrow, "Escape");
    let escaped = jobsRender(&bench, &jobs_component);
    assert_eq!(
        property(&jobsButton(&escaped), "aria-expanded").as_bool(),
        Some(false)
    );
    assert!(js_sys::Object::is(
        &jobsActiveElement(),
        &jobsButton(&after_arrow)
    ));

    jobsClick(&jobsButton(&escaped));
    let reopened = jobsRender(&bench, &jobs_component);
    let menu = jobsMenu(&reopened);
    jobsPointer(&menu);
    assert_eq!(
        property(
            &jobsButton(&jobsRender(&bench, &jobs_component)),
            "aria-expanded"
        )
        .as_bool(),
        Some(true)
    );
    jobsPointer(&jobsBody());
    assert_eq!(
        property(
            &jobsButton(&jobsRender(&bench, &jobs_component)),
            "aria-expanded"
        )
        .as_bool(),
        Some(false)
    );

    jobsClick(&jobsButton(&jobsRender(&bench, &jobs_component)));
    jobsRender(&bench, &jobs_component);
    jobsSet(&bench, &Array::new().into());
    assert!(jobsRender(&bench, &jobs_component).is_null());
    jobsUnmount(&bench);

    let settled = makeJobsBench();
    let settled_component = component(&settled);
    jobsSet(
        &settled,
        &data(
            r#"[{"id":"done","kind":"bash","label":"done","status":"completed","startedAt":1700000000000,"finishedAt":1700000001000}]"#,
        ),
    );
    let settled_closed = jobsRender(&settled, &settled_component);
    jobsClick(&jobsButton(&settled_closed));
    jobsRender(&settled, &settled_component);
    assert_eq!(jobsIntervalStarts(&settled), 0);
    jobsUnmount(&settled);
}

#[wasm_bindgen_test]
fn plugin_registers_ordered_header_action_and_retracts_it() {
    let bench = makeJobsPluginBench();
    configure_client_ui_jobs(property(&bench, "React"), property(&bench, "primitives")).unwrap();
    apply_client_ui_jobs(property(&bench, "ctx")).unwrap();
    assert_eq!(jobs_inject().length(), 3);
    assert_eq!(jobsPluginEntries(&bench).length(), 1);
    let entry = jobsPluginEntries(&bench).get(0);
    let options = property(&entry, "options");
    assert_eq!(
        property(&options, "name").as_string().as_deref(),
        Some("conversation.session.header.actions")
    );
    assert_eq!(
        property(&options, "id").as_string().as_deref(),
        Some("job-list")
    );
    assert_eq!(property(&options, "order").as_f64(), Some(20.0));
    jobsPluginDispose(&bench);
    assert_eq!(jobsPluginEntries(&bench).length(), 0);
    apply_client_ui_jobs(property(&bench, "ctx")).unwrap();
    assert_eq!(jobsPluginEntries(&bench).length(), 1);
    jobsPluginDispose(&bench);
    assert_eq!(jobsPluginEntries(&bench).length(), 0);
}
