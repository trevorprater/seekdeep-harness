//! Live WASM coverage for context occupancy and breakdown presentation.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_conversation::{
    configure_client_ui_conversation_context_meter, context_meter_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let projections = {}
let locale = 'zh'
let listeners = new Map()
class BenchNode { constructor(inside = false) { this.inside = inside } contains(target) { return target?.inside === true } }
let rootNode = new BenchNode(true)
let insideNode = new BenchNode(true)
let outsideNode = new BenchNode(false)
function sameDeps(left, right) { return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index])) }
export function installMeterBench() {
  hooks = []; cursor = 0; projections = {}; locale = 'zh'; listeners = new Map()
  rootNode = new BenchNode(true); insideNode = new BenchNode(true); outsideNode = new BenchNode(false)
  globalThis.Node = BenchNode
  globalThis.document = {
    head: { appendChild() {} }, createElement() { return { setAttribute() {} } }, querySelector() { return null },
    addEventListener(name, listener) { if (!listeners.has(name)) listeners.set(name, new Set()); listeners.get(name).add(listener) },
    removeEventListener(name, listener) { listeners.get(name)?.delete(listener) },
  }
  const React = {
    createElement(kind, props, ...children) {
      if (kind === 'span' && props?.ref) props.ref.current = rootNode
      return { kind, props: props ?? {}, children }
    },
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { type: 'state', value: initial }
      const set = update => { hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update }
      return [hooks[index].value, set]
    },
    useRef(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { type: 'ref', value: { current: initial } }
      return hooks[index].value
    },
    useEffect(effect, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) {
        hooks[index]?.cleanup?.()
        hooks[index] = { type: 'effect', deps: [...deps], cleanup: effect() }
      }
    },
  }
  return { React, uiPrimitives: { Tooltip: 'Tooltip' } }
}
export function meterObject(entries) { return Object.fromEntries(entries) }
export function meterSetProjections(value) { projections = value }
export function meterSetLocale(value) { locale = value }
export function makeMeterProjection() { return key => projections[key] }
export function makeMeterTranslate() {
  return (key, vars) => {
    if (key === 'context.aria') return locale === 'zh' ? `上下文已用 ${vars.percent}` : `${vars.percent} of context used`
    const zh = { 'context.used': '上下文使用情况', 'context.system': '系统提示词', 'context.tools': '工具', 'context.messages': '对话消息' }
    const en = { 'context.used': 'Context usage', 'context.system': 'System prompt', 'context.tools': 'Tools', 'context.messages': 'Messages' }
    return (locale === 'zh' ? zh : en)[key] ?? key
  }
}
export function meterRender(component, props) { cursor = 0; return component(props) }
export function meterUnmount() { for (const hook of [...hooks].reverse()) hook?.cleanup?.(); hooks = [] }
export function meterDispatchPointer(inside) { for (const listener of listeners.get('pointerdown') ?? []) listener({ target: inside ? insideNode : outsideNode }) }
export function meterDispatchKey(key) { for (const listener of listeners.get('keydown') ?? []) listener({ key }) }
export function meterListenerCount() { return [...listeners.values()].reduce((total, set) => total + set.size, 0) }
export function meterText(value) {
  if (value === null || value === undefined || typeof value === 'boolean') return ''
  if (typeof value === 'string' || typeof value === 'number') return String(value)
  if (Array.isArray(value)) return value.map(meterText).join('')
  return meterText(value.children)
}
export function meterFindButton(value, label) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.kind === 'button' && value.props?.['aria-label'] === label) return value
  for (const child of value.children ?? []) { const found = meterFindButton(child, label); if (found) return found }
  return undefined
}
export function meterFindRole(value, role) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.props?.role === role) return value
  for (const child of value.children ?? []) { const found = meterFindRole(child, role); if (found) return found }
  return undefined
}
export function meterFindClass(value, className) {
  if (value === null || value === undefined || typeof value !== 'object') return []
  const own = String(value.props?.className ?? '').split(/\s+/).includes(className) ? [value] : []
  return own.concat(...(value.children ?? []).map(child => meterFindClass(child, className)))
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installMeterBench)]
    fn install_meter_bench() -> JsValue;
    #[wasm_bindgen(js_name = meterObject)]
    fn meter_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = meterSetProjections)]
    fn meter_set_projections(value: &JsValue);
    #[wasm_bindgen(js_name = meterSetLocale)]
    fn meter_set_locale(value: &str);
    #[wasm_bindgen(js_name = makeMeterProjection)]
    fn make_meter_projection() -> Function;
    #[wasm_bindgen(js_name = makeMeterTranslate)]
    fn make_meter_translate() -> Function;
    #[wasm_bindgen(js_name = meterRender)]
    fn meter_render(component: &JsValue, props: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = meterUnmount)]
    fn meter_unmount();
    #[wasm_bindgen(js_name = meterDispatchPointer)]
    fn meter_dispatch_pointer(inside: bool);
    #[wasm_bindgen(js_name = meterDispatchKey)]
    fn meter_dispatch_key(key: &str);
    #[wasm_bindgen(js_name = meterListenerCount)]
    fn meter_listener_count() -> u32;
    #[wasm_bindgen(js_name = meterText)]
    fn meter_text(value: &JsValue) -> String;
    #[wasm_bindgen(js_name = meterFindButton)]
    fn meter_find_button(value: &JsValue, label: &str) -> JsValue;
    #[wasm_bindgen(js_name = meterFindRole)]
    fn meter_find_role(value: &JsValue, role: &str) -> JsValue;
    #[wasm_bindgen(js_name = meterFindClass)]
    fn meter_find_class(value: &JsValue, class_name: &str) -> Array;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn child(value: &JsValue, index: u32) -> JsValue {
    property(value, "children")
        .unchecked_into::<Array>()
        .get(index)
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    meter_object(&array).unchecked_into()
}

