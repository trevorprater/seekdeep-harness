//! Live JavaScript coverage for compiled Tooltip timing, refs, and placement.

#![cfg(target_arch = "wasm32")]

use std::{cell::Cell, rc::Rc};

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_primitives::{configure_client_ui_primitive_tooltip, tooltip_component};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let pendingLayout = []
let pendingEffects = []
let timers = []
let now = 0
let nextTimer = 1
let listeners = new Map()
let bubbleRect = { left: 0, right: 0, top: 0, bottom: 0, width: 0, height: 0 }
const styles = []

const depsEqual = (a, b) => a !== undefined && b !== undefined && a.length === b.length && a.every((v, i) => Object.is(v, b[i]))
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
function attachRefs(node) {
  if (node === null || node === undefined || typeof node !== 'object') return
  const ref = node.ref ?? node.props?.ref
  if (typeof ref === 'function') ref(node)
  else if (ref !== null && ref !== undefined) ref.current = node
  for (const child of node.children ?? []) attachRefs(child)
}

export function installTooltipBench() {
  hooks = []
  cursor = 0
  pendingLayout = []
  pendingEffects = []
  timers = []
  now = 0
  nextTimer = 1
  listeners = new Map()
  bubbleRect = { left: 0, right: 0, top: 0, bottom: 0, width: 0, height: 0 }
  styles.splice(0)
  globalThis.window = globalThis
  globalThis.innerWidth = 1024
  globalThis.innerHeight = 768
  globalThis.setTimeout = (callback, delay) => {
    const timer = { id: nextTimer++, callback, at: now + delay, active: true }
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
  globalThis.document = {
    head: { appendChild(node) { styles.push(node) } },
    createElement(kind) { return { kind, attributes: {}, setAttribute(k, v) { this.attributes[k] = v } } },
    querySelector(selector) {
      const match = selector.match(/data-plugin-css="([^"]+)"/)
      return match === null ? null : styles.find(style => style.attributes['data-plugin-css'] === match[1]) ?? null
    },
  }
  const React = {
    Fragment: 'Fragment',
    createElement(kind, props, ...children) {
      const node = {
        kind, props: props ?? {}, children, ref: props?.ref,
        style: { ...(props?.style ?? {}) }, rect: undefined,
        getBoundingClientRect() { return this.props?.role === 'tooltip' ? bubbleRect : this.rect ?? { left: 0, right: 0, top: 0, bottom: 0, width: 0, height: 0 } },
      }
      return node
    },
    cloneElement(child, injected) {
      return { ...child, props: { ...child.props, ...injected }, ref: injected.ref ?? child.ref }
    },
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { value: initial }
      return [hooks[index].value, update => {
        hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update
      }]
    },
    useRef(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { current: initial }
      return hooks[index]
    },
    useCallback(callback) { cursor += 1; return callback },
    useLayoutEffect(effect, deps) { effectHook(effect, deps, pendingLayout) },
    useEffect(effect, deps) { effectHook(effect, deps, pendingEffects) },
  }
  return { React, styles }
}

export function tooltipAnchor(props = {}, ref) {
  return {
    kind: 'button', props: { type: 'button', ...props }, children: ['anchor'], ref,
    style: {}, rect: { left: 0, right: 0, top: 0, bottom: 0, width: 0, height: 0 },
    getBoundingClientRect() { return this.rect },
  }
}
export function tooltipRender(component, props) {
  cursor = 0
  pendingLayout = []
  pendingEffects = []
  const node = component(props)
  attachRefs(node)
  for (const run of pendingLayout) run()
  for (const run of pendingEffects) run()
  return node
}
export function tooltipFresh() {
  for (const hook of hooks) hook?.cleanup?.()
  hooks = []
  cursor = 0
}
export function tooltipUnmount() { for (const hook of hooks) hook?.cleanup?.(); hooks = [] }
export function tooltipAdvance(ms) {
  now += ms
  for (const timer of timers) if (timer.active && timer.at <= now) { timer.active = false; timer.callback() }
}
export function tooltipSetBubbleRect(left, right, top, bottom, height) {
  bubbleRect = { left, right, top, bottom, width: right - left, height }
}
export function tooltipSetViewport(width, height) { globalThis.innerWidth = width; globalThis.innerHeight = height }
export function tooltipDispatchResize() { for (const listener of listeners.get('resize') ?? []) listener() }
export function tooltipListenerCount() { return listeners.get('resize')?.size ?? 0 }
export function tooltipObject(entries) { return Object.fromEntries(entries) }
export function tooltipStyles() { return styles }
"#)]
extern "C" {
    fn installTooltipBench() -> JsValue;
    fn tooltipAnchor(props: &JsValue, reference: &JsValue) -> JsValue;
    fn tooltipRender(component: &JsValue, props: &JsValue) -> JsValue;
    fn tooltipFresh();
    fn tooltipUnmount();
    fn tooltipAdvance(milliseconds: f64);
    fn tooltipSetBubbleRect(left: f64, right: f64, top: f64, bottom: f64, height: f64);
    fn tooltipSetViewport(width: f64, height: f64);
    fn tooltipDispatchResize();
    fn tooltipListenerCount() -> u32;
    fn tooltipObject(entries: &Array) -> JsValue;
    fn tooltipStyles() -> Array;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn props(entries: &[(&str, JsValue)]) -> JsValue {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    tooltipObject(&array)
}

fn children(node: &JsValue) -> Array {
    Array::from(&property(node, "children"))
}

fn anchor(tree: &JsValue) -> JsValue {
    children(tree).get(0)
}

fn bubble(tree: &JsValue) -> JsValue {
    children(tree).get(1)
}

fn invoke(node: &JsValue, name: &str) {
    property(&property(node, "props"), name)
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &Object::new())
        .unwrap();
}

