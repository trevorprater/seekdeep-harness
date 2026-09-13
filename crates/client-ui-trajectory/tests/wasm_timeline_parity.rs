//! Live Rust/WASM Timeline rendering, hook state, and DOM-event adapter parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Promise, Reflect};
use seekdeep_client_ui_trajectory::{
    configure_client_ui_trajectory_modules, trajectory_timeline_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
function hooks() {
  const slots = []
  let cursor = 0
  const React = {
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    useRef(initial) {
      const index = cursor++
      if (!(index in slots)) slots[index] = { current: initial }
      return slots[index]
    },
    useState(initial) {
      const index = cursor++
      if (!(index in slots)) slots[index] = initial
      return [slots[index], value => { slots[index] = typeof value === 'function' ? value(slots[index]) : value }]
    },
    useEffect(effect) { cursor++; effect() },
  }
  return { React, reset() { cursor = 0 } }
}

export function makeTimelineBench(kind = 'full') {
  const react = hooks()
  const calls = []
  let resolveLoad
  const loadPromise = new Promise(resolve => { resolveLoad = resolve })
  const cells = Array.from({ length: kind === 'full' ? 10 : 0 }, (_, index) => ({
    index,
    kind: index === 2 ? 'tool' : 'message',
    text: `record ${index}`,
    timeSeconds: index === 0 ? 2 : 1,
    startedAt: 1000 + index * 1000,
    isError: index === 2 ? true : undefined,
    assistantMetrics: index === 0 ? {
      timingRecorded: true,
      stepStartTime: 1000,
      firstTokenTime: 1500,
      completedTime: 3000,
      usageProvided: false,
      outputTokens: null,
    } : undefined,
  }))
  const props = {
    turns: kind === 'full' ? [{ turn: 1, groups: [{ title: 'Step 1', cells }] }] : [],
    mode: 'sequence',
    range: null,
    hasEarlierRecords: kind === 'empty',
    onLoadEarlier() { calls.push(['load']); return loadPromise },
    selectedIndex: null,
    searchMatchIndexes: kind === 'full' ? new Set([1]) : null,
    onRangeChange(range) { calls.push(['range', range]) },
    onRecordSelect(index) { calls.push(['select', index]) },
    onRecordFocus(index) { calls.push(['focus', index]) },
  }
  return {
    react,
    React: react.React,
    primitives: { Tooltip: 'Tooltip' },
    props,
    calls,
    resolveLoad,
  }
}

export function timelineRender(bench, component) {
  bench.react.reset()
  return component(bench.props)
}

export function timelineText(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(timelineText).join('')
  return (node.children ?? []).map(timelineText).join('')
}

export function timelineFind(node, property, value) {
  if (node === null || node === undefined || node === false) return undefined
  if (typeof node === 'string' || typeof node === 'number') return undefined
  if (Array.isArray(node)) {
    for (const child of node) {
      const found = timelineFind(child, property, value)
      if (found !== undefined) return found
    }
    return undefined
  }
  if (node.props?.[property] === value) return node
  for (const child of node.children ?? []) {
    const found = timelineFind(child, property, value)
    if (found !== undefined) return found
  }
  return undefined
}

export function timelineFindAll(node, property, value) {
  const output = []
  const visit = current => {
    if (current === null || current === undefined || current === false) return
    if (typeof current === 'string' || typeof current === 'number') return
    if (Array.isArray(current)) { current.forEach(visit); return }
    if (current.props?.[property] === value) output.push(current)
    ;(current.children ?? []).forEach(visit)
  }
  visit(node)
  return output
}

export function timelineFindKind(node, kind) {
  if (node === null || node === undefined || node === false) return undefined
  if (typeof node === 'string' || typeof node === 'number') return undefined
  if (Array.isArray(node)) {
    for (const child of node) {
      const found = timelineFindKind(child, kind)
      if (found !== undefined) return found
    }
    return undefined
  }
  if (node.kind === kind) return node
  for (const child of node.children ?? []) {
    const found = timelineFindKind(child, kind)
    if (found !== undefined) return found
  }
  return undefined
}

export function timelineEvent(currentTarget, target, values = {}) {
  const stats = { prevented: 0, stopped: 0, captured: [] }
  const targetNode = target ?? currentTarget
  targetNode.closest = selector => selector === '[data-timeline-record-index]'
    && targetNode.props?.['data-timeline-record-index'] !== undefined ? {
      dataset: { timelineRecordIndex: String(targetNode.props['data-timeline-record-index']) },
    } : null
  currentTarget.getBoundingClientRect = () => ({ left: 0, width: 100 })
  currentTarget.setPointerCapture = id => stats.captured.push(id)
  return {
    currentTarget,
    target: targetNode,
    clientX: values.clientX ?? 50,
    deltaY: values.deltaY ?? 0,
    pointerId: values.pointerId ?? 1,
    button: values.button ?? 0,
    key: values.key ?? '',
    preventDefault() { stats.prevented++ },
    stopPropagation() { stats.stopped++ },
    stats,
  }
}

export function timelineInvoke(node, property, event) { return node.props[property](event) }
export function timelineProp(node, key) { return node?.props?.[key] }
export function timelineStyle(node, key) { return node?.props?.style?.[key] }
export function timelineCalls(bench) { return bench.calls }
export function timelineSetProp(bench, key, value) { bench.props[key] = value }
export function timelineResolveLoad(bench, value) { bench.resolveLoad(value) }
export function timelineTick() { return Promise.resolve().then(() => Promise.resolve()) }
"#)]
extern "C" {
    fn makeTimelineBench(kind: &str) -> JsValue;
    fn timelineRender(bench: &JsValue, component: &Function) -> JsValue;
    fn timelineText(node: &JsValue) -> String;
    fn timelineFind(node: &JsValue, property: &str, value: &JsValue) -> JsValue;
    fn timelineFindAll(node: &JsValue, property: &str, value: &JsValue) -> Array;
    fn timelineFindKind(node: &JsValue, kind: &str) -> JsValue;
    fn timelineEvent(current: &JsValue, target: &JsValue, values: &JsValue) -> JsValue;
    fn timelineInvoke(node: &JsValue, property: &str, event: &JsValue) -> JsValue;
    fn timelineProp(node: &JsValue, key: &str) -> JsValue;
    fn timelineStyle(node: &JsValue, key: &str) -> JsValue;
    fn timelineCalls(bench: &JsValue) -> Array;
    fn timelineSetProp(bench: &JsValue, key: &str, value: &JsValue);
    fn timelineResolveLoad(bench: &JsValue, value: bool);
    fn timelineTick() -> Promise;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn values(entries: &[(&str, JsValue)]) -> JsValue {
    let value = js_sys::Object::new();
    for (key, entry) in entries {
        Reflect::set(&value, &JsValue::from_str(key), entry).unwrap();
    }
    value.into()
}

fn component(bench: &JsValue) -> Function {
    configure_client_ui_trajectory_modules(property(bench, "React"), property(bench, "primitives"))
        .unwrap();
    trajectory_timeline_component().unwrap().dyn_into().unwrap()
}

#[wasm_bindgen_test(async)]
async fn empty_history_boundary_owns_loading_promise_and_hover_isolation() {
    let bench = makeTimelineBench("empty");
    let component = component(&bench);
    let first = timelineRender(&bench, &component);
    assert!(timelineText(&first).contains("InputModelToolsNo timing data"));
    let boundary = timelineFind(
        &first,
        "aria-label",
        &JsValue::from_str("Load earlier history"),
    );
    assert!(!boundary.is_undefined());
    let click = timelineInvoke(&boundary, "onClick", &JsValue::UNDEFINED);
    let _ = Promise::resolve(&click);
    let loading = timelineRender(&bench, &component);
    assert!(
        !timelineFind(
            &loading,
            "aria-label",
            &JsValue::from_str("Loading earlier history")
        )
        .is_undefined()
    );
    assert_eq!(timelineCalls(&bench).length(), 1);
    timelineResolveLoad(&bench, true);
    JsFuture::from(timelineTick()).await.unwrap();
    let settled = timelineRender(&bench, &component);
    assert!(
        !timelineFind(
            &settled,
            "aria-label",
            &JsValue::from_str("Load earlier history")
        )
        .is_undefined()
    );
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)] // One mounted controller retains gesture state across rerenders.
fn spans_tooltips_search_error_zoom_pointer_and_escape_paths_are_live() {
    let bench = makeTimelineBench("full");
    let component = component(&bench);
    let first = timelineRender(&bench, &component);
    let spans = timelineFindAll(
        &first,
        "className",
        &JsValue::from_str("seekdeep-trajectory-timeline-span"),
    );
    assert_eq!(spans.length(), 10);
    let assistant = spans.get(0);
    assert_eq!(
        timelineStyle(&assistant, "--trajectory-span-width")
            .as_string()
            .as_deref(),
        Some("10%")
    );
    assert_eq!(
        timelineStyle(&assistant, "--trajectory-span-gap")
            .as_string()
            .as_deref(),
        Some("min(0.8%, 1px)")
    );
    assert_eq!(
        timelineProp(&assistant, "data-assistant-timing")
            .as_string()
            .as_deref(),
        Some("true")
    );
    let tooltip = timelineFindKind(&first, "Tooltip");
    let tooltip_label = timelineProp(&tooltip, "label").as_string().unwrap();
    assert!(tooltip_label.contains("Total 2,000 ms"));
    assert!(tooltip_label.contains("TTFT 500 ms · Decoding 1,500 ms"));
    assert_eq!(timelineProp(&tooltip, "delayMs").as_f64(), Some(500.0));
    let tool = spans.get(2);
    assert_eq!(timelineProp(&tool, "data-error").as_bool(), Some(true));
    assert_eq!(
        timelineProp(&assistant, "data-search-match")
            .as_string()
            .as_deref(),
        Some("false")
    );
    let root = timelineFind(
        &first,
        "aria-label",
        &JsValue::from_str("Trajectory timeline"),
    );
    let wheel = timelineEvent(
        &root,
        &root,
        &values(&[
            ("clientX", JsValue::from_f64(50.0)),
            ("deltaY", JsValue::from_f64(-1_000.0)),
        ]),
    );
    timelineInvoke(&root, "onWheel", &wheel);
    assert!(property(&wheel, "stats").is_object());
    let zoomed = timelineRender(&bench, &component);
    let domain = timelineFind(&zoomed, "data-timeline-domain", &JsValue::from_str(""));
    assert_eq!(
        timelineStyle(&domain, "--trajectory-domain-width")
            .as_string()
            .as_deref(),
        Some("250%")
    );

    let zoomed_track = timelineFind(
        &zoomed,
        "aria-label",
        &JsValue::from_str("Timeline overview; drag horizontally to focus events"),
    );
    let zoomed_span = timelineFindAll(
        &zoomed,
        "className",
        &JsValue::from_str("seekdeep-trajectory-timeline-span"),
    )
    .get(2);
    let down = timelineEvent(
        &zoomed_track,
        &zoomed_span,
        &values(&[("clientX", JsValue::from_f64(50.0))]),
    );
    timelineInvoke(&zoomed_track, "onPointerDown", &down);
    let up = timelineEvent(
        &zoomed_track,
        &zoomed_span,
        &values(&[("clientX", JsValue::from_f64(50.0))]),
    );
    timelineInvoke(&zoomed_track, "onPointerUp", &up);
    let calls = timelineCalls(&bench);
    assert!(calls.iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("select")
    }));

    timelineSetProp(
        &bench,
        "range",
        &values(&[
            ("start", JsValue::from_f64(2.0)),
            ("end", JsValue::from_f64(4.0)),
        ]),
    );
    let ranged = timelineRender(&bench, &component);
    let ranged_track = timelineFind(
        &ranged,
        "aria-label",
        &JsValue::from_str("Timeline overview; drag horizontally to focus events"),
    );
    let escape = timelineEvent(
        &ranged_track,
        &ranged_track,
        &values(&[("key", JsValue::from_str("Escape"))]),
    );
    timelineInvoke(&ranged_track, "onKeyDown", &escape);
    assert!(timelineCalls(&bench).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("range") && call.get(1).is_null()
    }));
}
