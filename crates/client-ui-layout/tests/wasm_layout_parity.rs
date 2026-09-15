//! Live WASM registration, store, theme, frame, resize, and drag parity.

#![cfg(target_arch = "wasm32")]
#![allow(clippy::float_cmp)] // The source contract produces exact integral pixel widths.

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_layout::{
    WasmLayoutController, apply_client_ui_layout, configure_client_ui_layout, layout_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
function styleDeclaration() {
  const values = new Map()
  return {
    colorScheme: '',
    setProperty(name, value) { values.set(name, String(value)) },
    removeProperty(name) {
      if (name === 'color-scheme') this.colorScheme = ''
      const before = values.get(name) ?? ''
      values.delete(name)
      return before
    },
    getPropertyValue(name) { return values.get(name) ?? '' },
  }
}

export function layoutBench() {
  let frameWidth = 1920
  let nextFrame = 1
  const frames = new Map()
  const observers = new Set()
  const rootStyle = styleDeclaration()
  const bodyStyle = styleDeclaration()
  const bodyAttributes = new Set()
  const head = {
    children: [],
    append(node) { node.isConnected = true; this.children.push(node) },
    appendChild(node) { node.isConnected = true; this.children.push(node); return node },
  }
  const body = {
    style: bodyStyle,
    setAttribute(name) { bodyAttributes.add(name) },
    removeAttribute(name) { bodyAttributes.delete(name) },
    hasAttribute(name) { return bodyAttributes.has(name) },
  }
  const document = {
    head,
    body,
    documentElement: { style: rootStyle },
    createElement(tag) {
      const node = {
        tag, attrs: {}, name: '', content: '', textContent: '', isConnected: false,
        setAttribute(name, value) { this.attrs[name] = value },
        remove() {
          this.isConnected = false
          const index = head.children.indexOf(this)
          if (index >= 0) head.children.splice(index, 1)
        },
      }
      return node
    },
  }
  globalThis.window = { innerWidth: frameWidth }
  globalThis.document = document
  globalThis.getComputedStyle = () => ({
    backgroundColor: bodyAttributes.has('data-ds-dark-theme')
      ? 'rgb(21, 21, 23)'
      : 'rgb(255, 255, 255)',
  })
  globalThis.requestAnimationFrame = callback => {
    const id = nextFrame++
    frames.set(id, callback)
    return id
  }
  globalThis.cancelAnimationFrame = id => { frames.delete(id) }
  globalThis.ResizeObserver = class {
    constructor(callback) { this.callback = callback; this.connected = true; observers.add(this) }
    observe(element) { this.element = element }
    disconnect() { this.connected = false; observers.delete(this) }
  }
  const flushFrames = () => {
    for (const [id, callback] of [...frames]) {
      frames.delete(id)
      callback(0)
    }
  }
  const resize = width => {
    frameWidth = width
    globalThis.window.innerWidth = width
    for (const observer of [...observers]) observer.callback([], observer)
    flushFrames()
  }
  const queueResize = width => {
    frameWidth = width
    globalThis.window.innerWidth = width
    for (const observer of [...observers]) observer.callback([], observer)
  }

  const hookRows = new WeakMap()
  let current
  let cursor = 0
  let pendingEffects = []
  const React = {
    Fragment: Symbol('Fragment'),
    createElement(kind, props, ...children) {
      props ||= {}
      if (typeof kind === 'function') return React.__render(kind, props)
      const node = {
        kind: typeof kind === 'symbol' ? 'Fragment' : kind,
        props,
        children,
      }
      const captured = new Set()
      node.element = {
        getBoundingClientRect: () => ({
          width: frameWidth, height: 1080, top: 0, left: 0,
          right: frameWidth, bottom: 1080,
        }),
        setPointerCapture(id) { captured.add(id) },
        releasePointerCapture(id) { captured.delete(id) },
        hasPointerCapture(id) { return captured.has(id) },
      }
      if (props.ref && typeof props.ref === 'object') props.ref.current = node.element
      return node
    },
    __render(fn, props) {
      const before = current
      const beforeCursor = cursor
      current = fn
      cursor = 0
      let tree
      try { tree = fn(props) }
      finally { current = before; cursor = beforeCursor }
      const effects = pendingEffects
      pendingEffects = []
      for (const pending of effects) {
        pending.previous?.cleanup?.()
        const cleanup = pending.effect()
        pending.row[pending.seat] = { deps: pending.deps, cleanup }
      }
      return tree
    },
    __dispose(fn) {
      for (const value of hookRows.get(fn) ?? []) value?.cleanup?.()
      hookRows.delete(fn)
    },
    useState(initial) {
      let row = hookRows.get(current)
      if (!row) hookRows.set(current, row = [])
      const seat = cursor++
      if (!(seat in row)) row[seat] = typeof initial === 'function' ? initial() : initial
      return [row[seat], update => {
        row[seat] = typeof update === 'function' ? update(row[seat]) : update
      }]
    },
    useRef(initial) {
      let row = hookRows.get(current)
      if (!row) hookRows.set(current, row = [])
      const seat = cursor++
      if (!(seat in row)) row[seat] = { current: initial }
      return row[seat]
    },
    useEffect(effect, deps = []) {
      let row = hookRows.get(current)
      if (!row) hookRows.set(current, row = [])
      const seat = cursor++
      const previous = row[seat]
      const changed = !previous || deps.length !== previous.deps.length
        || deps.some((value, index) => !Object.is(value, previous.deps[index]))
      if (changed) pendingEffects.push({ row, seat, previous, effect, deps: [...deps] })
    },
    useLayoutEffect(effect, deps = []) { React.useEffect(effect, deps) },
  }

  const runtime = {
    defineStore(declaration) {
      return {
        spec: declaration,
        create() {
          const state = declaration.init()
          const listeners = new Set()
          const actions = {}
          for (const [name, action] of Object.entries(declaration.actions)) {
            actions[name] = (...args) => {
              action(state, ...args)
              for (const listener of [...listeners]) listener()
            }
          }
          return {
            actions,
            getSnapshot: () => state,
            subscribe(listener) { listeners.add(listener); return () => listeners.delete(listener) },
            clearPersisted() {},
          }
        },
      }
    },
  }

  const registrations = []
  const slotCalls = []
  const effects = []
  const listeners = new Map()
  let theme = {
    preference: 'light', revision: 1, themes: [],
    active: { id: 'light-test', colorScheme: 'light', tokens: { '--layout-a': '#fff' } },
  }
  const slots = {
    register(options, component) {
      const row = { options, component }
      registrations.push(row)
      return () => {
        const index = registrations.indexOf(row)
        if (index >= 0) registrations.splice(index, 1)
      }
    },
  }
  const themeService = { getTheme() { return theme } }
  const services = { slots, theme: themeService }
  const ctx = {
    get(name) { return services[name] },
    reflect: {
      provide(name, value) {
        services[name] = value
        return () => { if (services[name] === value) delete services[name] }
      },
    },
    effect(install, label) {
      const dispose = install()
      const row = { label, dispose }
      effects.push(row)
      return () => {
        const index = effects.indexOf(row)
        if (index >= 0) effects.splice(index, 1)
        dispose?.()
      }
    },
    on(name, listener) {
      listeners.set(name, listener)
      return () => { if (listeners.get(name) === listener) listeners.delete(name) }
    },
  }
  const sessionState = {
    current: 's-test',
    byId: { 's-test': { id: 's-test', blank: false } },
  }
  let instance
  const ensureInstance = () => {
    if (!instance) {
      const registration = registrations[0]
      instance = registration.options.store.create()
      registration.options.inject(instance.actions)
    }
    return instance
  }
  const props = () => {
    const store = ensureInstance()
    return {
      useStore(selector) { return selector(store.getSnapshot()) },
      useSessions(selector) { return selector(sessionState) },
      actions: store.actions,
      renderSlot(name, owner) {
        slotCalls.push({ name, owner })
        return { kind: 'slot', props: { name, owner }, children: [] }
      },
    }
  }
  const render = () => React.__render(registrations[0].component, props())
  const emitTheme = (scheme, tokens = {}) => {
    theme = {
      preference: scheme, revision: theme.revision + 1, themes: [],
      active: { id: `${scheme}-test`, colorScheme: scheme, tokens },
    }
    listeners.get('theme/change')?.(theme)
  }
  const disposeAll = () => {
    React.__dispose(registrations[0]?.component)
    for (const row of [...effects].reverse()) row.dispose?.()
    effects.length = 0
  }
  return {
    React, runtime, ctx, services, registrations, slotCalls, effects, listeners,
    document, rootStyle, bodyStyle, bodyAttributes, sessionState, ensureInstance,
    render, emitTheme, disposeAll, flushFrames, resize, queueResize, frames,
    frameWidth: () => frameWidth,
  }
}

export function layoutRegistration(bench) { return bench.registrations[0] }
export function layoutService(bench) { return bench.services.layout }
export function layoutState(bench) { return bench.ensureInstance().getSnapshot() }
export function layoutActions(bench) { return bench.ensureInstance().actions }
export function layoutRender(bench) { return bench.render() }
export function layoutResize(bench, width) { bench.resize(width) }
export function layoutQueueResize(bench, width) { bench.queueResize(width) }
export function layoutFlushFrames(bench) { bench.flushFrames() }
export function layoutFrameCount(bench) { return bench.frames.size }
export function layoutEmitTheme(bench, scheme) {
  bench.emitTheme(scheme, scheme === 'dark'
    ? { '--layout-a': '#111', '--layout-b': '#eee' }
    : { '--layout-a': '#fff' })
}
export function layoutDispose(bench) { bench.disposeAll() }
export function layoutRootScheme(bench) { return bench.rootStyle.colorScheme }
export function layoutBodyDark(bench) { return bench.bodyAttributes.has('data-ds-dark-theme') }
export function layoutBodyToken(bench, name) { return bench.bodyStyle.getPropertyValue(name) }
export function layoutThemeMetaCount(bench) {
  return bench.document.head.children.filter(node => node.name === 'theme-color').length
}
export function layoutEffectLabels(bench) { return bench.effects.map(row => row.label) }
export function layoutTracks(node) { return node.props.style.gridTemplateColumns }
export function layoutFind(node, property, value) {
  if (!node) return undefined
  if (node.props?.[property] === value) return node
  for (const child of node.children ?? []) {
    const found = layoutFind(child, property, value)
    if (found) return found
  }
  return undefined
}
export function layoutFindAll(node, property, value, out = []) {
  if (!node) return out
  if (node.props?.[property] === value) out.push(node)
  for (const child of node.children ?? []) layoutFindAll(child, property, value, out)
  return out
}
export function layoutSlotCalls(bench) { return bench.slotCalls }
export function layoutSetSession(bench, id, blank) {
  bench.sessionState.current = id
  if (id !== undefined) bench.sessionState.byId[id] = { id, blank }
}
export function layoutDispatchPointer(node, name, x, pointerId = 1) {
  const event = {
    currentTarget: node.element,
    pointerId,
    clientX: x,
    preventDefault() { this.defaultPrevented = true },
  }
  node.props[name](event)
  return event.defaultPrevented === true
}
export function layoutStylesheetCount(bench) {
  return bench.document.head.children.filter(node => node.attrs?.['data-plugin'] === '@seekdeep-ai/seekdeep-client-ui-layout').length
}
export function layoutProduce(state, recipe) {
  const draft = JSON.parse(JSON.stringify(state))
  const replacement = recipe(draft)
  return replacement === undefined ? draft : replacement
}
export function layoutProduceFunction() { return layoutProduce }
"#)]
extern "C" {
    fn layoutBench() -> JsValue;
    fn layoutRegistration(bench: &JsValue) -> JsValue;
    fn layoutService(bench: &JsValue) -> JsValue;
    fn layoutState(bench: &JsValue) -> JsValue;
    fn layoutActions(bench: &JsValue) -> JsValue;
    fn layoutRender(bench: &JsValue) -> JsValue;
    fn layoutResize(bench: &JsValue, width: f64);
    fn layoutQueueResize(bench: &JsValue, width: f64);
    fn layoutFlushFrames(bench: &JsValue);
    fn layoutFrameCount(bench: &JsValue) -> u32;
    fn layoutEmitTheme(bench: &JsValue, scheme: &str);
    fn layoutDispose(bench: &JsValue);
    fn layoutRootScheme(bench: &JsValue) -> String;
    fn layoutBodyDark(bench: &JsValue) -> bool;
    fn layoutBodyToken(bench: &JsValue, name: &str) -> String;
    fn layoutThemeMetaCount(bench: &JsValue) -> u32;
    fn layoutEffectLabels(bench: &JsValue) -> Array;
    fn layoutTracks(node: &JsValue) -> String;
    fn layoutFind(node: &JsValue, property: &str, value: &str) -> JsValue;
    fn layoutFindAll(node: &JsValue, property: &str, value: &str) -> Array;
    fn layoutSlotCalls(bench: &JsValue) -> Array;
    fn layoutSetSession(bench: &JsValue, id: JsValue, blank: bool);
    fn layoutDispatchPointer(node: &JsValue, name: &str, x: f64, pointer_id: u32) -> bool;
    fn layoutStylesheetCount(bench: &JsValue) -> u32;
    fn layoutProduceFunction() -> Function;
}

fn property(value: &JsValue, name: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(name)).unwrap()
}

