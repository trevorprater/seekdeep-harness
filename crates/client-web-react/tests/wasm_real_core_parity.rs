//! Assembled live WASM renderer over the actual Rust Client Slot registry.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_runtime::WasmClientSlotRegistry;
use seekdeep_client_web_react::{
    configure_client_web_react, create_selector_shim, create_slot_renderer,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function actualBench() {
  const hooks = new WeakMap()
  let current, cursor = 0
  class Component { constructor(props) { this.props = props; this.state = {} } }
  const React = {
    Component, Fragment: Symbol('Fragment'),
    createContext(initial) {
      const context = { current: initial }
      function Provider() {}
      Provider.__context = context
      context.Provider = Provider
      return context
    },
    useContext(context) { return context.current },
    useRef(initial) {
      let row = hooks.get(current); if (!row) hooks.set(current, row = [])
      const seat = cursor++; if (!(seat in row)) row[seat] = { current: initial }
      return row[seat]
    },
    useState(initial) {
      let row = hooks.get(current); if (!row) hooks.set(current, row = [])
      const seat = cursor++; if (!(seat in row)) row[seat] = initial
      return [row[seat], value => { row[seat] = typeof value === 'function' ? value(row[seat]) : value }]
    },
    useMemo(factory, deps) {
      let row = hooks.get(current); if (!row) hooks.set(current, row = [])
      const seat = cursor++; const before = row[seat]
      if (!before || deps.some((value, index) => !Object.is(value, before.deps[index]))) row[seat] = { deps: [...deps], value: factory() }
      return row[seat].value
    },
    useSyncExternalStore(_subscribe, getSnapshot) { return getSnapshot() },
    createElement(kind, props, ...children) {
      props = { ...(props ?? {}) }
      if (children.length === 1) props.children = children[0]
      else if (children.length > 1) props.children = children
      return { kind, props, children }
    },
  }
  const mount = node => {
    if (node === undefined || node === null || node === false) return null
    if (Array.isArray(node)) return node.map(mount)
    if (typeof node !== 'object' || !('kind' in node)) return node
    const { kind, props } = node
    if (kind?.__context) {
      const context = kind.__context, before = context.current
      context.current = props.value
      try { return mount(props.children) } finally { context.current = before }
    }
    if (typeof kind === 'function' && kind.prototype instanceof Component) {
      const instance = new kind(props)
      try { return mount(instance.render()) }
      catch (error) {
        instance.state = { ...instance.state, ...kind.getDerivedStateFromError(error) }
        instance.componentDidCatch?.(error)
        return mount(instance.render())
      }
    }
    if (typeof kind === 'function') {
      const before = current, beforeCursor = cursor; current = kind; cursor = 0
      try { return mount(kind(props)) } finally { current = before; cursor = beforeCursor }
    }
    if (typeof kind === 'symbol') return mount(props.children)
    return { kind, props, children: node.children.map(mount) }
  }
  const observable = value => ({ getSnapshot: () => value, subscribe: () => () => {} })
  const absent = { sessionId: undefined, hooks: {}, props: {} }
  const sessions = { list: observable({ ids: [] }), provideInfo: observable(absent) }
  const workspaces = { list: observable({ items: [] }) }
  const effects = []
  const caller = {
    fiber: { name: 'actual-core-test' },
    effect(install) { const dispose = install(); effects.push(dispose); return dispose },
  }
  let captured
  const rootComponent = props => {
    captured = props.renderSlot
    return props.renderSlot('actual.single', { label: 'owner' }, { fallback: 'none' })
  }
  const childComponent = props => `child:${props.label}`
  return { React, mount, sessions, workspaces, caller, rootComponent, childComponent, captured: () => captured }
}
export function actualRootOptions() { return { name: 'root', children: { 'actual.single': { kind: 'single', scope: 'root' } } } }
export function actualChildOptions() { return { name: 'actual.single' } }
export function actualMount(bench, node) { return bench.mount(node) }
export function actualText(node) {
  if (node === undefined || node === null || node === false) return ''
  if (Array.isArray(node)) return node.map(actualText).join('')
  if (typeof node !== 'object') return String(node)
  return (node.children ?? []).map(actualText).join('')
}
export function actualCaptured(bench) { return bench.captured() }
"#)]
extern "C" {
    fn actualBench() -> JsValue;
    fn actualRootOptions() -> JsValue;
    fn actualChildOptions() -> JsValue;
    fn actualMount(bench: &JsValue, node: &JsValue) -> JsValue;
    fn actualText(node: &JsValue) -> String;
    fn actualCaptured(bench: &JsValue) -> Function;
}

fn property(value: &JsValue, name: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(name)).unwrap()
}

fn call(value: &JsValue, name: &str, arguments: &[JsValue]) -> JsValue {
    let function = property(value, name).dyn_into::<Function>().unwrap();
    let args = Array::new();
    for argument in arguments {
        args.push(argument);
    }
    function.apply(value, &args).unwrap()
}

#[wasm_bindgen_test]
fn actual_rust_registry_register_install_render_dispose_and_stale_binding_are_connected() {
    let bench = actualBench();
    let react = property(&bench, "React");
    let selector = create_selector_shim(react.clone()).unwrap();
    configure_client_web_react(react, selector.into()).unwrap();

    let registry = WasmClientSlotRegistry::new(None);
    registry.install_sessions(property(&bench, "sessions"));
    registry.install_workspaces(property(&bench, "workspaces"));
    let face = registry.face_for(property(&bench, "caller")).unwrap();
    call(&face, "install", &[create_slot_renderer().unwrap()]);
    let root_dispose = call(
        &face,
        "register",
        &[actualRootOptions(), property(&bench, "rootComponent")],
    )
    .dyn_into::<Function>()
    .unwrap();

    let render = || {
        actualMount(
            &bench,
            &call(
                &face,
                "renderSlot",
                &[JsValue::from_str("root"), Object::new().into()],
            ),
        )
    };
    assert_eq!(actualText(&render()), "none");
    let child_dispose = call(
        &face,
        "register",
        &[actualChildOptions(), property(&bench, "childComponent")],
    )
    .dyn_into::<Function>()
    .unwrap();
    assert_eq!(actualText(&render()), "child:owner");
    let retained = actualCaptured(&bench);
    child_dispose.call0(&JsValue::UNDEFINED).unwrap();
    child_dispose.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(actualText(&render()), "none");

    root_dispose.call0(&JsValue::UNDEFINED).unwrap();
    let error = retained
        .call2(
            &JsValue::UNDEFINED,
            &JsValue::from_str("actual.single"),
            &Object::new(),
        )
        .unwrap_err();
    assert_eq!(
        property(&error, "name").as_string().as_deref(),
        Some("StaleAuthorizationError")
    );
}
