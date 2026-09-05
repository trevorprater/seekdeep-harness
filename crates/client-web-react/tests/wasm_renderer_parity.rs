//! Live WASM root, slot-kind, authorization, kit, inject, error, and session renderer parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Reflect};
use seekdeep_client_web_react::{
    configure_client_web_react, create_selector_shim, create_slot_renderer,
};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function rendererBench() {
  const hooks = new WeakMap()
  let current
  let cursor = 0
  class Component { constructor(props) { this.props = props; this.state = {} } }
  const React = {
    Component,
    Fragment: Symbol('Fragment'),
    createContext(initial) {
      const context = { current: initial }
      context.Provider = function Provider() {}
      context.Provider.__context = context
      return context
    },
    useContext(context) { return context.current },
    useSyncExternalStore(subscribe, getSnapshot) {
      let row = hooks.get(current)
      if (!row) hooks.set(current, row = [])
      const seat = cursor++
      if (!(seat in row)) row[seat] = { cleanup: subscribe(() => {}) }
      return getSnapshot()
    },
    useState(initial) {
      let row = hooks.get(current)
      if (!row) hooks.set(current, row = [])
      const seat = cursor++
      if (!(seat in row)) row[seat] = initial
      return [row[seat], value => { row[seat] = typeof value === 'function' ? value(row[seat]) : value }]
    },
    useRef(initial) {
      let row = hooks.get(current)
      if (!row) hooks.set(current, row = [])
      const seat = cursor++
      if (!(seat in row)) row[seat] = { current: initial }
      return row[seat]
    },
    createElement(kind, props, ...children) {
      props = { ...(props ?? {}) }
      if (children.length === 1) props.children = children[0]
      else if (children.length > 1) props.children = children
      return { kind, props, children }
    },
    renderFunction(fn, props) {
      const before = current
      const beforeCursor = cursor
      current = fn
      cursor = 0
      try { return fn(props) }
      finally { current = before; cursor = beforeCursor }
    },
  }
  const useSelector = (_subscribe, getSnapshot, _server, selector) => selector(getSnapshot())
  const mount = node => {
    if (node === undefined || node === null || node === false) return null
    if (Array.isArray(node)) return node.map(mount)
    if (typeof node !== 'object' || !('kind' in node)) return node
    const { kind, props } = node
    if (kind?.__context) {
      const context = kind.__context
      const before = context.current
      context.current = props.value
      try { return mount(props.children) }
      finally { context.current = before }
    }
    if (typeof kind === 'function' && kind.prototype instanceof Component) {
      const instance = new kind(props)
      try { return mount(instance.render()) }
      catch (error) {
        const next = kind.getDerivedStateFromError(error)
        instance.state = { ...instance.state, ...next }
        instance.componentDidCatch?.(error)
        return mount(instance.render())
      }
    }
    if (typeof kind === 'function') return mount(React.renderFunction(kind, props))
    if (typeof kind === 'symbol') return mount(props.children)
    return { kind, props, children: (node.children ?? []).map(mount) }
  }

  const entries = new Map()
  const specs = new Map([['root', { kind: 'single', scope: 'root' }]])
  const live = new Set()
  const abdicated = new Set()
  const versions = new Map()
  const subscribers = new Map()
  const errors = []
  const list = observable({ ids: [] })
  const workspaces = observable({ items: [] })
  const absentInfo = { sessionId: undefined, hooks: {}, props: {} }
  const provideInfo = observable(absentInfo)
  const stores = new WeakMap()
  const host = {
    subscribe(key, listener) {
      let rows = subscribers.get(key)
      if (!rows) subscribers.set(key, rows = new Set())
      rows.add(listener)
      return () => rows.delete(listener)
    },
    getVersion(key) { return versions.get(key) ?? 0 },
    entriesOf(key) { return entries.get(key) ?? [] },
    entriesOfSlot(key) {
      const all = entries.get(key) ?? []
      const kind = specs.get(key)?.kind
      if (kind === 'chain') return all
      const output = []
      const cells = new Set()
      for (const entry of all) {
        if (abdicated.has(entry)) continue
        const cell = kind === 'keyed' ? entry.options.key : kind === 'list' ? entry.options.id : undefined
        if (cells.has(cell)) continue
        cells.add(cell)
        output.push(entry)
      }
      return output
    },
    reportEntryError(key, entry, error, info) {
      errors.push({ key, entry, error, info })
      if (info.abdicate) abdicated.add(entry)
    },
    specOf(key) { return specs.get(key) },
    isLive(entry) { return live.has(entry) },
    storeOf(entry) { return stores.get(entry) },
    sessions: { list, provideInfo },
    workspaces: { list: workspaces },
  }
  const bump = key => {
    versions.set(key, (versions.get(key) ?? 0) + 1)
    for (const listener of [...subscribers.get(key) ?? []]) listener()
  }
  const declare = (key, spec) => { specs.set(key, spec); bump(key) }
  const register = (key, entry) => {
    entry.options ||= {}
    const rows = [...entries.get(key) ?? [], entry]
    rows.sort((a, b) => (a.options.priority ?? 0) - (b.options.priority ?? 0)
      || (kindOf(key) === 'list' ? (a.options.order ?? 0) - (b.options.order ?? 0) : 0))
    entries.set(key, rows)
    live.add(entry)
    bump(key)
    return () => {
      entries.set(key, (entries.get(key) ?? []).filter(candidate => candidate !== entry))
      live.delete(entry)
      bump(key)
    }
  }
  const kindOf = key => specs.get(key)?.kind
  const setStore = (entry, store) => stores.set(entry, store)
  const render = (renderer, owner = {}) => mount(renderer.renderRoot(host, owner))
  return {
    React, useSelector, host, render, mount, declare, register, setStore,
    provideInfo, absentInfo, errors, entries, specs,
  }
}
function observable(value) {
  return { value, getSnapshot() { return this.value }, subscribe() { return () => {} } }
}
export function rendererRoot(bench, children, capture) {
  const entry = {
    children,
    component(props) {
      if (capture) capture.value = props.renderSlot
      const rendered = Object.keys(children).map(key => {
        const spec = children[key]
        if (spec.kind === 'chain') return props.renderSlotChain(key, {}, { fallback: `fallback:${key}` })
        const opts = spec.kind === 'keyed' ? { entryKey: 'wanted', fallback: `fallback:${key}` }
          : { fallback: `fallback:${key}` }
        return props.renderSlot(key, {}, opts)
      })
      return { kind: 'root-body', props: {}, children: rendered }
    },
  }
  return bench.register('root', entry)
}
export function rendererEntry(text, options = {}) { return { options, component: () => text } }
export function rendererThrow(text, options = {}) { return { options, component: () => { throw new Error(text) } } }
export function rendererChain(text, matched, priority = 0) {
  return { options: { priority }, select: () => matched, component: props => `${text}:${String(props.matched)}` }
}
export function rendererDeclare(bench, key, kind, scope) { bench.declare(key, { kind, scope }) }
export function rendererRegister(bench, key, entry) { return bench.register(key, entry) }
export function rendererRender(bench, renderer, owner) { return bench.render(renderer, owner) }
export function rendererRenderError(bench, renderer, owner) {
  try { bench.render(renderer, owner); return undefined } catch (error) { return error }
}
export function rendererText(node) {
  if (node === undefined || node === null || node === false) return ''
  if (Array.isArray(node)) return node.map(rendererText).join('')
  if (typeof node !== 'object') return String(node)
  return (node.children ?? []).map(rendererText).join('')
}
export function rendererErrors(bench) { return bench.errors }
export function rendererCapture() { return { value: undefined } }
export function rendererCaptured(capture) { return capture.value }
export function rendererCall(binding, key) { return binding(key, {}, {}) }
export function rendererSetSession(bench, id) {
  bench.provideInfo.value = id === undefined
    ? bench.absentInfo
    : { sessionId: id, hooks: { session: observable({ sid: id }) }, props: { feature: `feature:${id}` } }
}
export function rendererSessionRoot(bench) {
  const children = { session: { kind: 'single', scope: 'session' } }
  const entry = {
    children,
    component(props) { return props.renderSlot('session', {}, { fallback: 'no-session' }) },
  }
  return bench.register('root', entry)
}
export function rendererSessionEntry() {
  return { options: {}, component: props => `${props.sessionId}:${props.useSession(value => value.sid)}:${props.feature}` }
}
"#)]
extern "C" {
    fn rendererBench() -> JsValue;
    fn rendererRoot(bench: &JsValue, children: &JsValue, capture: &JsValue) -> Function;
    fn rendererEntry(text: &str, options: &JsValue) -> JsValue;
    fn rendererThrow(text: &str, options: &JsValue) -> JsValue;
    fn rendererChain(text: &str, matched: JsValue, priority: f64) -> JsValue;
    fn rendererDeclare(bench: &JsValue, key: &str, kind: &str, scope: &str);
    fn rendererRegister(bench: &JsValue, key: &str, entry: &JsValue) -> Function;
    fn rendererRender(bench: &JsValue, renderer: &JsValue, owner: &JsValue) -> JsValue;
    fn rendererRenderError(bench: &JsValue, renderer: &JsValue, owner: &JsValue) -> JsValue;
    fn rendererText(node: &JsValue) -> String;
    fn rendererErrors(bench: &JsValue) -> Array;
    fn rendererCapture() -> JsValue;
    fn rendererCaptured(capture: &JsValue) -> Function;
    fn rendererCall(binding: &Function, key: &str) -> JsValue;
    fn rendererSetSession(bench: &JsValue, id: JsValue);
    fn rendererSessionRoot(bench: &JsValue) -> Function;
    fn rendererSessionEntry() -> JsValue;
}

