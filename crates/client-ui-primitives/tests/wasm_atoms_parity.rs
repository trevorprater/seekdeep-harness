//! Live JavaScript-boundary coverage for compiled primitive atoms and portals.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Object, Reflect};
use seekdeep_client_ui_primitives::{
    button_component, configure_client_ui_primitive_atoms, connection_banner_component,
    input_component, onboarding_surface_component, pill_component, state_dot_component,
    toast_component,
};
use wasm_bindgen::{JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hookSlots = []
let hookCursor = 0
let cleanups = []
let timers = []
let nextTimer = 1
let listeners = new Map()
const styles = []
const root = { inert: false }
const body = { kind: 'body' }
let rootAvailable = true

export function installAtomBench() {
  hookSlots = []
  hookCursor = 0
  cleanups = []
  timers = []
  nextTimer = 1
  listeners = new Map()
  styles.splice(0)
  root.inert = false
  rootAvailable = true
  globalThis.window = globalThis
  globalThis.setTimeout = (callback, delay) => {
    const timer = { id: nextTimer++, callback, delay, active: true }
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
    body,
    head: { appendChild(node) { styles.push(node) } },
    createElement(kind) { return { kind, attributes: {}, setAttribute(k, v) { this.attributes[k] = v } } },
    querySelector(selector) {
      const match = selector.match(/data-plugin-css="([^"]+)"/)
      return match === null ? null : styles.find(style => style.attributes['data-plugin-css'] === match[1]) ?? null
    },
    getElementById(id) { return id === 'root' && rootAvailable ? root : null },
  }
  const React = {
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    useState(initial) {
      const index = hookCursor++
      if (!(index in hookSlots)) hookSlots[index] = initial
      return [hookSlots[index], update => {
        hookSlots[index] = typeof update === 'function' ? update(hookSlots[index]) : update
      }]
    },
    useEffect(effect) {
      const index = hookCursor++
      if (!(index in hookSlots)) {
        hookSlots[index] = true
        const cleanup = effect()
        if (typeof cleanup === 'function') cleanups.push(cleanup)
      }
    },
    useLayoutEffect(effect) {
      const index = hookCursor++
      if (!(index in hookSlots)) {
        hookSlots[index] = true
        const cleanup = effect()
        if (typeof cleanup === 'function') cleanups.push(cleanup)
      }
    },
  }
  const ReactDOM = { createPortal(node, container) { return { portal: node, container } } }
  return { React, ReactDOM, root, body, styles }
}

