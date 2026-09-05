//! Live WASM coverage for `JsonTree` recursion, focus, copying, and target lifecycles.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Object, Promise, Reflect};
use seekdeep_client_ui_primitives::{configure_client_ui_primitive_json_tree, json_tree_component};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let pendingEffects = []
let dirty = false
let refs = []
let stable = new Map()
let timers = []
let now = 0
let nextTimer = 1
let windowListeners = new Map()
let styles = []
let clipboardMode = 'resolve'
let clipboardCalls = []
let rootRect = { left: 0, right: 300, top: 0, bottom: 100, width: 300, height: 100 }
let rootWidth = 300
let rootHeight = 100
let copyButtonRect = { left: 274, right: 294, top: 0, bottom: 16, width: 20, height: 16 }

const depsEqual = (left, right) => left !== undefined && right !== undefined && left.length === right.length && left.every((value, index) => Object.is(value, right[index]))
function effectHook(effect, deps) {
  const index = cursor++
  const previous = hooks[index]
  if (previous === undefined || !depsEqual(previous.deps, deps)) {
    pendingEffects.push(() => {
      previous?.cleanup?.()
      const cleanup = effect()
      hooks[index] = { deps: [...deps], cleanup: typeof cleanup === 'function' ? cleanup : undefined }
    })
  }
}
function clearRefs() {
  for (const ref of refs.splice(0)) {
    if (typeof ref === 'function') ref(null)
    else ref.current = null
  }
}
function walkChildren(node) {
  const children = [...(node.children ?? [])]
  if (node.props?.anchor !== undefined) children.push(node.props.anchor)
  return children
}
function attachRefs(node) {
  if (!(node instanceof FakeElement)) return
  const ref = node.ref ?? node.props?.ref
  if (typeof ref === 'function') { ref(node); refs.push(ref) }
  else if (ref !== null && ref !== undefined) { ref.current = node; refs.push(ref) }
  for (const child of walkChildren(node)) attachRefs(child)
}
function text(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  return walkChildren(node).map(text).join('')
}
function all(node, predicate, output = []) {
  if (node === null || node === undefined || node === false || typeof node === 'string' || typeof node === 'number') return output
  if (predicate(node)) output.push(node)
  for (const child of walkChildren(node)) all(child, predicate, output)
  return output
}
class FakeElement {
  constructor(kind, props, children) {
    this.kind = kind
    this.props = props ?? {}
    this.children = children.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false)
    this.ref = props?.ref
    this.attributes = this.attributes ?? new Map()
    this.rect = this.rect ?? { left: 0, right: 200, top: 0, bottom: 16, width: 200, height: 16 }
    for (const child of this.children) if (child instanceof FakeElement) child.parentElement = this
    if (this.props?.anchor instanceof FakeElement) this.props.anchor.parentElement = this
  }
  contains(target) { return target === this || walkChildren(this).some(child => child instanceof FakeElement && child.contains(target)) }
  setAttribute(name, value) { this.attributes.set(name, String(value)) }
  removeAttribute(name) { this.attributes.delete(name) }
  getAttribute(name) { const value = this.attributes.get(name); return value === undefined ? null : value }
  closest(selector) {
    let current = this
    while (current !== undefined) {
      if (selector === '[role="tree"]' && current.props?.role === 'tree') return current
      if (selector === '[data-json-copy-button]' && current.props?.['data-json-copy-button']) return current
      current = current.parentElement
    }
    return null
  }
  querySelectorAll(selector) {
    if (selector !== '[data-json-expander]') return []
    return all(this, node => node.props?.['data-json-expander'] === true)
  }
  focus() { document.activeElement = this; this.props?.onFocus?.() }
  getBoundingClientRect() {
    if (String(this.props?.className ?? '').split(/\s+/).includes('seekdeep-primitive-json-tree-root')) return rootRect
    if (this.props?.['data-json-copy-button']) return copyButtonRect
    return this.rect
  }
  get clientWidth() { return String(this.props?.className ?? '').includes('seekdeep-primitive-json-tree-root') ? rootWidth : 0 }
  get clientHeight() { return String(this.props?.className ?? '').includes('seekdeep-primitive-json-tree-root') ? rootHeight : 0 }
}
function stableKey(kind, props) {
  if (props?.key !== undefined) return `${typeof kind === 'function' ? 'fn' : String(kind)}:${String(props.key)}`
  if (props?.['data-json-copy-button']) return 'json-copy-button'
  return undefined
}
function createElement(kind, props, ...children) {
  const key = stableKey(kind, props)
  let node = key === undefined ? undefined : stable.get(key)
  if (node === undefined) {
    node = new FakeElement(kind, props, children)
    if (key !== undefined) stable.set(key, node)
  } else {
    node.kind = kind
    node.props = props ?? {}
    node.children = children.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false)
    node.ref = props?.ref
    for (const child of node.children) if (child instanceof FakeElement) child.parentElement = node
    if (node.props?.anchor instanceof FakeElement) node.props.anchor.parentElement = node
  }
  return node
}
function addListener(name, listener) {
  let bucket = windowListeners.get(name)
  if (bucket === undefined) windowListeners.set(name, bucket = new Set())
  bucket.add(listener)
}

