//! Live WASM registration, shell rendering, collapse, and pointer-scrollbar parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Reflect};
use seekdeep_client_ui_sidebar::{
    apply_client_ui_sidebar, configure_client_ui_sidebar, sidebar_inject,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function sidebarBench() {
  const registrations = []
  const calls = []
  const slotCalls = []
  const localeCalls = []
  const timers = new Map()
  const listeners = new Map()
  let nextTimer = 1
  let now = 0
  const window = {
    setTimeout(callback, delay) {
      const id = nextTimer++
      timers.set(id, { callback, at: now + delay })
      return id
    },
    clearTimeout(id) { timers.delete(id) },
  }
  const document = {
    head: { appendChild() {} },
    createElement() { return { setAttribute() {}, textContent: '' } },
    addEventListener(name, listener) { listeners.set(name, listener) },
    removeEventListener(name, listener) {
      if (listeners.get(name) === listener) listeners.delete(name)
    },
  }
  globalThis.window = window
  globalThis.document = document

  const states = new WeakMap()
  let current
  let cursor = 0
  const React = {
    createElement(kind, props, ...children) {
      props ||= {}
      if (typeof kind === 'function') return React.__render(kind, props)
      const node = { kind, props, children }
      if (props.ref && typeof props.ref === 'object') {
        props.ref.current = {
          getBoundingClientRect: () => ({ left: 0, right: 280, top: 0, bottom: 600 }),
        }
      }
      return node
    },
    __render(fn, props) {
      const before = current
      const beforeCursor = cursor
      current = fn
      cursor = 0
      try { return fn(props) }
      finally { current = before; cursor = beforeCursor }
    },
    useState(initial) {
      let values = states.get(current)
      if (!values) states.set(current, values = [])
      const seat = cursor++
      if (!(seat in values)) values[seat] = initial
      return [values[seat], update => {
        values[seat] = typeof update === 'function' ? update(values[seat]) : update
      }]
    },
    useRef(initial) {
      let values = states.get(current)
      if (!values) states.set(current, values = [])
      const seat = cursor++
      if (!(seat in values)) values[seat] = { current: initial }
      return values[seat]
    },
    useEffect(effect, deps = []) {
      let values = states.get(current)
      if (!values) states.set(current, values = [])
      const seat = cursor++
      const previous = values[seat]
      const changed = !previous || deps.length !== previous.deps.length
        || deps.some((value, index) => !Object.is(value, previous.deps[index]))
      if (!changed) return
      previous?.cleanup?.()
      values[seat] = { deps: [...deps], cleanup: effect() }
    },
  }
  const primitives = Object.fromEntries([
    'BrandWordmark', 'FishLogo', 'IconNewChatOutline16', 'IconPanelLeftOutline16', 'Tooltip',
  ].map(name => [name, name]))
  const slots = {
    register(options, component) {
      registrations.push({ options, component })
      return () => { registrations.splice(registrations.findIndex(row => row.component === component), 1) }
    },
  }
  const layout = { toggleSidebar() { calls.push(['toggleSidebar']) } }
  const workspaces = { startSession(id) { calls.push(['startSession', id]) } }
  const locale = {
    register(namespace, dictionaries) {
      localeCalls.push({ namespace, dictionaries })
      return () => {}
    },
  }
  const services = { slots, layout, sessions: {}, workspaces, locale }
  const ctx = {
    get(name) { return services[name] },
    effect(install) { return install() },
  }
  const dictionary = {
    'session.new': 'New Session', 'session.new.label': 'New session',
    'toggle.open': 'Open sidebar', 'toggle.collapse': 'Collapse sidebar',
  }
  const props = (collapsed, width) => ({
    collapsed, width,
    startSession(id) { calls.push(['rootStartSession', id]) },
    toggleSidebar() { calls.push(['rootToggleSidebar']) },
    t(key) { return dictionary[key] ?? key },
    renderSlot(name, owner) {
      slotCalls.push({ name, owner })
      return { kind: 'slot', props: { name, owner }, children: [] }
    },
  })
  const tick = milliseconds => {
    now += milliseconds
    for (const [id, timer] of [...timers]) {
      if (timer.at > now) continue
      timers.delete(id)
      timer.callback()
    }
  }
  return {
    React, primitives, ctx, registrations, calls, slotCalls, localeCalls,
    props, tick, listeners, timers,
  }
}
export function sidebarRegistration(bench) { return bench.registrations[0] }
export function sidebarRender(bench, component, collapsed, width) {
  return bench.React.__render(component, bench.props(collapsed, width))
}
export function sidebarFind(node, property, value) {
  if (!node) return undefined
  if (node.props?.[property] === value) return node
  for (const child of node.children ?? []) {
    const found = sidebarFind(child, property, value)
    if (found) return found
  }
  return undefined
}
export function sidebarFindAll(node, property, value, out = []) {
  if (!node) return out
  if (node.props?.[property] === value) out.push(node)
  for (const child of node.children ?? []) sidebarFindAll(child, property, value, out)
  return out
}
export function sidebarKinds(node, out = []) {
  if (!node) return out
  if (typeof node.kind === 'string') out.push(node.kind)
  for (const child of node.children ?? []) sidebarKinds(child, out)
  return out
}
export function sidebarTick(bench, milliseconds) { bench.tick(milliseconds) }
export function sidebarCalls(bench) { return bench.calls }
export function sidebarSlotCalls(bench) { return bench.slotCalls }
export function sidebarLocaleCalls(bench) { return bench.localeCalls }
export function sidebarDispatchPointer(bench, x, y) { bench.listeners.get('pointermove')?.({ clientX: x, clientY: y }) }
export function sidebarTimerCount(bench) { return bench.timers.size }
export function sidebarSignatureText(node) {
  const rows = []
  const visit = value => {
    if (value === undefined || value === null || value === false) return
    if (typeof value !== 'object') {
      rows.push(`#text:${String(value)}`)
      return
    }
    if (value.kind === 'Tooltip') {
      visit(value.children?.[0])
      return
    }
    const props = value.props ?? {}
    let row = String(value.kind)
    if (props.className) row += `.${props.className.split(' ').join('.')}`
    if (props['aria-label']) row += `[aria=${props['aria-label']}]`
    if (props.size !== undefined) row += `[size=${props.size}]`
    if (props.style?.width !== undefined) row += `[width=${props.style.width}]`
    if (value.kind === 'slot') row += `[name=${props.name}][wide=${props.owner.wide}]`
    rows.push(row)
    for (const child of value.children ?? []) visit(child)
  }
  visit(node)
  return rows.join('\n')
}
"#)]
extern "C" {
    fn sidebarBench() -> JsValue;
    fn sidebarRegistration(bench: &JsValue) -> JsValue;
    fn sidebarRender(bench: &JsValue, component: &JsValue, collapsed: bool, width: f64) -> JsValue;
    fn sidebarFind(node: &JsValue, property: &str, value: &str) -> JsValue;
    fn sidebarFindAll(node: &JsValue, property: &str, value: &str) -> Array;
    fn sidebarKinds(node: &JsValue) -> Array;
    fn sidebarTick(bench: &JsValue, milliseconds: f64);
    fn sidebarCalls(bench: &JsValue) -> Array;
    fn sidebarSlotCalls(bench: &JsValue) -> Array;
    fn sidebarLocaleCalls(bench: &JsValue) -> Array;
    fn sidebarDispatchPointer(bench: &JsValue, x: f64, y: f64);
    fn sidebarTimerCount(bench: &JsValue) -> u32;
    fn sidebarSignatureText(node: &JsValue) -> String;
}

fn property(value: &JsValue, name: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(name)).unwrap()
}