fn setup() -> (JsValue, JsValue) {
    let bench = installTooltipBench();
    configure_client_ui_primitive_tooltip(property(&bench, "React")).unwrap();
    (bench, tooltip_component().unwrap())
}

#[wasm_bindgen_test]
fn lazy_delay_focus_cancel_handler_chaining_and_ref_forwarding_are_exact() {
    let (_bench, component) = setup();
    assert_eq!(tooltipStyles().length(), 1);
    let labels = Rc::new(Cell::new(0_u32));
    let label_count = labels.clone();
    let label = Closure::wrap(Box::new(move || {
        label_count.set(label_count.get() + 1);
        "Timing details".to_owned()
    }) as Box<dyn FnMut() -> String>)
    .into_js_value();
    let own_calls = Rc::new(Cell::new(0_u32));
    let own_count = own_calls.clone();
    let own_enter =
        Closure::wrap(Box::new(move || own_count.set(own_count.get() + 1)) as Box<dyn FnMut()>)
            .into_js_value();
    let reference = Object::new();
    Reflect::set(&reference, &"current".into(), &JsValue::NULL).unwrap();
    let child = tooltipAnchor(&props(&[("onMouseEnter", own_enter)]), &reference);
    let tooltip_props = props(&[
        ("label", label),
        ("delayMs", JsValue::from_f64(500.0)),
        ("children", child),
    ]);
    let first = tooltipRender(&component, &tooltip_props);
    assert_eq!(labels.get(), 0);
    let attached = property(&reference, "current");
    assert!(!attached.is_null());
    invoke(&anchor(&first), "onMouseEnter");
    assert_eq!(own_calls.get(), 1);
    tooltipAdvance(499.0);
    assert_eq!(labels.get(), 0);
    tooltipAdvance(1.0);
    let shown = tooltipRender(&component, &tooltip_props);
    assert_eq!(labels.get(), 1);
    assert_eq!(children(&shown).length(), 2);
    assert_eq!(
        children(&bubble(&shown)).get(0).as_string().as_deref(),
        Some("Timing details")
    );
    invoke(&anchor(&shown), "onMouseLeave");
    let hidden = tooltipRender(&component, &tooltip_props);
    assert_eq!(children(&hidden).length(), 1);

    invoke(&anchor(&hidden), "onMouseEnter");
    tooltipAdvance(100.0);
    invoke(&anchor(&hidden), "onMouseLeave");
    tooltipAdvance(500.0);
    assert_eq!(
        children(&tooltipRender(&component, &tooltip_props)).length(),
        1
    );
    let focused = tooltipRender(&component, &tooltip_props);
    invoke(&anchor(&focused), "onFocus");
    assert_eq!(
        children(&tooltipRender(&component, &tooltip_props)).length(),
        2
    );
    tooltipUnmount();
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)]
fn placement_clamps_flips_reclamps_and_honors_max_width() {
    let (_bench, component) = setup();
    let child = tooltipAnchor(&Object::new(), &JsValue::UNDEFINED);
    Reflect::set(
        &child,
        &"rect".into(),
        &props(&[
            ("left", JsValue::from_f64(900.0)),
            ("right", JsValue::from_f64(1100.0)),
            ("top", JsValue::from_f64(600.0)),
            ("bottom", JsValue::from_f64(700.0)),
            ("width", JsValue::from_f64(200.0)),
            ("height", JsValue::from_f64(100.0)),
        ]),
    )
    .unwrap();
    tooltipSetBubbleRect(900.0, 1100.0, 0.0, 300.0, 300.0);
    let tooltip_props = props(&[
        ("label", JsValue::from_str("Tall")),
        ("side", JsValue::from_str("bottom")),
        ("maxWidth", JsValue::from_f64(360.0)),
        ("children", child),
    ]);
    let first = tooltipRender(&component, &tooltip_props);
    invoke(&anchor(&first), "onMouseEnter");
    let fitted = tooltipRender(&component, &tooltip_props);
    assert_eq!(tooltipListenerCount(), 1);
    assert_eq!(
        property(&property(&bubble(&fitted), "style"), "left")
            .as_string()
            .as_deref(),
        Some("912px")
    );
    assert_eq!(
        property(
            &property(&property(&bubble(&fitted), "props"), "style"),
            "maxWidth"
        )
        .as_f64(),
        Some(360.0)
    );
    let flipped = tooltipRender(&component, &tooltip_props);
    assert_eq!(
        property(&property(&bubble(&flipped), "props"), "data-side")
            .as_string()
            .as_deref(),
        Some("top")
    );
    assert_eq!(
        property(
            &property(&property(&bubble(&flipped), "props"), "style"),
            "top"
        )
        .as_f64(),
        Some(592.0)
    );

    tooltipSetViewport(900.0, 768.0);
    tooltipSetBubbleRect(850.0, 950.0, 0.0, 300.0, 300.0);
    tooltipDispatchResize();
    assert_eq!(
        property(&property(&bubble(&flipped), "style"), "left")
            .as_string()
            .as_deref(),
        Some("938px")
    );
    tooltipUnmount();

    tooltipFresh();
    let child = tooltipAnchor(&Object::new(), &JsValue::UNDEFINED);
    Reflect::set(
        &child,
        &"rect".into(),
        &props(&[
            ("left", JsValue::from_f64(100.0)),
            ("right", JsValue::from_f64(200.0)),
            ("top", JsValue::from_f64(10.0)),
            ("bottom", JsValue::from_f64(40.0)),
            ("width", JsValue::from_f64(100.0)),
            ("height", JsValue::from_f64(30.0)),
        ]),
    )
    .unwrap();
    tooltipSetViewport(1_024.0, 768.0);
    tooltipSetBubbleRect(100.0, 200.0, 0.0, 100.0, 100.0);
    let props = props(&[
        ("label", JsValue::from_str("Above")),
        ("side", JsValue::from_str("top")),
        ("children", child),
    ]);
    let first = tooltipRender(&component, &props);
    invoke(&anchor(&first), "onMouseEnter");
    tooltipRender(&component, &props);
    let flipped = tooltipRender(&component, &props);
    assert_eq!(
        property(&property(&bubble(&flipped), "props"), "data-side")
            .as_string()
            .as_deref(),
        Some("bottom")
    );
    assert_eq!(
        property(
            &property(&property(&bubble(&flipped), "props"), "style"),
            "top"
        )
        .as_f64(),
        Some(48.0)
    );
    tooltipUnmount();
}