export function installJsonTreeBench() {
  for (const hook of hooks) hook?.cleanup?.()
  hooks = []
  cursor = 0
  pendingEffects = []
  dirty = false
  refs = []
  stable = new Map()
  timers = []
  now = 0
  nextTimer = 1
  windowListeners = new Map()
  styles = []
  clipboardMode = 'resolve'
  clipboardCalls = []
  rootRect = { left: 0, right: 300, top: 0, bottom: 100, width: 300, height: 100 }
  rootWidth = 300
  rootHeight = 100
  copyButtonRect = { left: 274, right: 294, top: 0, bottom: 16, width: 20, height: 16 }
  globalThis.Node = FakeElement
  globalThis.Element = FakeElement
  globalThis.window = globalThis
  globalThis.setTimeout = (callback, delay) => {
    const timer = { id: nextTimer++, callback, at: now + Number(delay), active: true }
    timers.push(timer)
    return timer.id
  }
  globalThis.clearTimeout = id => {
    const timer = timers.find(candidate => candidate.id === id)
    if (timer !== undefined) timer.active = false
  }
  globalThis.addEventListener = addListener
  globalThis.removeEventListener = (name, listener) => windowListeners.get(name)?.delete(listener)
  Object.defineProperty(globalThis, 'navigator', {
    configurable: true,
    value: { clipboard: { writeText(value) {
      clipboardCalls.push(value)
      return clipboardMode === 'reject' ? Promise.reject(new Error('denied')) : Promise.resolve()
    } } },
  })
  const body = new FakeElement('body', {}, [])
  globalThis.document = {
    body,
    activeElement: body,
    head: { appendChild(node) { styles.push(node) } },
    createElement(kind) { return { kind, attributes: {}, textContent: '', setAttribute(name, value) { this.attributes[name] = value } } },
    querySelector(selector) {
      const match = selector.match(/data-plugin-css="([^"]+)"/)
      return match === null ? null : styles.find(style => style.attributes['data-plugin-css'] === match[1]) ?? null
    },
  }
  const React = {
    Fragment: 'Fragment',
    createElement,
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { value: initial }
      return [hooks[index].value, update => {
        const value = typeof update === 'function' ? update(hooks[index].value) : update
        if (!Object.is(value, hooks[index].value)) { hooks[index].value = value; dirty = true }
      }]
    },
    useRef(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { current: initial }
      return hooks[index]
    },
    useCallback(callback) { cursor += 1; return callback },
    useEffect(effect, deps) { effectHook(effect, deps) },
    useLayoutEffect(effect, deps) { effectHook(effect, deps) },
  }
  const ReactDOM = { createPortal(child, container) { return createElement('Portal', { container }, child) } }
  return { React, ReactDOM, body, styles }
}