fn call(value: &JsValue, name: &str, args: &[JsValue]) -> JsValue {
    let function = property(value, name).dyn_into::<Function>().unwrap();
    let arguments = Array::new();
    for argument in args {
        arguments.push(argument);
    }
    function.apply(value, &arguments).unwrap()
}

fn number(value: &JsValue, name: &str) -> f64 {
    property(value, name).as_f64().unwrap()
}

fn bool_property(value: &JsValue, name: &str) -> bool {
    property(value, name).as_bool().unwrap_or(false)
}

fn actual_runtime_module() -> JsValue {
    seekdeep_client_runtime::install_store_produce(layoutProduceFunction());
    let module = Object::new();
    let define = wasm_bindgen::closure::Closure::wrap(Box::new(|declaration: JsValue| {
        seekdeep_client_runtime::define_store(declaration)
    })
        as Box<dyn FnMut(JsValue) -> Result<JsValue, JsValue>>);
    Reflect::set(
        &module,
        &JsValue::from_str("defineStore"),
        &define.into_js_value(),
    )
    .unwrap();
    module.into()
}

#[wasm_bindgen_test]
fn apply_owns_exact_service_registration_store_theme_and_teardown() {
    let unwired = WasmLayoutController::new();
    let error = unwired.toggle_sidebar().unwrap_err();
    assert!(
        property(&error, "message")
            .as_string()
            .unwrap()
            .contains("panel actions not wired")
    );

    let bench = layoutBench();
    configure_client_ui_layout(property(&bench, "React"), actual_runtime_module()).unwrap();
    assert_eq!(layoutStylesheetCount(&bench), 1);
    apply_client_ui_layout(property(&bench, "ctx")).unwrap();
    assert_eq!(
        layout_inject()
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        ["slots", "theme"]
    );
    assert_eq!(
        layoutEffectLabels(&bench)
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        [
            "ui-layout: service + root registration",
            "ui-layout: theme presenter"
        ]
    );
    let registration = layoutRegistration(&bench);
    let options = property(&registration, "options");
    assert_eq!(
        property(&options, "name").as_string().as_deref(),
        Some("root")
    );
    let children = property(&options, "children");
    for (name, kind, scope) in [
        ("sidebar", "single", "root"),
        ("conversation", "single", "session-maybe"),
        ("details", "single", "session"),
        ("shell.overlay", "list", "root"),
    ] {
        let child = property(&children, name);
        assert_eq!(property(&child, "kind").as_string().as_deref(), Some(kind));
        assert_eq!(
            property(&child, "scope").as_string().as_deref(),
            Some(scope)
        );
    }

    let state = layoutState(&bench);
    assert_eq!(number(&state, "sidebar"), 280.0);
    assert_eq!(number(&state, "details"), 0.0);
    assert!(!bool_property(&state, "narrow"));
    let actions = layoutActions(&bench);
    call(&actions, "setSidebar", &[JsValue::from_f64(9_999.0)]);
    call(&actions, "setDetails", &[JsValue::from_f64(1.0)]);
    assert_eq!(number(&layoutState(&bench), "sidebar"), 420.0);
    assert_eq!(number(&layoutState(&bench), "details"), 300.0);
    call(&actions, "closeDetails", &[]);
    call(&layoutService(&bench), "openDetails", &[]);
    assert_eq!(number(&layoutState(&bench), "details"), 360.0);
    call(&layoutService(&bench), "toggleSidebar", &[]);
    assert_eq!(number(&layoutState(&bench), "sidebar"), 0.0);
    call(&layoutService(&bench), "closeDetails", &[]);
    assert_eq!(number(&layoutState(&bench), "details"), 0.0);

    assert_eq!(layoutRootScheme(&bench), "light");
    assert!(!layoutBodyDark(&bench));
    assert_eq!(layoutBodyToken(&bench, "--layout-a"), "#fff");
    assert_eq!(layoutThemeMetaCount(&bench), 1);
    layoutEmitTheme(&bench, "dark");
    assert_eq!(layoutRootScheme(&bench), "dark");
    assert!(layoutBodyDark(&bench));
    assert_eq!(layoutBodyToken(&bench, "--layout-a"), "#111");
    assert_eq!(layoutBodyToken(&bench, "--layout-b"), "#eee");
    layoutEmitTheme(&bench, "light");
    assert_eq!(layoutBodyToken(&bench, "--layout-a"), "#fff");
    assert_eq!(layoutBodyToken(&bench, "--layout-b"), "");
    assert_eq!(layoutThemeMetaCount(&bench), 1);

    layoutDispose(&bench);
    assert!(layoutService(&bench).is_undefined());
    assert!(layoutRegistration(&bench).is_undefined());
    assert_eq!(layoutRootScheme(&bench), "");
    assert!(!layoutBodyDark(&bench));
    assert_eq!(layoutBodyToken(&bench, "--layout-a"), "");
    assert_eq!(layoutThemeMetaCount(&bench), 0);
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)]
fn frame_preserves_slots_concessions_sessions_narrow_override_and_drag_protocol() {
    let bench = layoutBench();
    configure_client_ui_layout(property(&bench, "React"), actual_runtime_module()).unwrap();
    apply_client_ui_layout(property(&bench, "ctx")).unwrap();

    let wide = layoutRender(&bench);
    assert_eq!(layoutTracks(&wide), "280px minmax(0, 1fr) 0px");
    assert_eq!(layoutFindAll(&wide, "data-side", "sidebar").length(), 1);
    assert_eq!(layoutFindAll(&wide, "data-side", "details").length(), 0);
    assert!(!layoutFind(&wide, "className", "seekdeep-layout-details-col").is_undefined());
    assert!(property(&wide, "props").is_object());
    let calls = layoutSlotCalls(&bench);
    assert_eq!(calls.length(), 4);
    assert_eq!(
        property(&calls.get(0), "name").as_string().as_deref(),
        Some("sidebar")
    );
    let sidebar_owner = property(&calls.get(0), "owner");
    assert!(!bool_property(&sidebar_owner, "collapsed"));
    assert_eq!(number(&sidebar_owner, "width"), 280.0);

    call(&layoutActions(&bench), "openDetails", &[]);
    let details_open = layoutRender(&bench);
    assert_eq!(layoutTracks(&details_open), "280px minmax(0, 1fr) 360px");
    assert_eq!(
        layoutFindAll(&details_open, "data-side", "details").length(),
        1
    );

    layoutResize(&bench, 1_250.0);
    let conceded = layoutRender(&bench);
    assert_eq!(layoutTracks(&conceded), "280px minmax(0, 1fr) 330px");
    let conceded_handle = layoutFind(&conceded, "data-side", "details");
    layoutDispatchPointer(&conceded_handle, "onPointerDown", 920.0, 7);
    layoutDispatchPointer(&conceded_handle, "onPointerMove", 930.0, 7);
    layoutFlushFrames(&bench);
    layoutDispatchPointer(&conceded_handle, "onPointerUp", 930.0, 7);
    assert_eq!(number(&layoutState(&bench), "details"), 320.0);
    call(
        &layoutActions(&bench),
        "setDetails",
        &[JsValue::from_f64(360.0)],
    );
    layoutResize(&bench, 1_920.0);
    let restored = layoutRender(&bench);
    assert_eq!(layoutTracks(&restored), "280px minmax(0, 1fr) 360px");

    let details_handle = layoutFind(&restored, "data-side", "details");
    assert!(layoutDispatchPointer(
        &details_handle,
        "onPointerDown",
        1_560.0,
        1
    ));
    assert!(!layoutDispatchPointer(
        &details_handle,
        "onPointerMove",
        1_520.0,
        1
    ));
    assert!(!layoutDispatchPointer(
        &details_handle,
        "onPointerMove",
        1_500.0,
        1
    ));
    assert_eq!(layoutFrameCount(&bench), 1);
    layoutFlushFrames(&bench);
    assert_eq!(number(&layoutState(&bench), "details"), 420.0);
    assert!(!layoutDispatchPointer(
        &details_handle,
        "onPointerUp",
        1_500.0,
        1
    ));
    assert_eq!(
        layoutTracks(&layoutRender(&bench)),
        "280px minmax(0, 1fr) 420px"
    );

    call(
        &layoutActions(&bench),
        "setSidebar",
        &[JsValue::from_f64(400.0)],
    );
    layoutResize(&bench, 980.0);
    let collapsed = layoutRender(&bench);
    assert_eq!(layoutTracks(&collapsed), "56px minmax(0, 1fr) 0px");
    assert!(bool_property(
        &property(&collapsed, "props"),
        "data-sidebar-collapsed"
    ));
    assert_eq!(
        layoutFindAll(&collapsed, "data-side", "sidebar").length(),
        0
    );
    call(&layoutService(&bench), "toggleSidebar", &[]);
    let narrow_expanded = layoutRender(&bench);
    assert_eq!(layoutTracks(&narrow_expanded), "400px minmax(0, 1fr) 0px");
    call(&layoutService(&bench), "toggleSidebar", &[]);
    assert_eq!(
        layoutTracks(&layoutRender(&bench)),
        "56px minmax(0, 1fr) 0px"
    );
    layoutResize(&bench, 1_920.0);
    assert_eq!(
        layoutTracks(&layoutRender(&bench)),
        "400px minmax(0, 1fr) 420px"
    );

    layoutSetSession(&bench, JsValue::from_str("s-next"), false);
    layoutRender(&bench);
    let after_switch = layoutRender(&bench);
    assert_eq!(layoutTracks(&after_switch), "400px minmax(0, 1fr) 0px");
    call(&layoutActions(&bench), "openDetails", &[]);
    layoutSetSession(&bench, JsValue::from_str("s-blank"), true);
    let blank = layoutRender(&bench);
    assert_eq!(layoutTracks(&blank), "400px minmax(0, 1fr) 0px");
    assert_eq!(number(&layoutState(&bench), "details"), 360.0);
    layoutSetSession(&bench, JsValue::from_str("s-next"), false);
    assert_eq!(
        layoutTracks(&layoutRender(&bench)),
        "400px minmax(0, 1fr) 360px"
    );
    layoutSetSession(&bench, JsValue::UNDEFINED, false);
    let no_session = layoutRender(&bench);
    assert_eq!(layoutTracks(&no_session), "400px minmax(0, 1fr) 0px");
    assert!(!layoutFind(&no_session, "className", "seekdeep-layout-overlay-layer").is_undefined());
}

