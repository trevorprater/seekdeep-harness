//! Live WASM coverage for `HoverCard` timing, portal, copy, and placement lifecycles.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Object, Promise, Reflect};
use seekdeep_client_ui_primitives::{
    POINTER_GRACE_MS, configure_client_ui_primitive_hover_card, hover_card_component,
};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let pendingLayout = []
let pendingEffects = []
let dirty = false
let attachedRefs = []
let timers = []
let now = 0
let nextTimer = 1
let listeners = new Map()
let styles = []
let anchorRect = { top: 40, right: 200, left: 100, bottom: 74, width: 100, height: 34 }
let cardHeight = 0
let stableCard = null
let clipboardMode = 'resolve'
let clipboardCalls = []
let clipboardResolvers = []
let selectionMode = 'collapsed'

const depsEqual = (a, b) => a !== undefined && b !== undefined && a.length === b.length && a.every((value, index) => Object.is(value, b[index]))
function effectHook(effect, deps, queue) {
  const index = cursor++
  const previous = hooks[index]
  if (previous === undefined || !depsEqual(previous.deps, deps)) {
    queue.push(() => {
      previous?.cleanup?.()
      const cleanup = effect()
      hooks[index] = { deps: [...deps], cleanup: typeof cleanup === 'function' ? cleanup : undefined }
    })
  }
}
function callbackHook(callback, deps) {
  const index = cursor++
  const previous = hooks[index]
  if (previous !== undefined && depsEqual(previous.deps, deps)) return previous.value
  hooks[index] = { deps: [...deps], value: callback }
  return callback
}
function clearAttachedRefs() {
  for (const ref of attachedRefs.splice(0)) {
    if (typeof ref === 'function') ref(null)
    else ref.current = null
  }
}
function attachRefs(node) {
  if (node === null || node === undefined || typeof node !== 'object') return
  const ref = node.ref ?? node.props?.ref
  if (typeof ref === 'function') { ref(node); attachedRefs.push(ref) }
  else if (ref !== null && ref !== undefined) { ref.current = node; attachedRefs.push(ref) }
  for (const child of node.children ?? []) attachRefs(child)
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
function makeNode(kind, props, ...children) {
  const className = props?.className ?? ''
  let node
  if (typeof className === 'string' && className.split(/\s+/).includes('seekdeep-primitive-hover-card-card')) {
    node = stableCard ?? {}
    stableCard = node
  } else node = {}
  Object.assign(node, {
    kind, props: props ?? {}, children: children.flat(Infinity).filter(value => value !== null && value !== undefined && value !== false),
    ref: props?.ref,
    getBoundingClientRect() { return className.includes('seekdeep-primitive-hover-card-root') ? anchorRect : { top: 0, right: 0, left: 0, bottom: 0, width: 0, height: this.offsetHeight } },
    contains(target) { return target === this || this.children.some(child => typeof child === 'object' && child?.contains?.(target)) },
  })
  Object.defineProperty(node, 'offsetHeight', { configurable: true, get: () => cardHeight })
  return node
}
function activeTimers() { return timers.filter(timer => timer.active).length }
function selection() {
  if (selectionMode === 'none') return null
  if (selectionMode === 'collapsed') return { isCollapsed: true, rangeCount: 0, getRangeAt() { throw new Error('no range') } }
  const intersections = selectionMode === 'multi' ? [false, true] : [selectionMode === 'inside']
  return {
    isCollapsed: false,
    rangeCount: intersections.length,
    getRangeAt(index) { return { intersectsNode() { return intersections[index] } } },
  }
}

export function installHoverCardBench() {
  for (const hook of hooks) hook?.cleanup?.()
  hooks = []
  cursor = 0
  pendingLayout = []
  pendingEffects = []
  dirty = false
  attachedRefs = []
  timers = []
  now = 0
  nextTimer = 1
  listeners = new Map()
  styles = []
  anchorRect = { top: 40, right: 200, left: 100, bottom: 74, width: 100, height: 34 }
  cardHeight = 0
  stableCard = null
  clipboardMode = 'resolve'
  clipboardCalls = []
  clipboardResolvers = []
  selectionMode = 'collapsed'
  globalThis.window = globalThis
  globalThis.innerHeight = 768
  globalThis.setTimeout = (callback, delay) => {
    const timer = { id: nextTimer++, callback, at: now + Number(delay), active: true }
    timers.push(timer)
    return timer.id
  }
  globalThis.clearTimeout = id => {
    const timer = timers.find(candidate => candidate.id === id)
    if (timer !== undefined) timer.active = false
  }
  globalThis.addEventListener = (name, listener) => {
    let bucket = listeners.get(name)
    if (bucket === undefined) listeners.set(name, bucket = new Set())
    bucket.add(listener)
  }
  globalThis.removeEventListener = (name, listener) => listeners.get(name)?.delete(listener)
  globalThis.getSelection = () => selection()
  Object.defineProperty(globalThis, 'navigator', {
    configurable: true,
    value: { clipboard: { writeText(value) {
      clipboardCalls.push(value)
      if (clipboardMode === 'reject') return Promise.reject(new Error('denied'))
      if (clipboardMode === 'defer') return new Promise(resolve => clipboardResolvers.push(resolve))
      return Promise.resolve()
    } } },
  })
  const body = makeNode('body', {}, [])
  globalThis.document = {
    body,
    head: { appendChild(node) { styles.push(node) } },
    createElement(kind) { return { kind, attributes: {}, textContent: '', setAttribute(key, value) { this.attributes[key] = value } } },
    querySelector(selector) {
      const match = selector.match(/data-plugin-css="([^"]+)"/)
      return match === null ? null : styles.find(style => style.attributes['data-plugin-css'] === match[1]) ?? null
    },
  }
  const React = {
    createElement: makeNode,
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
    useCallback: callbackHook,
    useLayoutEffect(effect, deps) { effectHook(effect, deps, pendingLayout) },
    useEffect(effect, deps) { effectHook(effect, deps, pendingEffects) },
  }
  const ReactDOM = { createPortal(child, container) { return makeNode('Portal', { container }, [child]) } }
  return { React, ReactDOM, body, styles }
}