fn click(node: &JsValue) {
    property(&property(node, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
}

fn invoke(node: &JsValue, name: &str) {
    property(&property(node, "props"), name)
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
}

fn class_name(root: &JsValue) -> String {
    property(&property(root, "props"), "className")
        .as_string()
        .unwrap()
}

#[wasm_bindgen_test]
fn apply_registers_exact_contract_and_delegates_injected_actions() {
    let bench = sidebarBench();
    configure_client_ui_sidebar(property(&bench, "React"), property(&bench, "primitives")).unwrap();
    apply_client_ui_sidebar(property(&bench, "ctx")).unwrap();
    assert_eq!(
        sidebar_inject()
            .iter()
            .filter_map(|value| value.as_string())
            .collect::<Vec<_>>(),
        ["slots", "layout", "sessions", "workspaces", "locale"]
    );
    let locale_calls = sidebarLocaleCalls(&bench);
    assert_eq!(locale_calls.length(), 1);
    let locale_call = locale_calls.get(0);
    assert_eq!(
        property(&locale_call, "namespace").as_string().as_deref(),
        Some("sidebar")
    );
    let dictionaries = property(&locale_call, "dictionaries");
    for (locale, expected) in [
        (
            "zh",
            [
                ("session.new", "新会话"),
                ("session.new.label", "新建会话"),
                ("toggle.open", "打开侧边栏"),
                ("toggle.collapse", "收起侧边栏"),
            ],
        ),
        (
            "en",
            [
                ("session.new", "New Session"),
                ("session.new.label", "New session"),
                ("toggle.open", "Open sidebar"),
                ("toggle.collapse", "Collapse sidebar"),
            ],
        ),
    ] {
        let dictionary = property(&dictionaries, locale);
        for (key, value) in expected {
            assert_eq!(
                property(&dictionary, key).as_string().as_deref(),
                Some(value)
            );
        }
    }
    let registration = sidebarRegistration(&bench);
    let options = property(&registration, "options");
    assert_eq!(
        property(&options, "name").as_string().as_deref(),
        Some("sidebar")
    );
    assert_eq!(
        property(&options, "locale").as_string().as_deref(),
        Some("sidebar")
    );
    let children = property(&options, "children");
    for (name, kind) in [
        ("sidebar.workspaces", "single"),
        ("sidebar.settings", "single"),
        ("sidebar.footer.action", "list"),
    ] {
        assert_eq!(
            property(&property(&children, name), "kind")
                .as_string()
                .as_deref(),
            Some(kind)
        );
    }
    let injected = property(&options, "inject")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    property(&injected, "startSession")
        .dyn_into::<Function>()
        .unwrap()
        .call1(&JsValue::UNDEFINED, &JsValue::from_str("workspace"))
        .unwrap();
    property(&injected, "toggleSidebar")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert_eq!(sidebarCalls(&bench).length(), 2);
}

#[wasm_bindgen_test]
fn expanded_collapsed_and_pointer_linger_paths_drive_the_live_component() {
    let bench = sidebarBench();
    configure_client_ui_sidebar(property(&bench, "React"), property(&bench, "primitives")).unwrap();
    apply_client_ui_sidebar(property(&bench, "ctx")).unwrap();
    let component = property(&sidebarRegistration(&bench), "component");
    let expanded = sidebarRender(&bench, &component, false, 300.0);
    assert!(class_name(&expanded).contains("seekdeep-sidebar-quiet-bars"));
    assert_eq!(
        sidebarFindAll(&expanded, "aria-label", "New session").length(),
        2
    );
    for icon in [
        "BrandWordmark",
        "IconPanelLeftOutline16",
        "IconNewChatOutline16",
    ] {
        assert!(
            sidebarKinds(&expanded)
                .iter()
                .any(|kind| kind.as_string().as_deref() == Some(icon)),
            "{icon}"
        );
    }
    click(&sidebarFind(&expanded, "aria-label", "New session"));
    click(&sidebarFind(&expanded, "aria-label", "Collapse sidebar"));
    assert_eq!(sidebarCalls(&bench).length(), 2);
    assert_eq!(sidebarSlotCalls(&bench).length(), 3);

    let fading = sidebarRender(&bench, &component, true, 56.0);
    assert!(class_name(&fading).contains("seekdeep-sidebar-fading"));
    sidebarTick(&bench, 150.0);
    let collapsed = sidebarRender(&bench, &component, true, 56.0);
    let classes = class_name(&collapsed);
    assert!(classes.contains("seekdeep-sidebar-collapsed"));
    assert!(classes.contains("seekdeep-sidebar-rail-in"));
    assert!(!classes.contains("seekdeep-sidebar-fading"));
    assert_eq!(
        sidebarFindAll(&collapsed, "aria-label", "New session").length(),
        1
    );
    assert!(
        sidebarKinds(&collapsed)
            .iter()
            .any(|kind| kind.as_string().as_deref() == Some("FishLogo"))
    );
    let workspaces = sidebarSlotCalls(&bench)
        .iter()
        .rev()
        .find(|call| property(call, "name").as_string().as_deref() == Some("sidebar.workspaces"))
        .unwrap();
    property(&property(&workspaces, "owner"), "expandSidebar")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert!(
        sidebarCalls(&bench)
            .iter()
            .any(|call| Array::from(&call).get(0).as_string().as_deref()
                == Some("rootToggleSidebar"))
    );

    invoke(&collapsed, "onPointerEnter");
    let revealed = sidebarRender(&bench, &component, true, 56.0);
    assert!(!class_name(&revealed).contains("seekdeep-sidebar-quiet-bars"));
    invoke(&revealed, "onPointerLeave");
    assert_eq!(sidebarTimerCount(&bench), 1);
    sidebarTick(&bench, 1_999.0);
    assert!(
        !class_name(&sidebarRender(&bench, &component, true, 56.0))
            .contains("seekdeep-sidebar-quiet-bars")
    );
    sidebarTick(&bench, 1.0);
    assert!(
        class_name(&sidebarRender(&bench, &component, true, 56.0))
            .contains("seekdeep-sidebar-quiet-bars")
    );

    invoke(
        &sidebarRender(&bench, &component, true, 56.0),
        "onPointerEnter",
    );
    let inside = sidebarRender(&bench, &component, true, 56.0);
    sidebarDispatchPointer(&bench, 100.0, 100.0);
    assert_eq!(sidebarTimerCount(&bench), 0);
    sidebarDispatchPointer(&bench, 500.0, 100.0);
    assert_eq!(sidebarTimerCount(&bench), 1);
    assert!(!class_name(&inside).contains("seekdeep-sidebar-quiet-bars"));
}

#[wasm_bindgen_test]
fn expanded_and_collapsed_trees_match_the_semantic_source_snapshots() {
    let bench = sidebarBench();
    configure_client_ui_sidebar(property(&bench, "React"), property(&bench, "primitives")).unwrap();
    apply_client_ui_sidebar(property(&bench, "ctx")).unwrap();
    let component = property(&sidebarRegistration(&bench), "component");

    let expanded = sidebarRender(&bench, &component, false, 300.0);
    assert_eq!(
        sidebarSignatureText(&expanded),
        r"div.seekdeep-sidebar-root.seekdeep-sidebar-quiet-bars[width=300]
div.seekdeep-sidebar-logo-row
button.seekdeep-sidebar-brand.seekdeep-sidebar-wide[aria=New session]
BrandWordmark
button.seekdeep-sidebar-icon-button.seekdeep-sidebar-toggle[aria=Collapse sidebar]
IconPanelLeftOutline16.seekdeep-sidebar-panel-icon[size=16]
button.seekdeep-sidebar-new-session[aria=New session]
IconNewChatOutline16[size=14]
span.seekdeep-sidebar-new-session-label.seekdeep-sidebar-wide
#text:New Session
div.seekdeep-sidebar-region-area
slot[name=sidebar.workspaces][wide=true]
div.seekdeep-sidebar-foot-area
div.seekdeep-sidebar-footer-actions
slot[name=sidebar.footer.action][wide=true]
div.seekdeep-sidebar-settings-area
slot[name=sidebar.settings][wide=true]"
    );

    let fading = sidebarRender(&bench, &component, true, 56.0);
    assert!(class_name(&fading).contains("seekdeep-sidebar-fading"));
    sidebarTick(&bench, 150.0);
    let collapsed = sidebarRender(&bench, &component, true, 56.0);
    assert_eq!(
        sidebarSignatureText(&collapsed),
        r"div.seekdeep-sidebar-root.seekdeep-sidebar-collapsed.seekdeep-sidebar-rail-in.seekdeep-sidebar-quiet-bars
div.seekdeep-sidebar-logo-row
button.seekdeep-sidebar-icon-button.seekdeep-sidebar-toggle[aria=Open sidebar]
FishLogo.seekdeep-sidebar-rail-fish[size=24]
IconPanelLeftOutline16.seekdeep-sidebar-panel-icon[size=18]
button.seekdeep-sidebar-new-session[aria=New session]
IconNewChatOutline16[size=18]
div.seekdeep-sidebar-region-area
slot[name=sidebar.workspaces][wide=false]
div.seekdeep-sidebar-foot-area
div.seekdeep-sidebar-footer-actions
slot[name=sidebar.footer.action][wide=false]
div.seekdeep-sidebar-settings-area
slot[name=sidebar.settings][wide=false]"
    );
}
