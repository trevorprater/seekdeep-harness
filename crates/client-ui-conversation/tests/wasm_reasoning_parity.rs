//! Live WASM coverage for frame throttling and compiled assistant reasoning rows.

#![cfg(target_arch = "wasm32")]

use std::{cell::Cell, rc::Rc};

use js_sys::{Function, Object, Reflect};
use seekdeep_client_ui_conversation::{
    configure_client_ui_conversation_reasoning, reasoning_row_component,
    use_throttled_visual_update,
};
use seekdeep_client_ui_primitives::{
    configure_client_ui_primitive_atoms, configure_client_ui_primitive_dialogs,
    configure_client_ui_primitive_icons, disclosure_row_component, icon_components,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let queuedLayout = []
let queuedEffects = []
let frames = new Map()
let nextFrame = 1
let styles = []
let tree = null

function sameDeps(left, right) {
  return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index]))
}
function text(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  return (node.children ?? []).map(text).join('')
}
function all(node, predicate, output = []) {
  if (node === null || node === undefined || node === false || typeof node === 'string' || typeof node === 'number') return output
  if (predicate(node)) output.push(node)
  for (const child of node.children ?? []) all(child, predicate, output)
  return output
}
function resolve(node) {
  if (node === null || node === undefined || node === false || typeof node === 'string' || typeof node === 'number') return node
  if (typeof node.kind === 'function') {
    const children = node.children.length === 0 ? undefined : node.children.length === 1 ? node.children[0] : node.children
    return resolve(node.kind({ ...node.props, children }))
  }
  return { ...node, children: (node.children ?? []).map(resolve) }
}
function reconcile(previous, next) {
  if (previous === null || next === null || typeof previous !== 'object' || typeof next !== 'object' || previous.kind !== next.kind) return next
  previous.props = next.props
  const length = Math.max(previous.children.length, next.children.length)
  const children = []
  for (let index = 0; index < length; index += 1) {
    if (index < next.children.length) children.push(reconcile(previous.children[index], next.children[index]))
  }
  previous.children = children
  return previous
}
function attachRefs(node) {
  if (node === null || node === undefined || typeof node !== 'object') return
  if (node.props?.ref) node.props.ref.current = node
  for (const child of node.children ?? []) attachRefs(child)
}
function runQueue(queue) {
  for (const item of queue.splice(0)) {
    const cleanup = item.effect()
    if (typeof cleanup === 'function') hooks[item.index].cleanup = cleanup
  }
}
function effectHook(effect, dependencies, queue) {
  const index = cursor++
  if (!(index in hooks) || !sameDeps(hooks[index].dependencies, dependencies)) {
    hooks[index]?.cleanup?.()
    hooks[index] = { dependencies: [...dependencies], cleanup: undefined }
    queue.push({ index, effect })
  }
}