export function hoverObject(entries) { return Object.fromEntries(entries) }
export function hoverMakeProps(options = {}) {
  return { anchor: makeNode('span', {}, ['row']), content: makeNode('div', {}, ['card body']), ...options }
}
export function hoverRender(component, props) {
  let tree
  for (let attempt = 0; attempt < 12; attempt++) {
    clearAttachedRefs()
    cursor = 0
    pendingLayout = []
    pendingEffects = []
    dirty = false
    tree = component(props)
    attachRefs(tree)
    for (const run of pendingLayout) run()
    for (const run of pendingEffects) run()
    if (!dirty) {
      if (all(tree, node => node === stableCard).length === 0) stableCard = null
      return tree
    }
  }
  throw new Error('HoverCard hook runtime did not settle')
}
export function hoverUnmount() {
  clearAttachedRefs()
  for (const hook of hooks) hook?.cleanup?.()
  hooks = []
  stableCard = null
}
export function hoverAdvance(milliseconds) {
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
export function hoverTreeCard(tree) { return all(tree, node => String(node.props?.className ?? '').split(/\s+/).includes('seekdeep-primitive-hover-card-card'))[0] }
export function hoverTreeStatus(tree) { return all(tree, node => node.props?.role === 'status')[0] }
export function hoverTreePortal(tree) { return all(tree, node => node.kind === 'Portal')[0] }
export function hoverTreeText(tree) { return text(tree) }
export function hoverEnter(tree) { tree.props.onPointerEnter() }
export function hoverLeave(tree) { tree.props.onPointerLeave() }
export function hoverPressAnchor(tree) { tree.props.onPointerDownCapture({ target: tree.children[0] }) }
export function hoverPressCard(tree) { tree.props.onPointerDownCapture({ target: hoverTreeCard(tree) }) }
export function hoverClickCard(tree) { const card = hoverTreeCard(tree); return card.props.onClick({ currentTarget: card }) }
export function hoverKeyCard(tree, key) { const card = hoverTreeCard(tree); let prevented = false; card.props.onKeyDown({ key, preventDefault() { prevented = true } }); return prevented }
export function hoverSetAnchorRect(top, right) { anchorRect = { top, right, left: right - 100, bottom: top + 34, width: 100, height: 34 } }
export function hoverSetCardHeight(height) { cardHeight = height }
export function hoverSetViewport(height) { globalThis.innerHeight = height }
export function hoverDispatch(name) { for (const listener of listeners.get(name) ?? []) listener() }
export function hoverListenerCount(name) { return listeners.get(name)?.size ?? 0 }
export function hoverSetClipboardMode(mode) { clipboardMode = mode }
export function hoverResolveClipboard() { for (const resolve of clipboardResolvers.splice(0)) resolve() }
export function hoverClipboardCalls() { return clipboardCalls }
export function hoverSetSelection(mode) { selectionMode = mode }
export function hoverTimerCount() { return activeTimers() }
export function hoverStyles() { return styles }
export function hoverTick() { return Promise.resolve().then(() => Promise.resolve()).then(() => Promise.resolve()) }
"#)]
extern "C" {
    fn installHoverCardBench() -> JsValue;
    fn hoverObject(entries: &Array) -> JsValue;
    fn hoverMakeProps(options: &JsValue) -> JsValue;
    fn hoverRender(component: &JsValue, props: &JsValue) -> JsValue;
    fn hoverUnmount();
    fn hoverAdvance(milliseconds: f64);
    fn hoverTreeCard(tree: &JsValue) -> JsValue;
    fn hoverTreeStatus(tree: &JsValue) -> JsValue;
    fn hoverTreePortal(tree: &JsValue) -> JsValue;
    fn hoverTreeText(tree: &JsValue) -> String;
    fn hoverEnter(tree: &JsValue);
    fn hoverLeave(tree: &JsValue);
    fn hoverPressAnchor(tree: &JsValue);
    fn hoverPressCard(tree: &JsValue);
    fn hoverClickCard(tree: &JsValue) -> JsValue;
    fn hoverKeyCard(tree: &JsValue, key: &str) -> bool;
    fn hoverSetAnchorRect(top: f64, right: f64);
    fn hoverSetCardHeight(height: f64);
    fn hoverSetViewport(height: f64);
    fn hoverDispatch(name: &str);
    fn hoverListenerCount(name: &str) -> u32;
    fn hoverSetClipboardMode(mode: &str);
    fn hoverResolveClipboard();
    fn hoverClipboardCalls() -> Array;
    fn hoverSetSelection(mode: &str);
    fn hoverTimerCount() -> u32;
    fn hoverStyles() -> Array;
    fn hoverTick() -> Promise;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn options(entries: &[(&str, JsValue)]) -> JsValue {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    hoverObject(&array)
}

fn make_props(entries: &[(&str, JsValue)]) -> JsValue {
    hoverMakeProps(&options(entries))
}

fn setup(entries: &[(&str, JsValue)]) -> (JsValue, JsValue, JsValue) {
    let bench = installHoverCardBench();
    configure_client_ui_primitive_hover_card(
        property(&bench, "React"),
        property(&bench, "ReactDOM"),
    )
    .unwrap();
    let component = hover_card_component().unwrap();
    let props = make_props(entries);
    (bench, component, props)
}

fn render(component: &JsValue, props: &JsValue) -> JsValue {
    hoverRender(component, props)
}

fn card_style(tree: &JsValue, key: &str) -> JsValue {
    property(&property(&hoverTreeCard(tree), "props"), "style").pipe(|style| property(&style, key))
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}

impl<T> Pipe for T {}

async fn tick() {
    JsFuture::from(hoverTick()).await.unwrap();
}

fn open(component: &JsValue, props: &JsValue, delay: f64) -> JsValue {
    let tree = render(component, props);
    hoverEnter(&tree);
    hoverAdvance(delay);
    render(component, props)
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)]
fn dwell_grace_press_disabled_and_timer_lifecycles_match_source() {
    let (bench, component, props) = setup(&[]);
    assert_eq!(hoverStyles().length(), 1);
    assert_eq!(
        property(
            &property(&hoverStyles().get(0), "attributes"),
            "data-plugin"
        )
        .as_string()
        .as_deref(),
        Some("@seekdeep-ai/seekdeep-client-ui-primitives")
    );
    let first = render(&component, &props);
    hoverEnter(&first);
    hoverAdvance(499.0);
    assert!(hoverTreeCard(&render(&component, &props)).is_undefined());
    hoverAdvance(1.0);
    let shown = render(&component, &props);
    assert!(!hoverTreeCard(&shown).is_undefined());
    assert!(Object::is(
        &property(&property(&hoverTreePortal(&shown), "props"), "container"),
        &property(&bench, "body")
    ));
    assert_eq!(card_style(&shown, "left").as_f64(), Some(208.0));
    assert_eq!(card_style(&shown, "top").as_f64(), Some(40.0));

    hoverLeave(&shown);
    hoverAdvance(f64::from(POINTER_GRACE_MS - 1));
    assert!(!hoverTreeCard(&render(&component, &props)).is_undefined());
    hoverAdvance(1.0);
    assert!(hoverTreeCard(&render(&component, &props)).is_undefined());
    let closed = render(&component, &props);
    hoverEnter(&closed);
    hoverAdvance(500.0);
    let reopened = render(&component, &props);
    assert!(!hoverTreeCard(&reopened).is_undefined());
    hoverEnter(&reopened);
    hoverLeave(&reopened);
    hoverAdvance(f64::from(POINTER_GRACE_MS));
    hoverAdvance(500.0);
    assert!(hoverTreeCard(&render(&component, &props)).is_undefined());
    hoverUnmount();

    let (_bench, component, props) = setup(&[]);
    let first = render(&component, &props);
    hoverEnter(&first);
    hoverLeave(&first);
    hoverAdvance(1_000.0);
    assert!(hoverTreeCard(&render(&component, &props)).is_undefined());
    let open_tree = open(&component, &props, 500.0);
    hoverLeave(&open_tree);
    hoverAdvance(f64::from(POINTER_GRACE_MS - 50));
    hoverEnter(&open_tree);
    hoverAdvance(f64::from(POINTER_GRACE_MS * 10));
    assert!(!hoverTreeCard(&render(&component, &props)).is_undefined());
    let open_tree = render(&component, &props);
    hoverPressCard(&open_tree);
    hoverAdvance(f64::from(POINTER_GRACE_MS));
    assert!(!hoverTreeCard(&render(&component, &props)).is_undefined());
    let open_tree = render(&component, &props);
    hoverPressAnchor(&open_tree);
    assert!(hoverTreeCard(&render(&component, &props)).is_undefined());
    hoverAdvance(1_000.0);
    assert!(hoverTreeCard(&render(&component, &props)).is_undefined());
    hoverPressAnchor(&render(&component, &props));
    assert!(hoverTreeCard(&render(&component, &props)).is_undefined());
    hoverUnmount();

    let (_bench, component, custom) = setup(&[("openDelayMs", JsValue::from_f64(50.0))]);
    assert!(!hoverTreeCard(&open(&component, &custom, 50.0)).is_undefined());
    hoverUnmount();

    let (_bench, component, disabled) = setup(&[("disabled", JsValue::TRUE)]);
    let tree = render(&component, &disabled);
    hoverEnter(&tree);
    hoverAdvance(1_000.0);
    assert!(hoverTreeCard(&render(&component, &disabled)).is_undefined());
    hoverUnmount();

    let (_bench, component, enabled) = setup(&[]);
    assert!(!hoverTreeCard(&open(&component, &enabled, 500.0)).is_undefined());
    let disabled = make_props(&[("disabled", JsValue::TRUE)]);
    assert!(hoverTreeCard(&render(&component, &disabled)).is_undefined());
    hoverUnmount();

    let (_bench, component, enabled_props) = setup(&[]);
    let tree = render(&component, &enabled_props);
    hoverEnter(&tree);
    assert_eq!(hoverTimerCount(), 1);
    hoverUnmount();
    assert_eq!(hoverTimerCount(), 0);
    hoverAdvance(1_000.0);
}

#[wasm_bindgen_test]
fn placement_correction_resize_scroll_and_listener_cleanup_match_source() {
    let (_bench, component, props) = setup(&[]);
    hoverSetViewport(300.0);
    hoverSetCardHeight(120.0);
    hoverSetAnchorRect(280.0, 200.0);
    let corrected = open(&component, &props, 500.0);
    assert_eq!(card_style(&corrected, "top").as_f64(), Some(172.0));
    hoverUnmount();

    let (_bench, component, props) = setup(&[]);
    hoverSetViewport(300.0);
    hoverSetAnchorRect(280.0, 200.0);
    let first = open(&component, &props, 500.0);
    assert_eq!(card_style(&first, "top").as_f64(), Some(280.0));
    hoverSetCardHeight(120.0);
    hoverDispatch("resize");
    let resized = render(&component, &props);
    assert_eq!(card_style(&resized, "top").as_f64(), Some(172.0));
    assert_eq!(hoverListenerCount("resize"), 1);
    assert_eq!(hoverListenerCount("scroll"), 1);
    hoverSetAnchorRect(90.0, 300.0);
    hoverDispatch("scroll");
    let scrolled = render(&component, &props);
    assert_eq!(card_style(&scrolled, "left").as_f64(), Some(308.0));
    assert_eq!(card_style(&scrolled, "top").as_f64(), Some(90.0));
    hoverLeave(&scrolled);
    hoverAdvance(f64::from(POINTER_GRACE_MS));
    render(&component, &props);
    assert_eq!(hoverListenerCount("resize"), 0);
    assert_eq!(hoverListenerCount("scroll"), 0);
    hoverUnmount();
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn selection_copy_feedback_keyboard_and_rejection_match_source() {
    let (_bench, component, props) = setup(&[
        ("copyText", JsValue::from_str("card body")),
        ("copyLabel", JsValue::from_str("Copy")),
        ("copiedLabel", JsValue::from_str("Copied")),
    ]);
    let tree = open(&component, &props, 500.0);
    let card = hoverTreeCard(&tree);
    assert_eq!(
        property(&property(&card, "props"), "role")
            .as_string()
            .as_deref(),
        Some("button")
    );
    assert_eq!(
        property(&property(&card, "props"), "aria-label")
            .as_string()
            .as_deref(),
        Some("Copy: card body")
    );
    assert_eq!(hoverTreeText(&hoverTreeStatus(&tree)), "");
    assert!(
        !property(&property(&card, "props"), "className")
            .as_string()
            .unwrap()
            .is_empty()
    );

    hoverSetSelection("inside");
    hoverClickCard(&tree);
    tick().await;
    assert_eq!(hoverClipboardCalls().length(), 0);
    hoverSetSelection("multi");
    hoverClickCard(&tree);
    tick().await;
    assert_eq!(hoverClipboardCalls().length(), 0);
    hoverSetSelection("outside");
    hoverSetCardHeight(96.0);
    hoverClickCard(&tree);
    tick().await;
    let copied = render(&component, &props);
    assert_eq!(hoverClipboardCalls().length(), 1);
    assert_eq!(hoverTreeText(&hoverTreeStatus(&copied)), "Copied");
    assert!(Object::is(&card, &hoverTreeCard(&copied)));
    assert_eq!(card_style(&copied, "minHeight").as_f64(), Some(96.0));
    hoverClickCard(&copied);
    tick().await;
    assert_eq!(hoverClipboardCalls().length(), 1);
    hoverAdvance(999.0);
    assert_eq!(
        hoverTreeText(&hoverTreeStatus(&render(&component, &props))),
        "Copied"
    );
    hoverAdvance(1.0);
    let restored = render(&component, &props);
    assert!(card_style(&restored, "minHeight").is_undefined());
    assert!(hoverTreeText(&restored).contains("card body"));

    assert!(!hoverKeyCard(&restored, "Escape"));
    assert_eq!(hoverClipboardCalls().length(), 1);
    assert!(hoverKeyCard(&restored, "Enter"));
    tick().await;
    assert_eq!(hoverClipboardCalls().length(), 2);
    hoverAdvance(1_000.0);
    let restored = render(&component, &props);
    assert!(hoverKeyCard(&restored, " "));
    tick().await;
    assert_eq!(hoverClipboardCalls().length(), 3);
    hoverUnmount();

    let (_bench, component, props) = setup(&[("copyText", JsValue::from_str("value"))]);
    hoverSetClipboardMode("reject");
    let tree = open(&component, &props, 500.0);
    hoverClickCard(&tree);
    tick().await;
    let rejected = render(&component, &props);
    assert!(hoverTreeText(&rejected).contains("card body"));
    assert_eq!(hoverTimerCount(), 0);
    hoverUnmount();
}

#[wasm_bindgen_test(async)]
#[allow(clippy::too_many_lines)]
async fn copy_cleanup_close_epoch_and_inflight_coalescing_match_source() {
    let (_bench, component, props) = setup(&[
        ("copyText", JsValue::from_str("value")),
        ("copiedLabel", JsValue::from_str("Copied")),
    ]);
    let tree = open(&component, &props, 500.0);
    hoverClickCard(&tree);
    tick().await;
    render(&component, &props);
    assert_eq!(hoverTimerCount(), 1);
    hoverUnmount();
    assert_eq!(hoverTimerCount(), 0);

    let (_bench, component, props) = setup(&[
        ("copyText", JsValue::from_str("value")),
        ("copiedLabel", JsValue::from_str("Copied")),
    ]);
    let tree = open(&component, &props, 500.0);
    hoverClickCard(&tree);
    tick().await;
    let copied = render(&component, &props);
    assert_eq!(hoverTreeText(&hoverTreeStatus(&copied)), "Copied");
    hoverLeave(&copied);
    hoverAdvance(f64::from(POINTER_GRACE_MS));
    assert!(hoverTreeCard(&render(&component, &props)).is_undefined());
    assert_eq!(hoverTimerCount(), 0);
    let closed = render(&component, &props);
    hoverEnter(&closed);
    hoverAdvance(500.0);
    assert!(hoverTreeText(&render(&component, &props)).contains("card body"));
    hoverUnmount();

    let (_bench, component, props) = setup(&[("copyText", JsValue::from_str("value"))]);
    hoverSetClipboardMode("defer");
    let tree = open(&component, &props, 500.0);
    hoverClickCard(&tree);
    hoverClickCard(&tree);
    assert_eq!(hoverClipboardCalls().length(), 1);
    hoverResolveClipboard();
    tick().await;
    assert_eq!(
        hoverTreeText(&hoverTreeStatus(&render(&component, &props))),
        "复制成功"
    );
    hoverUnmount();

    let (_bench, component, props) = setup(&[("copyText", JsValue::from_str("value"))]);
    hoverSetClipboardMode("defer");
    let tree = open(&component, &props, 500.0);
    hoverClickCard(&tree);
    hoverUnmount();
    hoverResolveClipboard();
    tick().await;
    assert_eq!(hoverTimerCount(), 0);

    let (_bench, component, props) = setup(&[("copyText", JsValue::from_str("value"))]);
    hoverSetClipboardMode("defer");
    let tree = open(&component, &props, 500.0);
    hoverClickCard(&tree);
    hoverLeave(&tree);
    hoverAdvance(f64::from(POINTER_GRACE_MS));
    let closed = render(&component, &props);
    hoverEnter(&closed);
    hoverAdvance(500.0);
    hoverResolveClipboard();
    tick().await;
    let reopened = render(&component, &props);
    assert_eq!(hoverTimerCount(), 0);
    assert!(hoverTreeText(&reopened).contains("card body"));
    hoverUnmount();
}