fn pressure(used: Option<f64>, projected: Option<f64>, window: Option<f64>) -> Object {
    let mut entries = Vec::new();
    if let Some(used) = used {
        entries.push(("pressureTokens", JsValue::from_f64(used)));
    }
    if let Some(projected) = projected {
        entries.push(("projectedTokens", JsValue::from_f64(projected)));
    }
    if let Some(window) = window {
        entries.push(("contextWindow", JsValue::from_f64(window)));
    }
    object(&entries)
}

fn breakdown() -> Object {
    object(&[
        ("systemTokens", JsValue::from_f64(120.0)),
        ("toolsTokens", JsValue::from_f64(21_500.0)),
        ("messageTokens", JsValue::from_f64(477_000.0)),
    ])
}

fn setup() -> (JsValue, Object) {
    let bench = install_meter_bench();
    configure_client_ui_conversation_context_meter(
        property(&bench, "React"),
        property(&bench, "uiPrimitives"),
    )
    .unwrap();
    let props = object(&[
        ("useProjection", make_meter_projection().into()),
        ("t", make_meter_translate().into()),
    ]);
    (context_meter_component().unwrap(), props)
}

fn values(pressure: Option<Object>, breakdown: Option<Object>) -> Object {
    let mut entries = Vec::new();
    if let Some(pressure) = pressure {
        entries.push(("contextPressure", pressure.into()));
    }
    if let Some(breakdown) = breakdown {
        entries.push(("contextBreakdown", breakdown.into()));
    }
    object(&entries)
}

#[wasm_bindgen_test]
fn pressure_and_capacity_gate_the_entire_meter() {
    let (component, props) = setup();
    for projected in [
        values(None, None),
        values(Some(pressure(Some(32_000.0), None, None)), None),
        values(Some(pressure(None, None, Some(128_000.0))), None),
    ] {
        meter_set_projections(projected.as_ref());
        assert!(meter_render(&component, props.as_ref()).is_null());
    }
    meter_unmount();
}