export function jsonObject(entries) { return Object.fromEntries(entries) }
export function jsonFixture(name) {
  if (name === 'basic') return { nested: { answer: 42 }, list: ['alpha', 'beta'] }
  if (name === 'focus') return { first: { nested: 1 }, second: { nested: 2 } }
  if (name === 'array') return ['plain', { nested: true }]
  if (name === 'commas') return { parent: { emptyObject: {}, emptyArray: [], scalar: 1, last: 2 } }
  if (name === 'arrayPath') return { list: [{ value: 'x' }, 'tail'] }
  if (name === 'copy') {
    const anonymous = Object.defineProperty(() => {}, 'name', { value: '' })
    return { plain: 'hello', 'odd-key': 3, object: { a: 1 }, missing: undefined, big: 7n, symbol: Symbol(), symbolNamed: Symbol('token'), named: function named() {}, anonymous }
  }
  if (name === 'complex') {
    const anonymous = Object.defineProperty(() => {}, 'name', { value: '' })
    const date = new Date('2026-07-28T00:00:00.000Z')
    return {
      '': 'empty key', nil: null, text: 'quoted', flag: true, count: 3, big: 4n, date,
      named: function named() {}, missing: undefined, symbol: Symbol('token'), emptyObject: {}, emptyArray: [],
      primitivePreview: { nil: null, flag: false, big: 9n, missing: undefined },
      exoticPreview: { symbol: Symbol(), named: function sample() {}, anonymous, date },
      wideObject: { a: 1, b: 2, c: 3, d: 4, e: 5 }, wideArray: [1, 2, 3, 4, 5, 6], deep: { a: { b: { c: 1 } } },
    }
  }
  if (name === 'placement') return { first: { a: 1 }, second: 2 }
  if (name === 'root') return { value: 1 }
  return {}
}
export function jsonCustomLabels() {
  return {
    copyValue: 'Value', copyJson: 'JSON', copyPath: 'Path', copyPrettyJson: 'Pretty',
    copyCompactJson: 'Compact', copied: 'Done', copyFailed: 'Nope', collapseNode: 'Close',
    expandNode: 'Open', copyButtonTitle: action => 'TIP ' + action,
  }
}
export function jsonRender(component, props) {
  let tree
  for (let attempt = 0; attempt < 16; attempt++) {
    clearRefs()
    cursor = 0
    pendingEffects = []
    dirty = false
    tree = component(props)
    attachRefs(tree)
    for (const run of pendingEffects) run()
    if (!dirty) return tree
  }
  throw new Error('JsonTree hook runtime did not settle')
}
export function jsonUnmount() {
  clearRefs()
  for (const hook of hooks) hook?.cleanup?.()
  hooks = []
}
export function jsonFindRole(tree, role, label) { return all(tree, node => node.props?.role === role && (label === undefined || node.props?.['aria-label'] === label))[0] }
export function jsonAllRole(tree, role) { return all(tree, node => node.props?.role === role) }
export function jsonExpanderIndex(tree, node) { return tree.querySelectorAll('[data-json-expander]').indexOf(node) }
export function jsonExpanderLabels(tree) { return tree.querySelectorAll('[data-json-expander]').map(node => node.props?.['aria-label']) }
export function jsonFindText(tree, exact) { return all(tree, node => text(node) === exact)[0] }
export function jsonFindTextPrefix(tree, prefix) { return all(tree, node => node.props?.role === 'treeitem' && text(node).startsWith(prefix))[0] }
export function jsonFindCopyButton(tree) { return all(tree, node => node.props?.['data-json-copy-button'])[0] }
export function jsonFindMenu(tree) { return all(tree, node => typeof node.kind === 'function')[0] }
export function jsonFindClass(tree, className) { return all(tree, node => String(node.props?.className ?? '').split(/\s+/).includes(className))[0] }
export function jsonRoot(tree) { return all(tree, node => String(node.props?.className ?? '').split(/\s+/).includes('seekdeep-primitive-json-tree-root'))[0] }
export function jsonRootRow(tree) { return all(tree, node => node.props?.['data-json-root-row'])[0] }
export function jsonText(tree) { return text(tree) }
export function jsonClick(node) { node.props?.onClick?.({ currentTarget: node, preventDefault() {}, stopPropagation() {} }) }
export function jsonContextMenu(node) { node.props?.onContextMenu?.({ currentTarget: node, preventDefault() {}, stopPropagation() {} }) }
export function jsonMouseOver(node, target) { node.props?.onMouseOver?.({ currentTarget: node, target: target ?? node, stopPropagation() {} }) }
export function jsonMouseLeave(node) { node.props?.onMouseLeave?.() }
export function jsonKey(node, key) { let prevented = false; node.props?.onKeyDown?.({ key, currentTarget: node, preventDefault() { prevented = true } }); return prevented }
export function jsonScroll(node) { node.props?.onScroll?.({ currentTarget: node }) }
export function jsonSelectMenu(tree, id) { jsonFindMenu(tree).props.onSelect(id) }
export function jsonCloseMenu(tree) { jsonFindMenu(tree).props.onClose() }
export function jsonMenuItems(tree) { return jsonFindMenu(tree).props.items }
export function jsonMenuOpen(tree) { return jsonFindMenu(tree).props.open }
export function jsonSetClipboardMode(mode) { clipboardMode = mode }
export function jsonClipboardCalls() { return clipboardCalls }
export function jsonTick() { return Promise.resolve().then(() => Promise.resolve()).then(() => Promise.resolve()) }
export function jsonAdvance(milliseconds) {
  const target = now + milliseconds
  while (true) {
    const timer = timers.filter(candidate => candidate.active && candidate.at <= target).sort((left, right) => left.at - right.at || left.id - right.id)[0]
    if (timer === undefined) break
    now = timer.at
    timer.active = false
    timer.callback()
  }
  now = target
}
export function jsonTimerCount() { return timers.filter(timer => timer.active).length }
export function jsonSetGeometry(left, width, height) { rootRect = { left, right: left + width, top: 0, bottom: height, width, height }; rootWidth = width; rootHeight = height }
export function jsonSetRowRect(node, top) { node.rect = { left: rootRect.left, right: rootRect.right, top, bottom: top + 16, width: rootRect.width, height: 16 } }
export function jsonDispatchWindow(name) { for (const listener of windowListeners.get(name) ?? []) listener() }
export function jsonListenerCount(name) { return windowListeners.get(name)?.size ?? 0 }
export function jsonStyle(node, key) { return node?.props?.style?.[key] }
export function jsonAttribute(node, key) { return node?.getAttribute?.(key) }
export function jsonActiveElement() { return document.activeElement }
export function jsonStyles() { return styles }
"#)]
extern "C" {
    fn installJsonTreeBench() -> JsValue;
    fn jsonObject(entries: &Array) -> JsValue;
    fn jsonFixture(name: &str) -> JsValue;
    fn jsonCustomLabels() -> JsValue;
    fn jsonRender(component: &JsValue, props: &JsValue) -> JsValue;
    fn jsonUnmount();
    fn jsonFindRole(tree: &JsValue, role: &str, label: &JsValue) -> JsValue;
    fn jsonAllRole(tree: &JsValue, role: &str) -> Array;
    fn jsonExpanderIndex(tree: &JsValue, node: &JsValue) -> i32;
    fn jsonExpanderLabels(tree: &JsValue) -> Array;
    fn jsonFindText(tree: &JsValue, exact: &str) -> JsValue;
    fn jsonFindTextPrefix(tree: &JsValue, prefix: &str) -> JsValue;
    fn jsonFindCopyButton(tree: &JsValue) -> JsValue;
    fn jsonFindMenu(tree: &JsValue) -> JsValue;
    fn jsonFindClass(tree: &JsValue, class_name: &str) -> JsValue;
    fn jsonRoot(tree: &JsValue) -> JsValue;
    fn jsonRootRow(tree: &JsValue) -> JsValue;
    fn jsonText(tree: &JsValue) -> String;
    fn jsonClick(node: &JsValue);
    fn jsonContextMenu(node: &JsValue);
    fn jsonMouseOver(node: &JsValue, target: &JsValue);
    fn jsonMouseLeave(node: &JsValue);
    fn jsonKey(node: &JsValue, key: &str) -> bool;
    fn jsonScroll(node: &JsValue);
    fn jsonSelectMenu(tree: &JsValue, id: &str);
    fn jsonCloseMenu(tree: &JsValue);
    fn jsonMenuItems(tree: &JsValue) -> Array;
    fn jsonMenuOpen(tree: &JsValue) -> bool;
    fn jsonSetClipboardMode(mode: &str);
    fn jsonClipboardCalls() -> Array;
    fn jsonTick() -> Promise;
    fn jsonAdvance(milliseconds: f64);
    fn jsonTimerCount() -> u32;
    fn jsonSetGeometry(left: f64, width: f64, height: f64);
    fn jsonSetRowRect(node: &JsValue, top: f64);
    fn jsonDispatchWindow(name: &str);
    fn jsonListenerCount(name: &str) -> u32;
    fn jsonStyle(node: &JsValue, key: &str) -> JsValue;
    fn jsonAttribute(node: &JsValue, key: &str) -> JsValue;
    fn jsonActiveElement() -> JsValue;
    fn jsonStyles() -> Array;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    jsonObject(&array).unchecked_into()
}