#[wasm_bindgen_test]
fn pointer_guards_and_resize_cleanup_preserve_pending_frame_boundaries() {
    let bench = layoutBench();
    configure_client_ui_layout(property(&bench, "React"), actual_runtime_module()).unwrap();
    apply_client_ui_layout(property(&bench, "ctx")).unwrap();
    let root = layoutRender(&bench);
    let handle = layoutFind(&root, "data-side", "sidebar");
    let before = number(&layoutState(&bench), "sidebar");
    layoutDispatchPointer(&handle, "onPointerMove", 500.0, 9);
    layoutDispatchPointer(&handle, "onPointerUp", 500.0, 9);
    assert_eq!(number(&layoutState(&bench), "sidebar"), before);
    assert_eq!(layoutFrameCount(&bench), 0);

    layoutDispatchPointer(&handle, "onPointerDown", 280.0, 1);
    layoutDispatchPointer(&handle, "onPointerMove", 360.0, 1);
    assert_eq!(layoutFrameCount(&bench), 1);
    layoutDispatchPointer(&handle, "onPointerUp", 360.0, 1);
    assert_eq!(layoutFrameCount(&bench), 0);
    assert_eq!(number(&layoutState(&bench), "sidebar"), 360.0);

    layoutResize(&bench, 0.0);
    assert_eq!(
        layoutTracks(&layoutRender(&bench)),
        "360px minmax(0, 1fr) 0px"
    );
    layoutQueueResize(&bench, 800.0);
    assert_eq!(layoutFrameCount(&bench), 1);
    layoutDispose(&bench);
    assert_eq!(layoutFrameCount(&bench), 0);
}
