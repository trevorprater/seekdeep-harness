//! Live Rust/WASM produced-files component, plugin, and native Definition parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Reflect};
use seekdeep_client_ui_deliverables::{
    apply_client_ui_deliverables, configure_client_ui_deliverables, deliverables_inject,
    exported_produced_files_component, exported_produced_for_closing,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
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

function sameDeps(left, right) {
  return left !== undefined && left.length === right.length
    && left.every((value, index) => Object.is(value, right[index]))
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

function hooks(bench) {
  const slots = []
  let cursor = 0
  let dirty = false
  let pending = []
  let priorFunctionRefs = []
  let currentFunctionRefs = []
  const React = {
    createElement(kind, props, ...children) {
      const actualProps = props ?? {}
      const node = { kind, props: actualProps, children, textContent: children.map(textOf).join('') }
      Object.defineProperty(node, 'clientWidth', {
        configurable: true,
        get() { return actualProps['data-produced-files-row'] !== undefined ? bench.available : 0 },
      })
      node.getBoundingClientRect = () => {
        const classes = actualProps.className ?? ''
        if (classes.includes('seekdeep-deliverables-probe')) {
          if (kind !== 'button') return { width: 60 }
          return { width: node.textContent === 'a.html' || node.textContent === 'b.css' ? 50 : 100 }
        }
        return { width: 0 }
      }
      const ref = actualProps.ref
      if (typeof ref === 'function') {
        ref(node)
        currentFunctionRefs.push(ref)
      } else if (ref !== null && typeof ref === 'object') {
        ref.current = node
      }
      return node
    },
    useState(initial) {
      const index = cursor++
      if (slots[index] === undefined) {
        const slot = { kind: 'state', value: initial }
        slot.set = value => {
          const next = typeof value === 'function' ? value(slot.value) : value
          if (!Object.is(next, slot.value)) { slot.value = next; dirty = true }
        }
        slots[index] = slot
      }
      return [slots[index].value, slots[index].set]
    },
    useRef(initial) {
      const index = cursor++
      if (slots[index] === undefined) slots[index] = { kind: 'ref', current: initial }
      return slots[index]
    },
    useLayoutEffect(effect, dependencies) {
      const index = cursor++
      if (slots[index] === undefined) slots[index] = { kind: 'effect', deps: undefined, cleanup: undefined }
      const slot = slots[index]
      const deps = Array.from(dependencies)
      if (!sameDeps(slot.deps, deps)) pending.push({ slot, deps, effect })
    },
  }
  return {
    React,
    render(component, props) {
      let tree
      let iterations = 0
      do {
        dirty = false
        for (const ref of priorFunctionRefs) ref(null)
        priorFunctionRefs = []
        currentFunctionRefs = []
        cursor = 0
        pending = []
        tree = component(props)
        for (const entry of pending) {
          if (typeof entry.slot.cleanup === 'function') entry.slot.cleanup()
          entry.slot.deps = entry.deps
          entry.slot.cleanup = entry.effect()
        }
        priorFunctionRefs = currentFunctionRefs
        iterations += 1
        if (iterations > 8) throw new Error('deliverables hook bench did not settle')
      } while (dirty)
      return tree
    },
    unmount() {
      for (const ref of priorFunctionRefs) ref(null)
      priorFunctionRefs = []
      for (const slot of [...slots].reverse()) {
        if (slot?.kind === 'effect' && typeof slot.cleanup === 'function') slot.cleanup()
      }
    },
  }
}

const zh = {
  'produced.label': '产物', 'produced.moreOne': '+ 1 个文件',
  'produced.more': '+ {count} 个文件', 'produced.open': '打开 {name}',
  'produced.showInFolder': '在文件夹中显示',
}
const en = {
  'produced.label': 'Produced', 'produced.moreOne': '+ 1 file',
  'produced.more': '+ {count} files', 'produced.open': 'Open {name}',
  'produced.showInFolder': 'Show in folder',
}
function translate(dictionary) {
  return (key, values = {}) => Object.entries(values).reduce(
    (text, [name, value]) => text.replace(`{${name}}`, value), dictionary[key] ?? key,
  )
}

export function makeDeliverablesBench(language = 'zh', canOpenPath = true, isLoopback = true) {
  const bench = {
    available: 226,
    gap: '8px',
    observed: [],
    observers: [],
    disconnected: 0,
    opened: [],
  }
  const hookState = hooks(bench)
  const paths = ['deep/a.html', 'b.css', 'c.ts', 'd.ts', 'e.ts', 'f.ts', 'g.ts']
  bench.React = hookState.React
  bench.hooks = hookState
  bench.props = {
    matched: paths,
    openFile(path) { bench.opened.push(path) },
    isLoopback,
    useHostDescription(selector) {
      return selector(canOpenPath === null ? undefined : { canOpenPath })
    },
    t: translate(language === 'en' ? en : zh),
  }
  globalThis.getComputedStyle = () => ({ columnGap: bench.gap, gap: bench.gap })
  globalThis.ResizeObserver = class {
    constructor(callback) { this.callback = callback; this.active = true; bench.observers.push(this) }
    observe(target) { if (this.active) bench.observed.push(target) }
    disconnect() { if (this.active) { this.active = false; bench.disconnected += 1 } }
  }
  return bench
}
export function deliverablesRender(bench, component) {
  bench.tree = bench.hooks.render(component, bench.props)
  return bench.tree
}
export function deliverablesText(tree) { return textOf(tree) }
export function deliverablesVisibleCount(tree) {
  const row = find(tree, node => node.props?.['data-produced-files-row'] !== undefined)
  return (row?.children ?? []).filter(child => child?.kind === 'button').length
}
export function deliverablesClickAria(tree, label) {
  const node = find(tree, candidate => candidate.props?.['aria-label'] === label
    || (candidate.kind === 'button' && textOf(candidate) === label))
  if (node === undefined) return false
  return node.props.onClick() === undefined
}
export function deliverablesTitleByAria(tree, label) {
  return find(tree, candidate => candidate.props?.['aria-label'] === label)?.props?.title
}
export function deliverablesSetAvailable(bench, available) { bench.available = available }
export function deliverablesClearGap(bench) { bench.gap = '' }
export function deliverablesResize(bench) {
  for (const observer of bench.observers) if (observer.active) observer.callback([], observer)
}
export function deliverablesSetPaths(bench, paths) { bench.props = { ...bench.props, matched: paths } }
export function deliverablesObserveCount(bench) { return bench.observed.length }
export function deliverablesResetObserve(bench) { bench.observed.length = 0 }
export function deliverablesDisconnectCount(bench) { return bench.disconnected }
export function deliverablesUnmount(bench) { bench.hooks.unmount() }
export function deliverablesOpenCalls(bench) { return bench.opened }
export function deliverablesStyleCount() {
  return document.querySelectorAll('style[data-plugin="@seekdeep-ai/seekdeep-client-ui-deliverables"]').length
}

export function makeDeliverablesPluginBench() {
  const effects = []
  const entries = []
  const definitions = []
  const services = new Map()
  const opened = []
  const hostDescription = { getSnapshot() { return undefined }, subscribe() { return () => {} } }
  const own = dispose => { effects.push(dispose); return dispose }
  const ctx = {
    effect(setup) { return own(setup()) },
    provide(name, value) { services.set(name, value); return own(() => services.delete(name)) },
    locale: {
      register() { return () => {} },
      bind() { return translate(en) },
    },
    conversationEvents: {
      register(definition) {
        definitions.push(definition)
        return own(() => definitions.splice(definitions.indexOf(definition), 1))
      },
    },
    connection: { isLoopback: false, hostDescription },
  }
  ctx.slots = {
    inject(name, install) { return own(install()) },
    register(options, component) {
      const entry = { options, component }
      entries.push(entry)
      return () => entries.splice(entries.indexOf(entry), 1)
    },
  }
  return { ctx, effects, entries, definitions, services, opened, hostDescription }
}
export function deliverablesPluginEntries(bench) { return bench.entries }
export function deliverablesPluginDefinitions(bench) { return bench.definitions }
export function deliverablesPluginInjected(bench) { return bench.entries[0].options.inject() }
export function deliverablesPluginService(bench) { return bench.services.get('chatFileMentions') }
export function deliverablesPluginOwner(bench, produced = [{ seq: 2, path: 'site/report.html' }]) {
  return {
    seq: 3,
    turn: { data: { get(key) { return key === 'deliverables' && produced !== null ? { produced } : undefined } } },
    openFile(path) { bench.opened.push(path) },
  }
}
export function deliverablesDefinitionLocation(bench, scope) {
  const context = {
    key: 'deliverables:2', kind: 'deliverables', id: '2', matches: [], start: undefined,
    state: { turn: 2, calls: {}, produced: [{ seq: 3, path: 'out/report.txt' }] },
    current: new Map(),
  }
  return bench.definitions[0].buildLocationData(context, scope)
}
export function deliverablesPluginDispose(bench) {
  for (const dispose of [...bench.effects].reverse()) dispose()
}
export function deliverablesPluginOpened(bench) { return bench.opened }
"#)]
extern "C" {
    fn makeDeliverablesBench(language: &str, can_open_path: bool, is_loopback: bool) -> JsValue;
    fn deliverablesRender(bench: &JsValue, component: &Function) -> JsValue;
    fn deliverablesText(tree: &JsValue) -> String;
    fn deliverablesVisibleCount(tree: &JsValue) -> u32;
    fn deliverablesClickAria(tree: &JsValue, label: &str) -> bool;
    fn deliverablesTitleByAria(tree: &JsValue, label: &str) -> JsValue;
    fn deliverablesSetAvailable(bench: &JsValue, available: f64);
    fn deliverablesClearGap(bench: &JsValue);
    fn deliverablesResize(bench: &JsValue);
    fn deliverablesSetPaths(bench: &JsValue, paths: &Array);
    fn deliverablesObserveCount(bench: &JsValue) -> u32;
    fn deliverablesResetObserve(bench: &JsValue);
    fn deliverablesDisconnectCount(bench: &JsValue) -> u32;
    fn deliverablesUnmount(bench: &JsValue);
    fn deliverablesOpenCalls(bench: &JsValue) -> Array;
    fn deliverablesStyleCount() -> u32;
    fn makeDeliverablesPluginBench() -> JsValue;
    fn deliverablesPluginEntries(bench: &JsValue) -> Array;
    fn deliverablesPluginDefinitions(bench: &JsValue) -> Array;
    fn deliverablesPluginInjected(bench: &JsValue) -> JsValue;
    fn deliverablesPluginService(bench: &JsValue) -> JsValue;
    fn deliverablesPluginOwner(bench: &JsValue, produced: &JsValue) -> JsValue;
    fn deliverablesDefinitionLocation(bench: &JsValue, scope: &str) -> JsValue;
    fn deliverablesPluginDispose(bench: &JsValue);
    fn deliverablesPluginOpened(bench: &JsValue) -> Array;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn component(bench: &JsValue) -> Function {
    configure_client_ui_deliverables(property(bench, "React")).unwrap();
    exported_produced_files_component()
        .unwrap()
        .dyn_into()
        .unwrap()
}

#[wasm_bindgen_test]
fn produced_file_row_measures_resizes_opens_and_cleans_up() {
    let bench = makeDeliverablesBench("zh", true, true);
    let component = component(&bench);
    let first = deliverablesRender(&bench, &component);
    assert!(deliverablesText(&first).contains("产物"));
    assert_eq!(deliverablesVisibleCount(&first), 2);
    assert!(deliverablesText(&first).contains("+ 5 个文件"));
    assert_eq!(deliverablesObserveCount(&bench), 8);
    assert_eq!(deliverablesStyleCount(), 1);
    configure_client_ui_deliverables(property(&bench, "React")).unwrap();
    assert_eq!(deliverablesStyleCount(), 1);
    assert_eq!(
        deliverablesTitleByAria(&first, "打开 deep/a.html")
            .as_string()
            .as_deref(),
        Some("deep/a.html")
    );
    assert!(deliverablesClickAria(&first, "打开 deep/a.html"));
    assert!(deliverablesClickAria(&first, "在文件夹中显示"));
    assert_eq!(deliverablesOpenCalls(&bench).length(), 2);
    assert_eq!(
        deliverablesOpenCalls(&bench).get(1).as_string().as_deref(),
        Some(".")
    );

    deliverablesSetAvailable(&bench, 150.0);
    deliverablesResize(&bench);
    let narrow = deliverablesRender(&bench, &component);
    assert_eq!(deliverablesVisibleCount(&narrow), 1);
    assert!(deliverablesText(&narrow).contains("+ 6 个文件"));

    deliverablesClearGap(&bench);
    deliverablesSetAvailable(&bench, 165.0);
    deliverablesResize(&bench);
    let gapless = deliverablesRender(&bench, &component);
    assert_eq!(deliverablesVisibleCount(&gapless), 2);

    let one = Array::of1(&JsValue::from_str("deep/a.html"));
    deliverablesResetObserve(&bench);
    deliverablesSetPaths(&bench, &one);
    let reduced = deliverablesRender(&bench, &component);
    assert_eq!(deliverablesVisibleCount(&reduced), 1);
    assert_eq!(deliverablesObserveCount(&bench), 3);
    assert_eq!(deliverablesDisconnectCount(&bench), 1);
    deliverablesUnmount(&bench);
    assert_eq!(deliverablesDisconnectCount(&bench), 2);
}

#[wasm_bindgen_test]
fn singular_copy_and_folder_capability_boundaries_are_live() {
    let english = makeDeliverablesBench("en", false, true);
    deliverablesSetAvailable(&english, 0.0);
    let english_component = component(&english);
    let tree = deliverablesRender(&english, &english_component);
    assert_eq!(deliverablesVisibleCount(&tree), 6);
    assert!(deliverablesText(&tree).contains("+ 1 file"));
    assert!(!deliverablesClickAria(&tree, "Show in folder"));

    let remote = makeDeliverablesBench("zh", true, false);
    let remote_component = component(&remote);
    let remote_tree = deliverablesRender(&remote, &remote_component);
    assert!(!deliverablesClickAria(&remote_tree, "在文件夹中显示"));
}

#[wasm_bindgen_test]
fn plugin_registers_definition_slot_mentions_and_retracts_all_effects() {
    let bench = makeDeliverablesPluginBench();
    let component_bench = makeDeliverablesBench("en", true, true);
    configure_client_ui_deliverables(property(&component_bench, "React")).unwrap();
    apply_client_ui_deliverables(property(&bench, "ctx")).unwrap();
    assert_eq!(deliverables_inject().length(), 4);
    assert_eq!(deliverablesPluginDefinitions(&bench).length(), 1);
    assert_eq!(deliverablesPluginEntries(&bench).length(), 1);
    let injected = deliverablesPluginInjected(&bench);
    assert_eq!(property(&injected, "isLoopback").as_bool(), Some(false));
    assert!(js_sys::Object::is(
        &property(&property(&injected, "hooks"), "hostDescription"),
        &property(&bench, "hostDescription")
    ));

    let service = deliverablesPluginService(&bench);
    let owner = deliverablesPluginOwner(&bench, &JsValue::UNDEFINED);
    let resolver = property(&service, "forClosing")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&service, &owner)
        .unwrap();
    let mention = property(&resolver, "resolve")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&resolver, &JsValue::from_str("report.html"))
        .unwrap();
    assert_eq!(
        property(&mention, "label").as_string().as_deref(),
        Some("Open site/report.html")
    );
    assert_eq!(
        property(&mention, "title").as_string().as_deref(),
        Some("site/report.html")
    );
    let returned = property(&mention, "open")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&mention)
        .unwrap();
    assert!(returned.is_undefined());
    assert_eq!(
        deliverablesPluginOpened(&bench)
            .get(0)
            .as_string()
            .as_deref(),
        Some("site/report.html")
    );

    let location = deliverablesDefinitionLocation(&bench, "turn");
    assert_eq!(
        property(&location, "kind").as_string().as_deref(),
        Some("turn")
    );
    assert_eq!(property(&location, "turn").as_f64(), Some(2.0));
    assert_eq!(
        property(&location, "key").as_string().as_deref(),
        Some("deliverables")
    );
    let produced = Array::from(&property(&property(&location, "value"), "produced"));
    assert_eq!(produced.length(), 1);
    assert_eq!(
        property(&produced.get(0), "path").as_string().as_deref(),
        Some("out/report.txt")
    );
    assert!(deliverablesDefinitionLocation(&bench, "step").is_null());

    let data = js_sys::JSON::parse(
        r#"{"produced":[{"seq":3,"path":"a"},{"seq":4,"path":"b"},{"seq":5,"path":"a"}]}"#,
    )
    .unwrap();
    let paths = exported_produced_for_closing(data, Some(4.0)).unwrap();
    assert_eq!(paths.length(), 2);
    assert_eq!(paths.get(0).as_string().as_deref(), Some("a"));
    assert_eq!(paths.get(1).as_string().as_deref(), Some("b"));

    deliverablesPluginDispose(&bench);
    assert_eq!(deliverablesPluginDefinitions(&bench).length(), 0);
    assert_eq!(deliverablesPluginEntries(&bench).length(), 0);
    assert!(deliverablesPluginService(&bench).is_undefined());
}