#[wasm_bindgen_test]
fn independent_triggers_disabled_teardown_and_callback_ref_match_source() {
    let (_bench, component) = setup();
    let callback_count = Rc::new(Cell::new(0_u32));
    let seen = callback_count.clone();
    let callback_ref = Closure::wrap(Box::new(move |_element: JsValue| {
        seen.set(seen.get() + 1);
    }) as Box<dyn FnMut(JsValue)>)
    .into_js_value();
    let child = tooltipAnchor(&Object::new(), &callback_ref);
    let enabled = props(&[
        ("label", JsValue::from_str("Sticky")),
        ("children", child.clone()),
    ]);
    let first = tooltipRender(&component, &enabled);
    assert_eq!(callback_count.get(), 1);
    invoke(&anchor(&first), "onFocus");
    invoke(&anchor(&first), "onMouseEnter");
    assert_eq!(children(&tooltipRender(&component, &enabled)).length(), 2);
    let shown = tooltipRender(&component, &enabled);
    invoke(&anchor(&shown), "onMouseLeave");
    assert_eq!(children(&tooltipRender(&component, &enabled)).length(), 1);
    let hidden = tooltipRender(&component, &enabled);
    invoke(&anchor(&hidden), "onMouseEnter");
    let shown = tooltipRender(&component, &enabled);
    invoke(&anchor(&shown), "onBlur");
    assert_eq!(children(&tooltipRender(&component, &enabled)).length(), 2);

    let disabled = props(&[
        ("label", JsValue::from_str("Sticky")),
        ("disabled", JsValue::TRUE),
        ("children", child),
    ]);
    tooltipRender(&component, &disabled);
    assert_eq!(children(&tooltipRender(&component, &disabled)).length(), 1);
    let disabled_tree = tooltipRender(&component, &disabled);
    invoke(&anchor(&disabled_tree), "onMouseEnter");
    assert_eq!(children(&tooltipRender(&component, &disabled)).length(), 1);
    tooltipUnmount();
}
