//! Live assembled Rust/WASM `TrajectoryView` composition parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Reflect};
use seekdeep_client_ui_trajectory::{
    configure_client_ui_trajectory_modules, trajectory_view_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
function reactHarness() {
  const fibers = new Map()
  const pending = []
  const panes = new Map()
  let fiber = 'root'
  let cursor = 0
  const slots = () => {
    if (!fibers.has(fiber)) fibers.set(fiber, [])
    return fibers.get(fiber)
  }
  const sameDeps = (left, right) => left !== undefined
    && left.length === right.length
    && left.every((value, index) => Object.is(value, right[index]))
  const enter = (next, callback) => {
    const previousFiber = fiber
    const previousCursor = cursor
    fiber = next
    cursor = 0
    try { return callback() } finally {
      fiber = previousFiber
      cursor = previousCursor
    }
  }
  const effect = (callback, deps = []) => {
    const state = slots()
    const index = cursor++
    if (sameDeps(state[index], deps)) return
    state[index] = deps.slice()
    pending.push(callback)
  }
  const React = {
    createElement(kind, props, ...children) {
      if (typeof kind === 'function') {
        return enter(kind, () => kind({ ...(props ?? {}), children }))
      }
      let node = { kind, props: props ?? {}, children }
      node.getBoundingClientRect = () => ({ left: 0, width: 100, top: 0, bottom: 30 })
      node.setPointerCapture = () => {}
      node.releasePointerCapture = () => {}
      node.scrollIntoView = () => {}
      node.closest = selector => selector === '[data-timeline-record-index]'
        && node.props?.['data-timeline-record-index'] !== undefined
        ? { dataset: { timelineRecordIndex: String(node.props['data-timeline-record-index']) } }
        : null
      children.flat(Infinity).forEach(child => {
        if (child !== null && typeof child === 'object') child.parentElement = node
      })
      if (props?.['data-trajectory-scroll'] !== undefined) {
        if (!panes.has(fiber)) {
          node.scrollTop = 0
          node.scrollHeight = 300
          node.clientHeight = 100
          panes.set(fiber, node)
        } else {
          const pane = panes.get(fiber)
          pane.kind = kind
          pane.props = props ?? {}
          pane.children = children
          node = pane
        }
      }
      if (props?.ref !== undefined && props.ref !== null) props.ref.current = node
      return node
    },
    useRef(initial) {
      const state = slots()
      const index = cursor++
      if (!(index in state)) state[index] = { current: initial }
      return state[index]
    },
    useState(initial) {
      const state = slots()
      const index = cursor++
      if (!(index in state)) state[index] = typeof initial === 'function' ? initial() : initial
      return [state[index], value => {
        state[index] = typeof value === 'function' ? value(state[index]) : value
      }]
    },
    useEffect: effect,
    useLayoutEffect: effect,
  }
  return {
    React,
    render(component, props) {
      const tree = enter(component, () => component(props))
      const effects = pending.splice(0)
      effects.forEach(callback => callback())
      return tree
    },
  }
}

function inspection(kind) {
  if (kind === 'empty') {
    return {
      eventNodes: [], eventLocations: new Map(), requests: [], callSchemas: new Map(),
      partial: null, runningCalls: [],
    }
  }
  return {
    eventNodes: [
      { kind: 'user', seq: 1, time: 1000, content: [{ type: 'text', text: 'hello' }], source: null },
      {
        kind: 'assistant', seq: 2, time: 6000, turn: 1, step: 1,
        blocks: [
          { kind: 'text', text: 'I will run bash' },
          { kind: 'tool-call', callId: 'c1', name: 'bash', argsRaw: '{"command":"ls"}' },
        ],
        usage: { inputTokens: 10, outputTokens: 20 },
      },
      {
        kind: 'tool-result', seq: 3, time: 7500, callId: 'c1',
        call: { name: 'bash', argsRaw: '{"command":"ls"}' }, callTime: 6200,
        content: [{ type: 'text', text: 'a.txt' }], isError: false,
        callView: null, resultView: null, subCalls: [],
      },
    ],
    eventLocations: new Map(),
    requests: [{
      purpose: 'assistant', startSeq: 2, turn: 1, step: 1,
      status: 'complete', startedAt: 1000, completedAt: 6000, resultSeq: 2,
      usage: { inputTokens: 10, outputTokens: 20 },
    }],
    callSchemas: new Map(),
    partial: null,
    runningCalls: [],
  }
}

export function makeViewBench(kind = 'full') {
  const react = reactHarness()
  const calls = []
  const session = {
    views: new Map([['trajectory', inspection(kind)]]),
    openState: 'open',
    loadingOlder: false,
    hasMore: false,
  }
  const bench = {
    react,
    React: react.React,
    primitives: {
      Tooltip: 'Tooltip', MarkdownText: 'MarkdownText', JsonTree: 'JsonTree',
      IconSearchOutline16: 'IconSearchOutline16',
    },
    duration: false,
    session,
    calls,
  }
  bench.props = {
    useSession(selector) { return selector(bench.session) },
    useDuration(selector) { return selector(bench.duration) },
    loadOlder() { calls.push(['load']); return Promise.resolve(true) },
    setActualDuration(value) { bench.duration = value; calls.push(['duration', value]) },
    inspect: null,
    onInspectDone() { calls.push(['inspect-done']) },
    t(key) {
      const labels = {
        'toolbar.aria': 'Trajectory controls',
        'toolbar.useActualDuration': 'Use actual duration',
        'toolbar.useEqualWidth': 'Use equal width',
        'toolbar.duration': 'Duration',
        'toolbar.actualTime': 'Actual time',
        'toolbar.expandTurns': 'Expand turns',
        'toolbar.collapseTurns': 'Collapse turns',
        'toolbar.turns': 'Turns',
        'toolbar.expandCalls': 'Expand calls',
        'toolbar.collapseCalls': 'Collapse calls',
        'toolbar.calls': 'Calls',
        'toolbar.search': 'Search trajectory',
        'toolbar.searchPlaceholder': 'Search events',
      }
      return labels[key] ?? key
    },
  }
  return bench
}

export function viewRender(bench, component) { return bench.react.render(component, bench.props) }
export function viewText(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(viewText).join('')
  if (node.kind === 'MarkdownText') return node.props?.text ?? ''
  return (node.children ?? []).map(viewText).join('')
}
export function viewFind(node, property, value) {
  if (node === null || node === undefined || node === false) return undefined
  if (typeof node === 'string' || typeof node === 'number') return undefined
  if (Array.isArray(node)) {
    for (const child of node) {
      const found = viewFind(child, property, value)
      if (found !== undefined) return found
    }
    return undefined
  }
  if (node.props?.[property] === value) return node
  for (const child of node.children ?? []) {
    const found = viewFind(child, property, value)
    if (found !== undefined) return found
  }
  return undefined
}
export function viewRowsContaining(node, value) {
  const rows = []
  const visit = current => {
    if (current === null || current === undefined || current === false) return
    if (typeof current === 'string' || typeof current === 'number') return
    if (Array.isArray(current)) { current.forEach(visit); return }
    if (current.props?.role === 'row' && viewText(current).includes(value)) rows.push(current)
    ;(current.children ?? []).forEach(visit)
  }
  visit(node)
  return rows
}
export function viewInvoke(node, property, event) {
  return event === undefined ? node.props[property]() : node.props[property](event)
}
export function viewEvent(currentTarget, target = currentTarget, values = {}) {
  return {
    currentTarget, target,
    clientX: values.clientX ?? 50,
    pointerId: values.pointerId ?? 1,
    button: values.button ?? 0,
    key: values.key ?? '',
    preventDefault() {}, stopPropagation() {},
  }
}
export function viewProp(node, key) { return node?.props?.[key] }
export function viewCalls(bench) { return bench.calls }
export function viewSetInspect(bench, callId) { bench.props.inspect = callId === null ? null : { callId } }
"#)]
extern "C" {
    fn makeViewBench(kind: &str) -> JsValue;
    fn viewRender(bench: &JsValue, component: &Function) -> JsValue;
    fn viewText(node: &JsValue) -> String;
    fn viewFind(node: &JsValue, property: &str, value: &JsValue) -> JsValue;
    fn viewRowsContaining(node: &JsValue, value: &str) -> Array;
    fn viewInvoke(node: &JsValue, property: &str, event: &JsValue) -> JsValue;
    fn viewEvent(current: &JsValue, target: &JsValue, values: &JsValue) -> JsValue;
    fn viewProp(node: &JsValue, key: &str) -> JsValue;
    fn viewCalls(bench: &JsValue) -> Array;
    fn viewSetInspect(bench: &JsValue, call_id: &str);
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn component(bench: &JsValue) -> Function {
    configure_client_ui_trajectory_modules(property(bench, "React"), property(bench, "primitives"))
        .unwrap();
    trajectory_view_component().unwrap().dyn_into().unwrap()
}

fn settled_render(bench: &JsValue, component: &Function) -> JsValue {
    let _ = viewRender(bench, component);
    let _ = viewRender(bench, component);
    viewRender(bench, component)
}

#[wasm_bindgen_test]
fn empty_and_populated_views_mount_real_toolbar_timeline_and_table() {
    let empty = makeViewBench("empty");
    let empty_component = component(&empty);
    let empty_tree = settled_render(&empty, &empty_component);
    assert_eq!(
        viewProp(&empty_tree, "data-conversation-composer-overlay")
            .as_string()
            .as_deref(),
        Some("")
    );
    assert!(!viewFind(&empty_tree, "role", &JsValue::from_str("toolbar")).is_undefined());
    assert!(viewText(&empty_tree).contains("No timing data"));
    let empty_table = viewFind(&empty_tree, "role", &JsValue::from_str("table"));
    assert_eq!(viewProp(&empty_table, "aria-rowcount").as_f64(), Some(0.0));

    let full = makeViewBench("full");
    let full_component = component(&full);
    let full_tree = settled_render(&full, &full_component);
    assert_eq!(
        viewRowsContaining(&full_tree, "I will run bash").length(),
        1
    );
    assert!(!viewFind(&full_tree, "data-kind", &JsValue::from_str("tool")).is_undefined());
    assert!(
        !viewFind(
            &full_tree,
            "data-timeline-record-index",
            &JsValue::from_f64(2.0)
        )
        .is_undefined()
    );
}

#[wasm_bindgen_test]
fn timeline_selection_search_duration_and_inspect_flow_through_assembled_children() {
    let bench = makeViewBench("full");
    let component = component(&bench);
    let first = settled_render(&bench, &component);

    let duration = viewFind(
        &first,
        "aria-label",
        &JsValue::from_str("Use actual duration"),
    );
    viewInvoke(&duration, "onClick", &JsValue::UNDEFINED);
    let duration_tree = viewRender(&bench, &component);
    assert!(viewCalls(&bench).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("duration")
            && call.get(1).as_bool() == Some(true)
    }));
    let timeline_domain = viewFind(
        &duration_tree,
        "data-timeline-domain",
        &JsValue::from_str(""),
    );
    assert!(!timeline_domain.is_undefined());

    let search = viewFind(
        &duration_tree,
        "aria-label",
        &JsValue::from_str("Search trajectory"),
    );
    let search_event = js_sys::Object::new();
    let current = js_sys::Object::new();
    Reflect::set(
        &current,
        &JsValue::from_str("value"),
        &JsValue::from_str("a.txt"),
    )
    .unwrap();
    Reflect::set(&search_event, &JsValue::from_str("currentTarget"), &current).unwrap();
    viewInvoke(&search, "onChange", &search_event.into());
    let searched = viewRender(&bench, &component);
    assert_eq!(viewRowsContaining(&searched, "a.txt").length(), 1);
    assert_eq!(viewRowsContaining(&searched, "I will run bash").length(), 0);

    viewSetInspect(&bench, "c1");
    let _ = viewRender(&bench, &component);
    let inspected = viewRender(&bench, &component);
    let tool = viewRowsContaining(&inspected, "bash").get(0);
    assert_eq!(viewProp(&tool, "aria-selected").as_bool(), Some(true));
    assert!(viewCalls(&bench).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("inspect-done")
    }));
}

#[wasm_bindgen_test]
fn clicking_a_timeline_span_selects_the_same_table_record() {
    let bench = makeViewBench("full");
    let component = component(&bench);
    let first = settled_render(&bench, &component);
    let track = viewFind(
        &first,
        "aria-label",
        &JsValue::from_str("Timeline overview; drag horizontally to focus events"),
    );
    let span = viewFind(
        &first,
        "data-timeline-record-index",
        &JsValue::from_f64(2.0),
    );
    let down = viewEvent(&track, &span, &JsValue::UNDEFINED);
    viewInvoke(&track, "onPointerDown", &down);
    let up = viewEvent(&track, &span, &JsValue::UNDEFINED);
    viewInvoke(&track, "onPointerUp", &up);
    let _ = viewRender(&bench, &component);
    let selected = viewRender(&bench, &component);
    let tool = viewRowsContaining(&selected, "bash").get(0);
    assert_eq!(viewProp(&tool, "aria-selected").as_bool(), Some(true));
    assert!(!viewFind(&selected, "role", &JsValue::from_str("complementary")).is_undefined());
}