fn props(data: JsValue, entries: &[(&str, JsValue)]) -> Object {
    let output = object(&[("data", data)]);
    for (key, value) in entries {
        Reflect::set(&output, &JsValue::from_str(key), value).unwrap();
    }
    output
}

fn setup() -> (JsValue, JsValue) {
    let bench = installJsonTreeBench();
    configure_client_ui_primitive_json_tree(
        property(&bench, "React"),
        property(&bench, "ReactDOM"),
    )
    .unwrap();
    (bench, json_tree_component().unwrap())
}

fn render(component: &JsValue, props: &Object) -> JsValue {
    jsonRender(component, props.as_ref())
}

fn role(tree: &JsValue, name: &str, label: Option<&str>) -> JsValue {
    jsonFindRole(
        tree,
        name,
        &label.map_or(JsValue::UNDEFINED, JsValue::from_str),
    )
}

async fn tick() {
    JsFuture::from(jsonTick()).await.unwrap();
}

#[wasm_bindgen_test]
fn top_level_previews_expansion_commas_and_empty_root_match_source() {
    let (_bench, component) = setup();
    assert_eq!(jsonStyles().length(), 2);
    let basic = props(
        jsonFixture("basic"),
        &[("label", JsValue::from_str("Payload"))],
    );
    let tree = render(&component, &basic);
    let role_tree = role(&tree, "tree", Some("Payload"));
    assert_eq!(jsonAllRole(&role_tree, "treeitem").length(), 2);
    assert_eq!(
        jsonText(&jsonAllRole(&role_tree, "treeitem").get(0)),
        "nested:{answer: 42},"
    );
    assert_eq!(
        jsonText(&jsonAllRole(&role_tree, "treeitem").get(1)),
        "list:[\"alpha\", \"beta\"]"
    );
    let expanders = jsonAllRole(&role_tree, "button");
    assert_eq!(
        property(&property(&expanders.get(0), "props"), "tabIndex").as_f64(),
        Some(0.0)
    );
    assert_eq!(
        property(&property(&expanders.get(1), "props"), "tabIndex").as_f64(),
        Some(-1.0)
    );
    jsonClick(&expanders.get(0));
    let expanded = render(&component, &basic);
    assert_eq!(
        jsonAllRole(&role(&expanded, "tree", Some("Payload")), "treeitem").length(),
        3
    );
    assert!(!jsonFindText(&expanded, "answer:").is_undefined());
    assert!(!role(&expanded, "button", Some("Collapse JSON node")).is_undefined());

    jsonUnmount();
    let (_bench, component) = setup();
    let commas = props(jsonFixture("commas"), &[]);
    let first = render(&component, &commas);
    jsonClick(&jsonFindText(&first, "parent:"));
    let expanded = render(&component, &commas);
    for expected in ["emptyObject:{},", "emptyArray:[],", "scalar:1,", "last:2"] {
        assert!(
            !jsonFindTextPrefix(&expanded, expected).is_undefined(),
            "{expected}"
        );
    }
    jsonClick(&jsonFindText(&expanded, "parent:"));
    assert_eq!(
        jsonAllRole(
            &role(&render(&component, &commas), "tree", None),
            "treeitem"
        )
        .length(),
        1
    );

    jsonUnmount();
    let (_bench, component) = setup();
    let array = props(jsonFixture("array"), &[]);
    let tree = render(&component, &array);
    assert!(jsonText(&role(&tree, "tree", None)).contains("0:\"plain\""));
    assert_eq!(
        property(
            &property(&role(&tree, "button", Some("Expand JSON node")), "props"),
            "tabIndex"
        )
        .as_f64(),
        Some(0.0)
    );
    jsonUnmount();

    let (_bench, component) = setup();
    let empty = props(jsonFixture("empty"), &[("expandTopLevel", JsValue::FALSE)]);
    let tree = render(&component, &empty);
    assert_eq!(jsonText(&role(&tree, "tree", None)), "{}");
    assert!(role(&tree, "button", Some("Collapse JSON node")).is_undefined());
    jsonUnmount();
}