fn property(value: &JsValue, name: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(name)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let value = js_sys::Object::new();
    for (key, item) in entries {
        Reflect::set(&value, &JsValue::from_str(key), item).unwrap();
    }
    value.into()
}

fn configure(bench: &JsValue) {
    let react = property(bench, "React");
    let selector = create_selector_shim(react.clone()).unwrap();
    configure_client_web_react(react, selector.into()).unwrap();
}

#[wasm_bindgen_test]
fn root_slot_kinds_chain_errors_and_stale_authorization_are_live() {
    let bench = rendererBench();
    configure(&bench);
    let renderer = create_slot_renderer().unwrap();
    let error = rendererRenderError(&bench, &renderer, &object(&[]));
    assert!(
        property(&error, "message")
            .as_string()
            .unwrap()
            .contains("boot order")
    );

    let children = object(&[
        (
            "single",
            object(&[("kind", "single".into()), ("scope", "root".into())]),
        ),
        (
            "list",
            object(&[("kind", "list".into()), ("scope", "root".into())]),
        ),
        (
            "keyed",
            object(&[("kind", "keyed".into()), ("scope", "root".into())]),
        ),
        (
            "chain",
            object(&[("kind", "chain".into()), ("scope", "root".into())]),
        ),
    ]);
    for (key, kind) in [
        ("single", "single"),
        ("list", "list"),
        ("keyed", "keyed"),
        ("chain", "chain"),
    ] {
        rendererDeclare(&bench, key, kind, "root");
    }
    let capture = rendererCapture();
    let dispose_root = rendererRoot(&bench, &children, &capture);
    assert_eq!(
        rendererText(&rendererRender(&bench, &renderer, &object(&[]))),
        "fallback:singlefallback:listfallback:keyedfallback:chain"
    );
    rendererRegister(&bench, "single", &rendererEntry("S", &object(&[])));
    rendererRegister(
        &bench,
        "list",
        &rendererEntry("2", &object(&[("id", "two".into()), ("order", 2.0.into())])),
    );
    rendererRegister(
        &bench,
        "list",
        &rendererEntry("1", &object(&[("id", "one".into()), ("order", 1.0.into())])),
    );
    rendererRegister(
        &bench,
        "keyed",
        &rendererEntry("K", &object(&[("key", "wanted".into())])),
    );
    rendererRegister(
        &bench,
        "chain",
        &rendererChain("decline", JsValue::NULL, 0.0),
    );
    rendererRegister(&bench, "chain", &rendererChain("take", "yes".into(), 1.0));
    assert_eq!(
        rendererText(&rendererRender(&bench, &renderer, &object(&[]))),
        "S12Ktake:yes"
    );
    let binding = rendererCaptured(&capture);
    dispose_root.call0(&JsValue::UNDEFINED).unwrap();
    let error = binding
        .call3(
            &JsValue::UNDEFINED,
            &"missing".into(),
            &object(&[]),
            &object(&[]),
        )
        .unwrap_err();
    assert_eq!(
        property(&error, "name").as_string().as_deref(),
        Some("StaleAuthorizationError")
    );
}