#[wasm_bindgen_test]
fn ring_and_open_panel_pin_exact_occupancy_breakdown_and_tooltip_contract() {
    let (component, props) = setup();
    meter_set_projections(
        values(
            Some(pressure(Some(32_000.0), None, Some(128_000.0))),
            Some(breakdown()),
        )
        .as_ref(),
    );
    let closed = meter_render(&component, props.as_ref());
    let button = meter_find_button(&closed, "上下文已用 25%");
    let tooltip = child(&closed, 0);
    assert_eq!(
        property(&tooltip, "kind").as_string().as_deref(),
        Some("Tooltip")
    );
    assert_eq!(
        property(&property(&tooltip, "props"), "side")
            .as_string()
            .as_deref(),
        Some("top")
    );
    assert_eq!(
        property(&property(&tooltip, "props"), "delayMs").as_f64(),
        Some(200.0)
    );
    assert_eq!(
        property(&property(&tooltip, "props"), "disabled").as_bool(),
        Some(false)
    );
    assert_eq!(
        property(&property(&button, "props"), "aria-expanded").as_bool(),
        Some(false)
    );
    let fill = meter_find_class(&closed, "seekdeep-conversation-contextMeter-fill").get(0);
    assert_eq!(
        property(&property(&fill, "props"), "strokeDasharray")
            .as_string()
            .as_deref(),
        Some("8.63937979737193 34.55751918948772")
    );
    property(&property(&button, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let open = meter_render(&component, props.as_ref());
    let panel = meter_find_role(&open, "dialog");
    let text = meter_text(&panel);
    assert!(text.starts_with("上下文已用25%"));
    assert!(text.contains("~32K / 128K"));
    assert!(text.contains("系统提示词~120"));
    assert!(text.contains("工具~21.5K"));
    assert!(text.contains("对话消息~477K"));
    assert_eq!(
        meter_find_class(&panel, "seekdeep-conversation-contextMeter-segment").length(),
        3
    );
    assert_eq!(
        property(&property(&child(&open, 0), "props"), "disabled").as_bool(),
        Some(true)
    );
    meter_unmount();
}

#[wasm_bindgen_test]
fn projected_tokens_locale_order_zero_width_and_missing_breakdown_match_source() {
    let (component, props) = setup();
    meter_set_projections(
        values(
            Some(pressure(Some(32_000.0), Some(3_000.0), Some(128_000.0))),
            None,
        )
        .as_ref(),
    );
    let closed = meter_render(&component, props.as_ref());
    let button = meter_find_button(&closed, "上下文已用 2%");
    property(&property(&button, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let open = meter_render(&component, props.as_ref());
    let panel = meter_find_role(&open, "dialog");
    assert!(meter_text(&panel).contains("~3K / 128K"));
    assert_eq!(
        meter_find_class(&panel, "seekdeep-conversation-contextMeter-segment").length(),
        1
    );
    assert!(!meter_text(&panel).contains("系统提示词"));

    meter_unmount();
    let (component, props) = setup();
    meter_set_locale("en");
    meter_set_projections(
        values(
            Some(pressure(Some(32_000.0), None, Some(128_000.0))),
            Some(breakdown()),
        )
        .as_ref(),
    );
    let closed = meter_render(&component, props.as_ref());
    let button = meter_find_button(&closed, "25% of context used");
    property(&property(&button, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let open = meter_render(&component, props.as_ref());
    assert!(meter_text(&meter_find_role(&open, "dialog")).starts_with("25%of context used"));

    meter_unmount();
    let (component, props) = setup();
    meter_set_projections(
        values(
            Some(pressure(Some(0.0), None, Some(128_000.0))),
            Some(breakdown()),
        )
        .as_ref(),
    );
    let closed = meter_render(&component, props.as_ref());
    let button = meter_find_button(&closed, "上下文已用 0%");
    property(&property(&button, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let panel = meter_find_role(&meter_render(&component, props.as_ref()), "dialog");
    assert_eq!(
        meter_find_class(&panel, "seekdeep-conversation-contextMeter-segment").length(),
        0
    );
    meter_unmount();
}

#[wasm_bindgen_test]
fn disappearing_capacity_closes_the_panel_before_it_returns() {
    let (component, props) = setup();
    meter_set_projections(
        values(
            Some(pressure(Some(32_000.0), None, Some(128_000.0))),
            Some(breakdown()),
        )
        .as_ref(),
    );
    let closed = meter_render(&component, props.as_ref());
    let button = meter_find_button(&closed, "上下文已用 25%");
    property(&property(&button, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert!(!meter_find_role(&meter_render(&component, props.as_ref()), "dialog").is_undefined());
    meter_set_projections(
        values(
            Some(pressure(Some(32_000.0), None, None)),
            Some(breakdown()),
        )
        .as_ref(),
    );
    assert!(meter_render(&component, props.as_ref()).is_null());
    meter_set_projections(
        values(
            Some(pressure(Some(32_000.0), None, Some(128_000.0))),
            Some(breakdown()),
        )
        .as_ref(),
    );
    let restored = meter_render(&component, props.as_ref());
    let button = meter_find_button(&restored, "上下文已用 25%");
    assert_eq!(
        property(&property(&button, "props"), "aria-expanded").as_bool(),
        Some(false)
    );
    meter_unmount();
}

#[wasm_bindgen_test]
fn inside_pointer_is_ignored_while_outside_pointer_escape_and_unmount_cleanup_close() {
    let (component, props) = setup();
    meter_set_projections(
        values(
            Some(pressure(Some(32_000.0), None, Some(128_000.0))),
            Some(breakdown()),
        )
        .as_ref(),
    );
    let closed = meter_render(&component, props.as_ref());
    let trigger = meter_find_button(&closed, "上下文已用 25%");
    property(&property(&trigger, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let _ = meter_render(&component, props.as_ref());
    assert_eq!(meter_listener_count(), 2);
    meter_dispatch_pointer(true);
    assert!(!meter_find_role(&meter_render(&component, props.as_ref()), "dialog").is_undefined());
    meter_dispatch_pointer(false);
    let closed = meter_render(&component, props.as_ref());
    assert!(meter_find_role(&closed, "dialog").is_undefined());

    let trigger = meter_find_button(&closed, "上下文已用 25%");
    property(&property(&trigger, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let _ = meter_render(&component, props.as_ref());
    meter_dispatch_key("Escape");
    assert!(meter_find_role(&meter_render(&component, props.as_ref()), "dialog").is_undefined());
    meter_unmount();
    assert_eq!(meter_listener_count(), 0);
}