export function atomRender(component, props) {
  hookCursor = 0
  return component(props)
}
export function atomFresh() {
  for (const cleanup of cleanups.splice(0).reverse()) cleanup()
  hookSlots = []
  hookCursor = 0
}
export function atomUnmount() {
  for (const cleanup of cleanups.splice(0).reverse()) cleanup()
}
export function atomStyles() { return styles }
export function atomTimers() { return timers }
export function atomFireTimer(index) {
  const timer = timers[index]
  if (timer?.active) { timer.active = false; timer.callback() }
}
export function atomDispatch(name) { for (const listener of listeners.get(name) ?? []) listener() }
export function atomListenerCount(name) { return listeners.get(name)?.size ?? 0 }
export function atomRoot() { return root }
export function atomBody() { return body }
export function atomSetRootAvailable(value) { rootAvailable = value }
export function atomProp(value, key) { return value?.[key] }
export function atomObject(entries) { return Object.fromEntries(entries) }
"#)]
extern "C" {
    fn installAtomBench() -> JsValue;
    fn atomRender(component: &JsValue, props: &JsValue) -> JsValue;
    fn atomFresh();
    fn atomUnmount();
    fn atomStyles() -> Array;
    fn atomTimers() -> Array;
    fn atomFireTimer(index: u32);
    fn atomDispatch(name: &str);
    fn atomListenerCount(name: &str) -> u32;
    fn atomRoot() -> JsValue;
    fn atomBody() -> JsValue;
    fn atomSetRootAvailable(value: bool);
    fn atomObject(entries: &Array) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn props(entries: &[(&str, JsValue)]) -> JsValue {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    atomObject(&array)
}

fn kind(node: &JsValue) -> String {
    property(node, "kind").as_string().unwrap()
}

fn node_props(node: &JsValue) -> JsValue {
    property(node, "props")
}

fn children(node: &JsValue) -> Array {
    Array::from(&property(node, "children"))
}

#[wasm_bindgen_test]
fn button_pill_and_input_preserve_native_props_and_component_chrome() {
    let bench = installAtomBench();
    configure_client_ui_primitive_atoms(property(&bench, "React"), property(&bench, "ReactDOM"))
        .unwrap();
    assert_eq!(atomStyles().length(), 7);
    configure_client_ui_primitive_atoms(property(&bench, "React"), property(&bench, "ReactDOM"))
        .unwrap();
    assert_eq!(atomStyles().length(), 7);

    let clicked = Closure::wrap(Box::new(|| {}) as Box<dyn FnMut()>).into_js_value();
    let icon = JsValue::from_str("icon");
    let button = atomRender(
        &button_component().unwrap(),
        &props(&[
            ("variant", JsValue::from_str("primary")),
            ("size", JsValue::from_str("sm")),
            ("icon", icon.clone()),
            ("className", JsValue::from_str("caller")),
            ("children", JsValue::from_str("Create")),
            ("disabled", JsValue::TRUE),
            ("type", JsValue::from_str("submit")),
            ("onClick", clicked.clone()),
        ]),
    );
    assert_eq!(kind(&button), "button");
    let native = node_props(&button);
    assert_eq!(
        property(&native, "type").as_string().as_deref(),
        Some("submit")
    );
    assert_eq!(property(&native, "disabled").as_bool(), Some(true));
    let class = property(&native, "className").as_string().unwrap();
    for expected in ["button-button", "button-primary", "button-sm", "caller"] {
        assert!(class.contains(expected), "{expected}");
    }
    assert_eq!(children(&button).length(), 2);
    assert_eq!(kind(&children(&button).get(0)), "span");

    let default_button = atomRender(
        &button_component().unwrap(),
        &props(&[("children", JsValue::from_str("Default"))]),
    );
    assert_eq!(
        property(&node_props(&default_button), "type")
            .as_string()
            .as_deref(),
        Some("button")
    );

    let static_pill = atomRender(
        &pill_component().unwrap(),
        &props(&[
            ("active", JsValue::TRUE),
            ("children", JsValue::from_str("Static")),
            ("title", JsValue::from_str("ignored")),
        ]),
    );
    assert_eq!(kind(&static_pill), "span");
    assert!(property(&node_props(&static_pill), "title").is_undefined());
    assert!(
        property(&node_props(&static_pill), "className")
            .as_string()
            .unwrap()
            .contains("pill-active")
    );

    let interactive = atomRender(
        &pill_component().unwrap(),
        &props(&[
            ("children", JsValue::from_str("Open")),
            ("onClick", clicked),
            ("disabled", JsValue::TRUE),
        ]),
    );
    assert_eq!(kind(&interactive), "button");
    assert_eq!(
        property(&node_props(&interactive), "disabled").as_bool(),
        Some(true)
    );

    let changed = Closure::wrap(Box::new(|| {}) as Box<dyn FnMut()>).into_js_value();
    let input = atomRender(
        &input_component().unwrap(),
        &props(&[
            ("icon", icon),
            ("className", JsValue::from_str("search")),
            ("value", JsValue::from_str("needle")),
            ("onChange", changed),
        ]),
    );
    assert_eq!(kind(&input), "span");
    assert_eq!(children(&input).length(), 2);
    let native_input = children(&input).get(1);
    assert_eq!(kind(&native_input), "input");
    assert_eq!(
        property(&node_props(&native_input), "value")
            .as_string()
            .as_deref(),
        Some("needle")
    );
}

#[wasm_bindgen_test]
fn state_dot_and_connection_banner_preserve_exact_visual_state_contracts() {
    let bench = installAtomBench();
    configure_client_ui_primitive_atoms(property(&bench, "React"), property(&bench, "ReactDOM"))
        .unwrap();
    let ongoing = atomRender(
        &state_dot_component().unwrap(),
        &props(&[
            ("state", JsValue::from_str("ongoing")),
            ("size", JsValue::from_f64(12.0)),
            ("className", JsValue::from_str("state")),
        ]),
    );
    assert_eq!(kind(&ongoing), "svg");
    assert_eq!(children(&ongoing).length(), 8);
    assert_eq!(
        property(&node_props(&ongoing), "width").as_f64(),
        Some(12.0)
    );
    assert_eq!(
        property(
            &property(&node_props(&children(&ongoing).get(0)), "style"),
            "animationDelay"
        )
        .as_string()
        .as_deref(),
        Some("-1000ms")
    );
    assert_eq!(
        property(
            &property(&node_props(&children(&ongoing).get(7)), "style"),
            "animationDelay"
        )
        .as_string()
        .as_deref(),
        Some("-125ms")
    );
    let done = atomRender(
        &state_dot_component().unwrap(),
        &props(&[("state", JsValue::from_str("done"))]),
    );
    assert_eq!(kind(&done), "span");
    assert_eq!(
        property(&node_props(&done), "data-state")
            .as_string()
            .as_deref(),
        Some("done")
    );
    assert_eq!(
        property(&property(&node_props(&done), "style"), "width").as_f64(),
        Some(10.0)
    );

    assert!(
        atomRender(
            &connection_banner_component().unwrap(),
            &props(&[("reconnecting", JsValue::FALSE)])
        )
        .is_null()
    );
    let banner = atomRender(
        &connection_banner_component().unwrap(),
        &props(&[("reconnecting", JsValue::TRUE)]),
    );
    assert_eq!(kind(&banner), "div");
    assert_eq!(
        children(&banner).get(0).as_string().as_deref(),
        Some("连接已断开，正在重连…")
    );
}

#[wasm_bindgen_test]
fn onboarding_portal_owns_root_inertness_for_its_exact_lifetime() {
    let bench = installAtomBench();
    configure_client_ui_primitive_atoms(property(&bench, "React"), property(&bench, "ReactDOM"))
        .unwrap();
    let portal = atomRender(
        &onboarding_surface_component().unwrap(),
        &props(&[("children", JsValue::from_str("Welcome"))]),
    );
    assert!(property(&atomRoot(), "inert").as_bool().unwrap());
    assert!(Object::is(&property(&portal, "container"), &atomBody()));
    let overlay = property(&portal, "portal");
    assert_eq!(kind(&overlay), "div");
    assert_eq!(children(&overlay).length(), 2);
    assert_eq!(
        children(&children(&overlay).get(1))
            .get(0)
            .as_string()
            .as_deref(),
        Some("Welcome")
    );
    atomUnmount();
    assert!(!property(&atomRoot(), "inert").as_bool().unwrap());

    atomFresh();
    atomSetRootAvailable(false);
    let portal = atomRender(
        &onboarding_surface_component().unwrap(),
        &props(&[("children", JsValue::from_str("Detached"))]),
    );
    assert_eq!(
        children(&children(&property(&portal, "portal")).get(1))
            .get(0)
            .as_string()
            .as_deref(),
        Some("Detached")
    );
    atomUnmount();
}

#[wasm_bindgen_test]
fn toast_owns_timer_portal_anchor_measurement_resize_and_cleanup() {
    let bench = installAtomBench();
    configure_client_ui_primitive_atoms(property(&bench, "React"), property(&bench, "ReactDOM"))
        .unwrap();
    let done = std::rc::Rc::new(std::cell::Cell::new(0_u32));
    let count = done.clone();
    let on_done = Closure::wrap(Box::new(move || count.set(count.get() + 1)) as Box<dyn FnMut()>)
        .into_js_value();
    let anchor = props(&[]);
    let rect = props(&[
        ("left", JsValue::from_f64(100.0)),
        ("width", JsValue::from_f64(200.0)),
    ]);
    let measure_rect = rect.clone();
    let measure =
        Closure::wrap(Box::new(move || measure_rect.clone()) as Box<dyn FnMut() -> JsValue>);
    Reflect::set(
        &anchor,
        &"getBoundingClientRect".into(),
        &measure.into_js_value(),
    )
    .unwrap();
    let toast_props = props(&[
        ("text", JsValue::from_str("Saved")),
        ("icon", JsValue::from_str("!")),
        ("anchor", anchor),
        ("onDone", on_done),
    ]);
    let first = atomRender(&toast_component().unwrap(), &toast_props);
    assert!(Object::is(&property(&first, "container"), &atomBody()));
    assert_eq!(atomTimers().length(), 1);
    assert_eq!(
        property(&atomTimers().get(0), "delay").as_f64(),
        Some(4_000.0)
    );
    assert_eq!(atomListenerCount("resize"), 1);
    let rerendered = atomRender(&toast_component().unwrap(), &toast_props);
    let toast = property(&rerendered, "portal");
    assert_eq!(
        property(&property(&node_props(&toast), "style"), "left").as_f64(),
        Some(200.0)
    );
    Reflect::set(&rect, &"left".into(), &JsValue::from_f64(200.0)).unwrap();
    atomDispatch("resize");
    let moved = atomRender(&toast_component().unwrap(), &toast_props);
    assert_eq!(
        property(
            &property(&node_props(&property(&moved, "portal")), "style"),
            "left",
        )
        .as_f64(),
        Some(300.0)
    );
    atomFireTimer(0);
    assert_eq!(done.get(), 1);
    atomUnmount();
    assert_eq!(atomListenerCount("resize"), 0);

    atomFresh();
    let untouched = std::rc::Rc::new(std::cell::Cell::new(0_u32));
    let untouched_count = untouched.clone();
    let on_done = Closure::wrap(
        Box::new(move || untouched_count.set(untouched_count.get() + 1)) as Box<dyn FnMut()>,
    )
    .into_js_value();
    let plain = atomRender(
        &toast_component().unwrap(),
        &props(&[("text", JsValue::from_str("Plain")), ("onDone", on_done)]),
    );
    assert_eq!(children(&property(&plain, "portal")).length(), 1);
    assert_eq!(atomTimers().length(), 2);
    atomUnmount();
    assert_eq!(
        property(&atomTimers().get(1), "active").as_bool(),
        Some(false)
    );
    atomFireTimer(1);
    assert_eq!(untouched.get(), 0);
}