#[wasm_bindgen_test]
fn roving_focus_and_arrow_expansion_match_source() {
    let (_bench, component) = setup();
    let props = props(jsonFixture("focus"), &[("expandTopLevel", JsValue::FALSE)]);
    let tree = render(&component, &props);
    let root = role(&tree, "button", Some("Collapse JSON node"));
    let children = jsonAllRole(&tree, "button");
    assert_eq!(
        property(&property(&root, "props"), "tabIndex").as_f64(),
        Some(0.0)
    );
    assert!(jsonKey(&root, "ArrowDown"));
    let tree = render(&component, &props);
    let child = role(&tree, "button", Some("Expand JSON node"));
    assert!(Object::is(&jsonActiveElement(), &child));
    assert_eq!(
        property(&property(&child, "props"), "tabIndex").as_f64(),
        Some(0.0)
    );
    assert!(jsonKey(&child, "ArrowRight"));
    let tree = render(&component, &props);
    let child = jsonAllRole(&tree, "button").get(1);
    assert_eq!(
        property(&property(&child, "props"), "aria-expanded").as_bool(),
        Some(true)
    );
    assert!(jsonKey(&child, "ArrowLeft"));
    let tree = render(&component, &props);
    let child = jsonAllRole(&tree, "button").get(1);
    assert_eq!(
        property(&property(&child, "props"), "aria-expanded").as_bool(),
        Some(false)
    );
    assert!(!jsonKey(&child, "Enter"));
    let role_tree = role(&tree, "tree", None);
    assert_eq!(
        jsonExpanderLabels(&role_tree)
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        ["Collapse JSON node", "Expand JSON node", "Expand JSON node"]
    );
    assert_eq!(jsonExpanderIndex(&role_tree, &child), 1);
    assert!(jsonKey(&child, "ArrowUp"));
    assert_eq!(
        property(&property(&jsonActiveElement(), "props"), "aria-label")
            .as_string()
            .as_deref(),
        Some("Collapse JSON node")
    );
    let tree = render(&component, &props);
    let root = role(&tree, "button", Some("Collapse JSON node"));
    assert_eq!(
        property(&property(&root, "props"), "tabIndex").as_f64(),
        Some(0.0)
    );
    assert!(jsonKey(&root, "ArrowUp"));
    assert_eq!(
        property(&property(&jsonActiveElement(), "props"), "aria-label")
            .as_string()
            .as_deref(),
        Some("Expand JSON node")
    );
    let tree = render(&component, &props);
    let expanders = jsonAllRole(&tree, "button");
    assert_eq!(
        property(
            &property(&expanders.get(expanders.length() - 1), "props"),
            "tabIndex"
        )
        .as_f64(),
        Some(0.0)
    );
    assert!(children.length() >= 3);
    jsonUnmount();
}