#[wasm_bindgen_test]
fn crashing_shadow_abdicates_to_the_next_winner_and_session_kit_is_scoped() {
    let bench = rendererBench();
    configure(&bench);
    let renderer = create_slot_renderer().unwrap();
    rendererDeclare(&bench, "single", "single", "root");
    rendererRoot(
        &bench,
        &object(&[(
            "single",
            object(&[("kind", "single".into()), ("scope", "root".into())]),
        )]),
        &JsValue::UNDEFINED,
    );
    rendererRegister(
        &bench,
        "single",
        &rendererThrow("boom", &object(&[("priority", 0.0.into())])),
    );
    rendererRegister(
        &bench,
        "single",
        &rendererEntry("healthy", &object(&[("priority", 1.0.into())])),
    );
    assert_eq!(
        rendererText(&rendererRender(&bench, &renderer, &object(&[]))),
        ""
    );
    assert_eq!(rendererErrors(&bench).length(), 1);
    assert_eq!(
        rendererText(&rendererRender(&bench, &renderer, &object(&[]))),
        "healthy"
    );

    let session_bench = rendererBench();
    configure(&session_bench);
    let renderer = create_slot_renderer().unwrap();
    rendererDeclare(&session_bench, "session", "single", "session");
    rendererSessionRoot(&session_bench);
    rendererRegister(&session_bench, "session", &rendererSessionEntry());
    assert_eq!(
        rendererText(&rendererRender(&session_bench, &renderer, &object(&[]))),
        "no-session"
    );
    rendererSetSession(&session_bench, "s-one".into());
    assert_eq!(
        rendererText(&rendererRender(&session_bench, &renderer, &object(&[]))),
        "s-one:s-one:feature:s-one"
    );
}