export function installReasoningBench() {
  hooks = []
  cursor = 0
  queuedLayout = []
  queuedEffects = []
  frames = new Map()
  nextFrame = 1
  styles = []
  tree = null
  globalThis.requestAnimationFrame = callback => { const id = nextFrame++; frames.set(id, callback); return id }
  globalThis.cancelAnimationFrame = id => { frames.delete(Number(id)) }
  globalThis.document = {
    head: { appendChild(node) { styles.push(node) } },
    createElement(kind) { return { kind, attributes: {}, setAttribute(name, value) { this.attributes[name] = value } } },
    querySelector(selector) {
      const match = selector.match(/data-plugin-css="([^"]+)"/)
      return match === null ? null : styles.find(style => style.attributes['data-plugin-css'] === match[1]) ?? null
    },
  }
  const React = {
    Fragment: Symbol('Fragment'),
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children: children.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false), scrollLeft: 0, scrollWidth: 0, clientWidth: 0 } },
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { value: initial }
      return [hooks[index].value, update => { hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update }]
    },
    useRef(initial) { const index = cursor++; if (!(index in hooks)) hooks[index] = { current: initial }; return hooks[index] },
    useCallback(callback, dependencies) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].dependencies, dependencies)) hooks[index] = { value: callback, dependencies: [...dependencies] }
      return hooks[index].value
    },
    useLayoutEffect(effect, dependencies) { effectHook(effect, dependencies, queuedLayout) },
    useEffect(effect, dependencies) { effectHook(effect, dependencies, queuedEffects) },
  }
  const ReactDOM = { createPortal(child) { return child } }
  return { React, ReactDOM, uiPrimitives: { DisclosureRow: 'DisclosureRow', IconThinkOutline14: 'Think' } }
}
export function reasoningRender(component, props) {
  cursor = 0
  queuedLayout = []
  queuedEffects = []
  const next = resolve(component(props))
  tree = reconcile(tree, next)
  attachRefs(tree)
  runQueue(queuedLayout)
  runQueue(queuedEffects)
  return tree
}
export function reasoningHookReset() { cursor = 0; queuedLayout = []; queuedEffects = [] }
export function reasoningFlushHookEffects() { runQueue(queuedLayout); runQueue(queuedEffects) }
export function reasoningUnmount() { for (const hook of hooks) hook?.cleanup?.(); hooks = []; tree = null }
export function reasoningFlushFrames(count) {
  for (let index = 0; index < count; index += 1) {
    const callbacks = [...frames.values()]
    frames.clear()
    for (const callback of callbacks) callback(index)
  }
}
export function reasoningPendingFrames() { return frames.size }
export function reasoningText(node) { return text(node) }
export function reasoningFindClass(root, className) { return all(root, node => String(node.props?.className ?? '').split(/\s+/).includes(className))[0] }
export function reasoningFindKind(root, kind) { return all(root, node => node.kind === kind)[0] }
export function reasoningFindRole(root, role) { return all(root, node => node.props?.role === role)[0] }
export function reasoningClick(node) { node.props.onClick() }
export function reasoningSetGeometry(node, scrollWidth, clientWidth) { node.scrollWidth = scrollWidth; node.clientWidth = clientWidth }
export function reasoningStyles() { return styles }
export function reasoningTranslate(key) { return key === 'row.running' ? '运行中' : key }
"#)]
extern "C" {
    fn installReasoningBench() -> JsValue;
    fn reasoningRender(component: &JsValue, props: &JsValue) -> JsValue;
    fn reasoningHookReset();
    fn reasoningFlushHookEffects();
    fn reasoningUnmount();
    fn reasoningFlushFrames(count: u32);
    fn reasoningPendingFrames() -> u32;
    fn reasoningText(node: &JsValue) -> String;
    fn reasoningFindClass(root: &JsValue, class_name: &str) -> JsValue;
    fn reasoningFindKind(root: &JsValue, kind: &str) -> JsValue;
    fn reasoningFindRole(root: &JsValue, role: &str) -> JsValue;
    fn reasoningClick(node: &JsValue);
    fn reasoningSetGeometry(node: &JsValue, scroll_width: f64, client_width: f64);
    fn reasoningStyles() -> js_sys::Array;
    fn reasoningTranslate(key: &str) -> String;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn props(text: &str, running: bool) -> Object {
    let props = Object::new();
    Reflect::set(&props, &JsValue::from_str("text"), &JsValue::from_str(text)).unwrap();
    Reflect::set(
        &props,
        &JsValue::from_str("running"),
        &JsValue::from_bool(running),
    )
    .unwrap();
    let translate =
        Closure::wrap(Box::new(move |key: String| reasoningTranslate(&key))
            as Box<dyn FnMut(String) -> String>);
    Reflect::set(&props, &JsValue::from_str("t"), &translate.into_js_value()).unwrap();
    props
}

fn setup() -> (JsValue, JsValue, u32) {
    let bench = installReasoningBench();
    let react = property(&bench, "React");
    let react_dom = property(&bench, "ReactDOM");
    configure_client_ui_primitive_atoms(react.clone(), react_dom.clone()).unwrap();
    configure_client_ui_primitive_dialogs(react.clone(), react_dom).unwrap();
    configure_client_ui_primitive_icons(react.clone());
    let base_styles = reasoningStyles().length();
    let primitives = Object::new();
    Reflect::set(
        &primitives,
        &JsValue::from_str("DisclosureRow"),
        &disclosure_row_component().unwrap(),
    )
    .unwrap();
    Reflect::set(
        &primitives,
        &JsValue::from_str("IconThinkOutline14"),
        &property(&icon_components().unwrap(), "IconThinkOutline14"),
    )
    .unwrap();
    configure_client_ui_conversation_reasoning(react, primitives.into()).unwrap();
    (bench, reasoning_row_component().unwrap(), base_styles)
}

#[wasm_bindgen_test]
fn frame_scheduler_coalesces_uses_latest_callback_and_cleans_up() {
    let (_bench, _component, _base_styles) = setup();
    let first_calls = Rc::new(Cell::new(0_u32));
    let observed = first_calls.clone();
    let first =
        Closure::wrap(Box::new(move || observed.set(observed.get() + 1)) as Box<dyn FnMut()>)
            .into_js_value()
            .dyn_into::<Function>()
            .unwrap();
    reasoningHookReset();
    let scheduler = use_throttled_visual_update(first, None).unwrap();
    reasoningFlushHookEffects();
    scheduler.call0(&JsValue::UNDEFINED).unwrap();
    scheduler.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(reasoningPendingFrames(), 1);
    reasoningFlushFrames(2);
    assert_eq!(first_calls.get(), 0);

    let latest_calls = Rc::new(Cell::new(0_u32));
    let observed = latest_calls.clone();
    let latest =
        Closure::wrap(Box::new(move || observed.set(observed.get() + 1)) as Box<dyn FnMut()>)
            .into_js_value()
            .dyn_into::<Function>()
            .unwrap();
    reasoningHookReset();
    let stable = use_throttled_visual_update(latest, None).unwrap();
    reasoningFlushHookEffects();
    assert!(Object::is(&scheduler, &stable));
    reasoningFlushFrames(1);
    assert_eq!(first_calls.get(), 0);
    assert_eq!(latest_calls.get(), 1);

    stable.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(reasoningPendingFrames(), 1);
    reasoningUnmount();
    assert_eq!(reasoningPendingFrames(), 0);
}

#[wasm_bindgen_test]
fn running_summary_follows_latest_line_then_restores_settled_first_line() {
    let (_bench, component, base_styles) = setup();
    assert_eq!(reasoningStyles().length(), base_styles + 2);
    let tree = reasoningRender(
        &component,
        props("Inspect the session\nNewest reasoning tokens", true).as_ref(),
    );
    assert_eq!(
        property(&property(&tree, "props"), "data-state")
            .as_string()
            .as_deref(),
        Some("running")
    );
    assert!(reasoningText(&tree).contains("运行中"));
    let summary = reasoningFindClass(&tree, "seekdeep-conversation-reasoning-summary");
    assert_eq!(reasoningText(&summary), "Newest reasoning tokens");
    reasoningSetGeometry(&summary, 300.0, 100.0);

    let tree = reasoningRender(
        &component,
        props(
            "Inspect the session\nNewest reasoning tokens keep arriving",
            true,
        )
        .as_ref(),
    );
    let summary = reasoningFindClass(&tree, "seekdeep-conversation-reasoning-summary");
    assert_eq!(property(&summary, "scrollLeft").as_f64(), Some(0.0));
    reasoningFlushFrames(2);
    assert_eq!(property(&summary, "scrollLeft").as_f64(), Some(0.0));
    reasoningFlushFrames(1);
    assert_eq!(property(&summary, "scrollLeft").as_f64(), Some(200.0));
    assert_eq!(
        property(&property(&summary, "props"), "data-follow-end").as_bool(),
        Some(true)
    );

    let tree = reasoningRender(
        &component,
        props(
            "Inspect the session\nNewest reasoning tokens keep arriving\n",
            false,
        )
        .as_ref(),
    );
    reasoningFlushFrames(3);
    let summary = reasoningFindClass(&tree, "seekdeep-conversation-reasoning-summary");
    assert_eq!(reasoningText(&summary), "Inspect the session");
    assert_eq!(property(&summary, "scrollLeft").as_f64(), Some(0.0));
    assert!(property(&property(&summary, "props"), "data-follow-end").is_undefined());
    assert!(!reasoningText(&tree).contains("运行中"));
    reasoningUnmount();
}

#[wasm_bindgen_test]
fn disclosure_expands_and_collapses_plain_reasoning_body() {
    let (_bench, component, _base_styles) = setup();
    let props = props("Inspect the session\nCheck persistence", false);
    let tree = reasoningRender(&component, props.as_ref());
    let button = reasoningFindRole(&tree, "button");
    assert!(
        !button.is_undefined(),
        "initial tree omitted disclosure button: {}",
        reasoningText(&tree)
    );
    assert_eq!(
        property(&property(&button, "props"), "aria-expanded").as_bool(),
        Some(false)
    );
    reasoningClick(&button);
    let tree = reasoningRender(&component, props.as_ref());
    let button = reasoningFindRole(&tree, "button");
    assert!(
        !button.is_undefined(),
        "expanded tree omitted disclosure button: {}",
        reasoningText(&tree)
    );
    assert_eq!(
        property(&property(&button, "props"), "aria-expanded").as_bool(),
        Some(true)
    );
    assert_eq!(
        reasoningText(&reasoningFindClass(
            &tree,
            "seekdeep-conversation-reasoning-thinkBody"
        )),
        "Inspect the session\nCheck persistence"
    );
    assert!(reasoningFindClass(&tree, "seekdeep-conversation-reasoning-summary").is_undefined());
    assert!(!reasoningText(&tree).contains("IN"));
    reasoningClick(&button);
    let tree = reasoningRender(&component, props.as_ref());
    let button = reasoningFindRole(&tree, "button");
    assert!(
        !button.is_undefined(),
        "collapsed tree omitted disclosure button: {}",
        reasoningText(&tree)
    );
    assert_eq!(
        property(&property(&button, "props"), "aria-expanded").as_bool(),
        Some(false)
    );
    reasoningUnmount();
}