#[wasm_bindgen_test]
fn complex_primitives_empty_containers_and_bounded_previews_match_source() {
    let (_bench, component) = setup();
    let props = props(jsonFixture("complex"), &[("copyable", JsValue::FALSE)]);
    let tree = render(&component, &props);
    let text = jsonText(&role(&tree, "tree", None));
    for expected in [
        "\"\":\"empty key\"",
        "nil:null",
        "flag:true",
        "count:3",
        "big:4n",
        "date:2026-07-28T00:00:00.000Z",
        "named:function() { }",
        "missing:undefined",
        "symbol:Symbol(token)",
        "emptyObject:{}",
        "emptyArray:[]",
        "primitivePreview:{nil: null, flag: false, big: 9, missing: undefined}",
        "exoticPreview:{symbol: Symbol, named: sample, anonymous: Function, date: }",
        "wideObject:{a: 1, b: 2, c: 3, d: 4, …}",
        "wideArray:[1, 2, 3, 4, 5, …]",
        "deep:{a: {b: {…}}}",
    ] {
        assert!(text.contains(expected), "{expected}\n{text}");
    }
    jsonMouseOver(&jsonAllRole(&tree, "treeitem").get(0), &JsValue::UNDEFINED);
    assert!(jsonFindCopyButton(&render(&component, &props)).is_undefined());
    jsonUnmount();
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn array_path_and_every_copy_mode_match_source() {
    let (_bench, component) = setup();
    let array_props = props(jsonFixture("arrayPath"), &[]);
    let tree = render(&component, &array_props);
    jsonClick(&role(&tree, "button", Some("Expand JSON node")));
    let tree = render(&component, &array_props);
    let row = jsonFindTextPrefix(&tree, "0:");
    jsonMouseOver(&row, &JsValue::UNDEFINED);
    let tree = render(&component, &array_props);
    jsonContextMenu(&jsonFindCopyButton(&tree));
    let tree = render(&component, &array_props);
    assert!(jsonMenuOpen(&tree));
    jsonSelectMenu(&tree, "path");
    tick().await;
    assert_eq!(
        jsonClipboardCalls().get(0).as_string().as_deref(),
        Some("$.list[0]")
    );
    jsonUnmount();

    let (_bench, component) = setup();
    let copy_props = props(jsonFixture("copy"), &[]);
    let mut tree = render(&component, &copy_props);
    let copy = |tree: &JsValue, prefix: &str| {
        let row = jsonFindTextPrefix(tree, prefix);
        jsonMouseOver(&row, &JsValue::UNDEFINED);
    };
    copy(&tree, "plain:");
    tree = render(&component, &copy_props);
    jsonClick(&jsonFindCopyButton(&tree));
    tick().await;
    assert_eq!(
        jsonClipboardCalls().get(0).as_string().as_deref(),
        Some("hello")
    );

    for (prefix, mode, expected) in [
        ("odd-key:", "path", "$[\"odd-key\"]"),
        ("odd-key:", "json", "3"),
        ("object:", "prettyJson", "{\n  \"a\": 1\n}"),
        ("object:", "json", "{\"a\":1}"),
    ] {
        tree = render(&component, &copy_props);
        copy(&tree, prefix);
        tree = render(&component, &copy_props);
        jsonContextMenu(&jsonFindCopyButton(&tree));
        tree = render(&component, &copy_props);
        let labels = jsonMenuItems(&tree)
            .iter()
            .filter_map(|item| property(&item, "label").as_string())
            .collect::<Vec<_>>();
        assert_eq!(labels.len(), 3);
        jsonSelectMenu(&tree, mode);
        tick().await;
        assert_eq!(
            jsonClipboardCalls()
                .get(jsonClipboardCalls().length() - 1)
                .as_string()
                .as_deref(),
            Some(expected)
        );
    }
    for (prefix, expected) in [
        ("missing:", "undefined"),
        ("big:", "7"),
        ("symbol:", "Symbol"),
        ("symbolNamed:", "token"),
        ("named:", "named"),
        ("anonymous:", "Function"),
    ] {
        tree = render(&component, &copy_props);
        copy(&tree, prefix);
        tree = render(&component, &copy_props);
        jsonClick(&jsonFindCopyButton(&tree));
        tick().await;
        assert_eq!(
            jsonClipboardCalls()
                .get(jsonClipboardCalls().length() - 1)
                .as_string()
                .as_deref(),
            Some(expected)
        );
    }
    jsonUnmount();
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn feedback_positioning_menu_lock_stale_data_and_root_copy_match_source() {
    let (_bench, component) = setup();
    jsonSetClipboardMode("reject");
    let failure_props = props(jsonFixture("root"), &[]);
    let tree = render(&component, &failure_props);
    let row = jsonAllRole(&tree, "treeitem").get(0);
    jsonMouseOver(&row, &JsValue::UNDEFINED);
    let tree = render(&component, &failure_props);
    jsonClick(&jsonFindCopyButton(&tree));
    tick().await;
    let failed = render(&component, &failure_props);
    assert_eq!(
        property(
            &property(&jsonFindCopyButton(&failed), "props"),
            "aria-label"
        )
        .as_string()
        .as_deref(),
        Some("Copy failed")
    );
    jsonClick(&jsonFindCopyButton(&failed));
    tick().await;
    assert_eq!(jsonTimerCount(), 1);
    jsonAdvance(1_500.0);
    let idle = render(&component, &failure_props);
    assert_eq!(
        property(&property(&jsonFindCopyButton(&idle), "props"), "aria-label")
            .as_string()
            .as_deref(),
        Some("Copy value")
    );
    jsonUnmount();
    assert_eq!(jsonTimerCount(), 0);

    let (_bench, component) = setup();
    let placement = props(jsonFixture("placement"), &[]);
    jsonSetGeometry(10.0, 300.0, 100.0);
    let tree = render(&component, &placement);
    let first = jsonAllRole(&tree, "treeitem").get(0);
    let second = jsonAllRole(&tree, "treeitem").get(1);
    jsonSetRowRect(&first, 75.0);
    jsonSetRowRect(&second, 20.0);
    jsonMouseOver(&first, &JsValue::UNDEFINED);
    let tree = render(&component, &placement);
    let copy_button = jsonFindCopyButton(&tree);
    assert_eq!(
        jsonStyle(
            &jsonFindClass(&tree, "seekdeep-primitive-json-tree-copyAnchor"),
            "left"
        )
        .as_f64(),
        Some(284.0)
    );
    assert_eq!(
        property(&property(&jsonFindMenu(&tree), "props"), "side")
            .as_string()
            .as_deref(),
        Some("top")
    );
    jsonScroll(&jsonRoot(&tree));
    jsonDispatchWindow("scroll");
    jsonDispatchWindow("resize");
    assert_eq!(jsonListenerCount("scroll"), 1);
    jsonContextMenu(&copy_button);
    jsonMouseOver(&second, &JsValue::UNDEFINED);
    jsonMouseOver(&jsonRoot(&tree), &jsonRoot(&tree));
    jsonMouseLeave(&jsonRoot(&tree));
    let tree = render(&component, &placement);
    assert!(jsonMenuOpen(&tree));
    jsonCloseMenu(&tree);
    assert!(jsonFindCopyButton(&render(&component, &placement)).is_undefined());

    jsonMouseOver(&second, &JsValue::UNDEFINED);
    let tree = render(&component, &placement);
    assert!(!jsonFindCopyButton(&tree).is_undefined());
    jsonMouseOver(&jsonRoot(&tree), &jsonRoot(&tree));
    assert!(jsonFindCopyButton(&render(&component, &placement)).is_undefined());

    let replacement = jsonFixture("empty");
    Reflect::set(&placement, &JsValue::from_str("data"), &replacement).unwrap();
    assert!(jsonFindCopyButton(&render(&component, &placement)).is_undefined());
    jsonUnmount();
    assert_eq!(jsonListenerCount("scroll"), 0);

    let (_bench, component) = setup();
    let root_props = props(jsonFixture("root"), &[]);
    let tree = render(&component, &root_props);
    let opening = jsonRootRow(&tree);
    jsonMouseOver(&opening, &JsValue::UNDEFINED);
    let tree = render(&component, &root_props);
    jsonClick(&jsonFindCopyButton(&tree));
    tick().await;
    assert_eq!(
        jsonClipboardCalls().get(0).as_string().as_deref(),
        Some("{\n  \"value\": 1\n}")
    );
    jsonMouseLeave(&jsonRoot(&tree));
    assert!(jsonFindCopyButton(&render(&component, &root_props)).is_undefined());
    jsonUnmount();
}

#[wasm_bindgen_test(async)]
async fn localized_labels_class_name_and_expand_mode_changes_preserve_the_public_contract() {
    let (_bench, component) = setup();
    let props = props(
        jsonFixture("basic"),
        &[
            ("expandTopLevel", JsValue::FALSE),
            ("className", JsValue::from_str("caller")),
            ("labels", jsonCustomLabels()),
        ],
    );
    let tree = render(&component, &props);
    assert!(
        property(&property(&jsonRoot(&tree), "props"), "className")
            .as_string()
            .unwrap()
            .contains("caller")
    );
    assert!(!role(&tree, "button", Some("Close")).is_undefined());

    Reflect::set(&props, &JsValue::from_str("expandTopLevel"), &JsValue::TRUE).unwrap();
    let tree = render(&component, &props);
    assert_eq!(jsonAllRole(&tree, "treeitem").length(), 2);
    assert_eq!(jsonAllRole(&tree, "button").length(), 2);
    assert!(role(&tree, "button", Some("Close")).is_undefined());
    let primitive = jsonFindTextPrefix(&tree, "list:");
    jsonMouseOver(&primitive, &JsValue::UNDEFINED);
    let tree = render(&component, &props);
    let button = jsonFindCopyButton(&tree);
    assert_eq!(
        property(&property(&button, "props"), "aria-label")
            .as_string()
            .as_deref(),
        Some("Pretty")
    );
    assert_eq!(
        property(&property(&button, "props"), "title")
            .as_string()
            .as_deref(),
        Some("TIP Pretty")
    );
    jsonContextMenu(&button);
    let tree = render(&component, &props);
    assert_eq!(
        jsonMenuItems(&tree)
            .iter()
            .filter_map(|item| property(&item, "label").as_string())
            .collect::<Vec<_>>(),
        ["Pretty", "Compact", "Path"]
    );
    jsonCloseMenu(&tree);

    let tree = render(&component, &props);
    jsonMouseOver(&jsonFindTextPrefix(&tree, "nested:"), &JsValue::UNDEFINED);
    let tree = render(&component, &props);
    jsonClick(&jsonFindCopyButton(&tree));
    tick().await;
    assert_eq!(
        property(
            &property(&jsonFindCopyButton(&render(&component, &props)), "props"),
            "aria-label"
        )
        .as_string()
        .as_deref(),
        Some("Done")
    );
    jsonUnmount();
}
