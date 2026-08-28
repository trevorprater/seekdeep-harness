//! Live Rust/WASM trajectory-table rendering and lifecycle parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Promise, Reflect};
use seekdeep_client_ui_trajectory::{
    configure_client_ui_trajectory_modules, trajectory_table_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
function hooks() {
  const slots = []
  let cursor = 0
  let pending = []
  let pane
  const sameDeps = (left, right) => left !== undefined
    && left.length === right.length
    && left.every((value, index) => Object.is(value, right[index]))
  const effect = (callback, deps = []) => {
    const index = cursor++
    if (sameDeps(slots[index], deps)) return
    slots[index] = deps.slice()
    pending.push(callback)
  }
  const React = {
    createElement(kind, props, ...children) {
      let node = { kind, props: props ?? {}, children }
      node.getBoundingClientRect = () => ({
        width: props?.role === 'complementary' ? 400 : props?.className?.includes?.('split') ? 1000 : 400,
      })
      node.setPointerCapture = () => {}
      node.releasePointerCapture = () => {}
      children.flat(Infinity).forEach(child => {
        if (child !== null && typeof child === 'object') child.parentElement = node
      })
      if (props?.['data-trajectory-scroll'] !== undefined) {
        if (pane === undefined) {
          pane = node
          pane.scrollTop = 0
          pane.scrollHeight = 200
          pane.clientHeight = 100
        } else {
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
      const index = cursor++
      if (!(index in slots)) slots[index] = { current: initial }
      return slots[index]
    },
    useState(initial) {
      const index = cursor++
      if (!(index in slots)) slots[index] = initial
      return [slots[index], value => {
        slots[index] = typeof value === 'function' ? value(slots[index]) : value
      }]
    },
    useEffect: effect,
    useLayoutEffect: effect,
  }
  return {
    React,
    render(component, props) {
      cursor = 0
      pending = []
      const tree = component(props)
      const effects = pending
      pending = []
      effects.forEach(callback => callback())
      return tree
    },
    pane() { return pane },
  }
}

function ordinaryTurns() {
  return [{
    turn: 2,
    groups: [{
      title: 'Step 1',
      description: '1.5s bash×2',
      cells: [
        {
          index: 1,
          kind: 'message',
          sourceSeq: 100,
          text: 'Checking files',
          outputDetail: 'Checking files',
          input: 10,
          output: 20,
          think: 5,
          timeSeconds: 1.5,
          assistantMetrics: {
            timingRecorded: true,
            stepStartTime: 1000,
            firstTokenTime: 1500,
            completedTime: 2500,
            usageProvided: true,
            outputTokens: 20,
          },
        },
        {
          index: 2,
          kind: 'tool',
          sourceSeq: 101,
          callId: 'call-1',
          text: 'bash · {"command":"pwd"}',
          inputDetail: '{"command":"pwd"}',
          timeSeconds: null,
        },
        {
          index: 3,
          kind: 'tool',
          sourceSeq: 102,
          text: 'bash · {"command":"false"}',
          inputDetail: '{"command":"false"}',
          outputDetail: 'ToolError: non_zero_exit',
          result: 'non_zero_exit',
          isError: true,
          timeSeconds: 0.2,
        },
      ],
    }],
  }]
}

function callbacks(calls) {
  return {
    onToggleTurn(turn) { calls.push(['toggle-turn', turn]) },
    onToggleAssistant(id) { calls.push(['toggle-assistant', id]) },
    onRecordSelect(index) { calls.push(['select', index]) },
    onSelectedIndexChange(index) { calls.push(['selected-index', index]) },
    onClearSelection() { calls.push(['clear']) },
    onInspectApplied() { calls.push(['inspect-applied']) },
  }
}

export function makeTableBench(kind = 'ordinary') {
  const react = hooks()
  const calls = []
  let resolveLoad
  const loadPromise = new Promise(resolve => { resolveLoad = resolve })
  const turns = kind === 'long' ? [{
    turn: 1,
    groups: [{
      title: 'Context',
      cells: Array.from({ length: 500 }, (_, index) => ({
        index: index + 1,
        kind: 'context',
        sourceSeq: index + 1,
        text: `Context ${index + 1}`,
        timeSeconds: 0,
      })),
    }],
  }] : ordinaryTurns()
  const props = {
    turns,
    streamingCells: [],
    timelineFocusIndexes: null,
    searchMatchIndexes: null,
    recordSelection: null,
    recordFocus: null,
    historyLoading: false,
    olderHistoryLoading: false,
    historyStartSeq: 1,
    hasOlderRecords: kind === 'history',
    onLoadOlder() { calls.push(['load']); return loadPromise },
    collapsedTurns: new Set(),
    collapsedAssistants: new Set(),
    inspectCallId: null,
    ...callbacks(calls),
  }
  return {
    react,
    React: react.React,
    primitives: { Tooltip: 'Tooltip', MarkdownText: 'MarkdownText', JsonTree: 'JsonTree' },
    props,
    calls,
    resolveLoad,
  }
}

export function tableRender(bench, component) {
  return bench.react.render(component, bench.props)
}

export function tablePane(bench) { return bench.react.pane() }

export function tableText(node) {
  if (node === null || node === undefined || node === false) return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(tableText).join('')
  return (node.children ?? []).map(tableText).join('')
}

export function tableFind(node, property, value) {
  if (node === null || node === undefined || node === false) return undefined
  if (typeof node === 'string' || typeof node === 'number') return undefined
  if (Array.isArray(node)) {
    for (const child of node) {
      const found = tableFind(child, property, value)
      if (found !== undefined) return found
    }
    return undefined
  }
  if (node.props?.[property] === value) return node
  for (const child of node.children ?? []) {
    const found = tableFind(child, property, value)
    if (found !== undefined) return found
  }
  return undefined
}

export function tableFindText(node, value) {
  if (node === null || node === undefined || node === false) return undefined
  if (typeof node === 'string' || typeof node === 'number') return undefined
  if (!Array.isArray(node) && tableText(node) === value) return node
  for (const child of Array.isArray(node) ? node : node.children ?? []) {
    const found = tableFindText(child, value)
    if (found !== undefined) return found
  }
  return undefined
}

export function tableFindAll(node, property, value) {
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

export function tableRowsContaining(node, value) {
  return tableFindAll(node, 'role', 'row').filter(row => tableText(row).includes(value))
}

export function tableInvoke(node, property, event) {
  return event === undefined ? node.props[property]() : node.props[property](event)
}

export function tableEvent(currentTarget, target = currentTarget, values = {}) {
  const stats = { prevented: 0, stopped: 0 }
  return {
    currentTarget,
    target,
    key: values.key ?? '',
    preventDefault() { stats.prevented++ },
    stopPropagation() { stats.stopped++ },
    stats,
  }
}

export function tableProp(node, key) { return node?.props?.[key] }
export function tableCalls(bench) { return bench.calls }
export function tableSetProp(bench, key, value) { bench.props[key] = value }
export function tableSetTurns(bench, turns) { bench.props.turns = turns }
export function tableOrdinaryTurns() { return ordinaryTurns() }
export function tableThinkingTurns() {
  return [{ turn: 1, groups: [{ title: 'Step 1', cells: [{
    index: 1,
    kind: 'message',
    sourceSeq: 1,
    text: 'private chain…',
    thinkingDetail: 'private chain private chain',
    outputDetail: 'answer',
    timeSeconds: 1,
  }] }] }]
}
export function tableJsonTurns() {
  return [{ turn: 1, groups: [{ title: 'Step 1', cells: [{
    index: 1,
    kind: 'tool',
    sourceSeq: 1,
    text: 'read · {"path":"result.json"}',
    outputDetail: '{"value":1,"nested":{"ok":true}}',
    outputBlocks: [{ type: 'text', content: '{"value":1,"nested":{"ok":true}}' }],
    timeSeconds: 0.1,
  }] }] }]
}
export function tableRoleTurns() {
  return [{ turn: 1, groups: [{ title: 'Context', cells: [
    { index: 1, kind: 'context', text: 'Workspace context', timeSeconds: 0 },
    { index: 2, kind: 'compacted', text: 'Compacted history', timeSeconds: 0 },
  ] }] }]
}
export function tableRequestRunTurns() {
  return [1, 2, 3].map((turn, index) => ({ turn, groups: [{ title: 'Step 1', cells: [{
    index: index + 1,
    kind: 'message',
    text: index === 2 ? 'Recovered response' : '',
    requestOnly: index < 2,
    isError: index < 2 ? true : undefined,
    timeSeconds: 0.1,
  }] }] }))
}
export function tableSetCollapsedTurn(bench, turn) { bench.props.collapsedTurns = new Set([turn]) }
export function tableResolveLoad(bench, advanced) { bench.resolveLoad(advanced) }
export function tableTick() { return Promise.resolve().then(() => Promise.resolve()) }
"#)]
extern "C" {
    fn makeTableBench(kind: &str) -> JsValue;
    fn tableRender(bench: &JsValue, component: &Function) -> JsValue;
    fn tablePane(bench: &JsValue) -> JsValue;
    fn tableText(node: &JsValue) -> String;
    fn tableFind(node: &JsValue, property: &str, value: &JsValue) -> JsValue;
    fn tableFindText(node: &JsValue, value: &str) -> JsValue;
    fn tableFindAll(node: &JsValue, property: &str, value: &JsValue) -> Array;
    fn tableRowsContaining(node: &JsValue, value: &str) -> Array;
    fn tableInvoke(node: &JsValue, property: &str, event: &JsValue) -> JsValue;
    fn tableEvent(current: &JsValue, target: &JsValue, values: &JsValue) -> JsValue;
    fn tableProp(node: &JsValue, key: &str) -> JsValue;
    fn tableCalls(bench: &JsValue) -> Array;
    fn tableSetProp(bench: &JsValue, key: &str, value: &JsValue);
    fn tableSetTurns(bench: &JsValue, turns: &JsValue);
    fn tableOrdinaryTurns() -> JsValue;
    fn tableThinkingTurns() -> JsValue;
    fn tableJsonTurns() -> JsValue;
    fn tableRoleTurns() -> JsValue;
    fn tableRequestRunTurns() -> JsValue;
    fn tableSetCollapsedTurn(bench: &JsValue, turn: u32);
    fn tableResolveLoad(bench: &JsValue, advanced: bool);
    fn tableTick() -> Promise;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let value = js_sys::Object::new();
    for (key, entry) in entries {
        Reflect::set(&value, &JsValue::from_str(key), entry).unwrap();
    }
    value.into()
}

fn component(bench: &JsValue) -> Function {
    configure_client_ui_trajectory_modules(property(bench, "React"), property(bench, "primitives"))
        .unwrap();
    trajectory_table_component().unwrap().dyn_into().unwrap()
}

fn settled_render(bench: &JsValue, component: &Function) -> JsValue {
    let _ = tableRender(bench, component);
    tableRender(bench, component)
}

#[wasm_bindgen_test]
fn selection_tokens_timing_and_prepend_stability_are_live() {
    let bench = makeTableBench("ordinary");
    let component = component(&bench);
    let first = settled_render(&bench, &component);
    let assistant = tableRowsContaining(&first, "Checking files").get(0);
    assert!(!assistant.is_undefined());
    assert_eq!(
        tableProp(&assistant, "aria-selected").as_bool(),
        Some(false)
    );
    tableInvoke(&assistant, "onClick", &JsValue::UNDEFINED);

    let selected = tableRender(&bench, &component);
    let selected_row = tableRowsContaining(&selected, "Checking files").get(0);
    assert_eq!(
        tableProp(&selected_row, "aria-selected").as_bool(),
        Some(true)
    );
    for expected in [
        "Tokens",
        "20 tok",
        "Reasoning",
        "5 tok",
        "Content",
        "15 tok",
    ] {
        assert!(
            !tableFindText(&selected, expected).is_undefined(),
            "missing {expected}"
        );
    }
    let request_timing = tableFind(
        &selected,
        "aria-label",
        &JsValue::from_str("Request Timing"),
    );
    tableInvoke(&request_timing, "onClick", &JsValue::UNDEFINED);
    let timing = tableRender(&bench, &component);
    for expected in ["500 ms", "1.00 s", "20.0 tok/s"] {
        assert!(
            !tableFindText(&timing, expected).is_undefined(),
            "missing {expected}"
        );
    }

    let old_turns = tableOrdinaryTurns();
    let older = object(&[
        ("turn", JsValue::from_f64(1.0)),
        (
            "groups",
            Array::of1(&object(&[
                ("title", JsValue::from_str("Step 1")),
                (
                    "cells",
                    Array::of1(&object(&[
                        ("index", JsValue::from_f64(1.0)),
                        ("kind", JsValue::from_str("message")),
                        ("sourceSeq", JsValue::from_f64(1.0)),
                        ("text", JsValue::from_str("older response")),
                        ("timeSeconds", JsValue::from_f64(1.0)),
                    ]))
                    .into(),
                ),
            ]))
            .into(),
        ),
    ]);
    let shifted = old_turns.clone();
    let tail = Array::from(&shifted).get(0);
    let groups = Array::from(&property(&tail, "groups"));
    let tail_cells = Array::from(&property(&groups.get(0), "cells"));
    for (offset, cell) in tail_cells.iter().enumerate() {
        Reflect::set(
            &cell,
            &JsValue::from_str("index"),
            &JsValue::from_f64(f64::from(u32::try_from(offset + 2).unwrap())),
        )
        .unwrap();
    }
    tableSetTurns(&bench, &Array::of2(&older, &tail).into());
    let shifted_render = tableRender(&bench, &component);
    let shifted_selected = tableRowsContaining(&shifted_render, "Checking files").get(0);
    assert_eq!(
        tableProp(&shifted_selected, "aria-selected").as_bool(),
        Some(false)
    );
    assert!(!tableFindText(&shifted_render, "Request #2").is_undefined());
}

#[wasm_bindgen_test(async)]
async fn older_history_promise_and_virtual_window_are_live() {
    let history = makeTableBench("history");
    let history_component = component(&history);
    let first = settled_render(&history, &history_component);
    let table = tableFind(&first, "role", &JsValue::from_str("table"));
    assert_eq!(tableProp(&table, "aria-rowcount").as_f64(), Some(4.0));
    let load = tableFind(
        &first,
        "aria-label",
        &JsValue::from_str("Load earlier history"),
    );
    let returned = tableInvoke(&load, "onClick", &JsValue::UNDEFINED);
    let _ = Promise::resolve(&returned);
    let loading = tableRender(&history, &history_component);
    assert!(
        !tableFind(
            &loading,
            "aria-label",
            &JsValue::from_str("Loading earlier history…")
        )
        .is_undefined()
    );
    assert_eq!(tableCalls(&history).length(), 2); // selected-index effect plus load
    tableResolveLoad(&history, true);
    JsFuture::from(tableTick()).await.unwrap();
    let settled = tableRender(&history, &history_component);
    assert!(
        !tableFind(
            &settled,
            "aria-label",
            &JsValue::from_str("Load earlier history")
        )
        .is_undefined()
    );

    let long = makeTableBench("long");
    let long_component = component(&long);
    let initial = settled_render(&long, &long_component);
    let mounted = tableFindAll(&initial, "data-kind", &JsValue::from_str("context"));
    assert!(mounted.length() > 0 && mounted.length() < 500);
    assert!(
        !tableFind(
            &initial,
            "data-virtual-spacer",
            &JsValue::from_str("bottom")
        )
        .is_undefined()
    );
    let pane = tablePane(&long);
    Reflect::set(
        &pane,
        &JsValue::from_str("scrollTop"),
        &JsValue::from_f64(9_000.0),
    )
    .unwrap();
    Reflect::set(
        &pane,
        &JsValue::from_str("scrollHeight"),
        &JsValue::from_f64(15_000.0),
    )
    .unwrap();
    let event = tableEvent(&pane, &pane, &JsValue::UNDEFINED);
    tableInvoke(&pane, "onScroll", &event);
    let scrolled = tableRender(&long, &long_component);
    let first_context = tableFindAll(&scrolled, "data-kind", &JsValue::from_str("context")).get(0);
    assert!(
        tableProp(&first_context, "data-virtual-position")
            .as_f64()
            .unwrap()
            > 0.0
    );
    assert!(!tableFind(&scrolled, "data-virtual-spacer", &JsValue::from_str("top")).is_undefined());
}

#[wasm_bindgen_test]
fn request_selection_and_inspect_acknowledgement_are_live() {
    let bench = makeTableBench("ordinary");
    let component = component(&bench);
    let first = settled_render(&bench, &component);
    let request = tableFind(&first, "aria-label", &JsValue::from_str("Request #1"));
    tableInvoke(
        &request,
        "onClick",
        &tableEvent(&request, &request, &JsValue::UNDEFINED),
    );
    let selected = tableRender(&bench, &component);
    let active = tableFind(&selected, "aria-label", &JsValue::from_str("Request #1"));
    assert_eq!(tableProp(&active, "aria-pressed").as_bool(), Some(true));
    assert!(!tableFindText(&selected, "Request #1").is_undefined());

    tableSetProp(&bench, "inspectCallId", &JsValue::from_str("call-1"));
    let _ = tableRender(&bench, &component);
    let inspected = tableRender(&bench, &component);
    let tool = tableRowsContaining(&inspected, "bash").get(0);
    assert_eq!(tableProp(&tool, "aria-selected").as_bool(), Some(true));
    assert!(tableCalls(&bench).iter().any(|call| {
        let call = Array::from(&call);
        call.get(0).as_string().as_deref() == Some("inspect-applied")
    }));
}

#[wasm_bindgen_test]
fn thinking_disclosure_error_payload_and_json_tree_paths_are_live() {
    let thinking = makeTableBench("ordinary");
    tableSetTurns(&thinking, &tableThinkingTurns());
    let thinking_component = component(&thinking);
    let first = settled_render(&thinking, &thinking_component);
    let row = tableRowsContaining(&first, "private chain").get(0);
    tableInvoke(&row, "onClick", &JsValue::UNDEFINED);
    let selected = tableRender(&thinking, &thinking_component);
    let toggle = tableFind(&selected, "aria-label", &JsValue::from_str("Thinking"));
    assert_eq!(tableProp(&toggle, "aria-expanded").as_bool(), Some(false));
    assert!(
        tableFind(
            &selected,
            "text",
            &JsValue::from_str("private chain private chain")
        )
        .is_undefined()
    );
    tableInvoke(&toggle, "onClick", &JsValue::UNDEFINED);
    let expanded = tableRender(&thinking, &thinking_component);
    assert!(
        !tableFind(
            &expanded,
            "text",
            &JsValue::from_str("private chain private chain")
        )
        .is_undefined()
    );

    let json = makeTableBench("ordinary");
    tableSetTurns(&json, &tableJsonTurns());
    let json_component = component(&json);
    let json_first = settled_render(&json, &json_component);
    let tool = tableRowsContaining(&json_first, "read").get(0);
    tableInvoke(&tool, "onClick", &JsValue::UNDEFINED);
    let tool_selected = tableRender(&json, &json_component);
    let result_tab = tableFind(&tool_selected, "aria-label", &JsValue::from_str("Result"));
    tableInvoke(&result_tab, "onClick", &JsValue::UNDEFINED);
    let result = tableRender(&json, &json_component);
    assert!(!tableFind(&result, "label", &JsValue::from_str("Result JSON")).is_undefined());

    let failed = makeTableBench("ordinary");
    let failed_component = component(&failed);
    let failed_first = settled_render(&failed, &failed_component);
    let failed_row = tableRowsContaining(&failed_first, "false").get(0);
    tableInvoke(&failed_row, "onClick", &JsValue::UNDEFINED);
    let failed_selected = tableRender(&failed, &failed_component);
    let failed_result_tab = tableFind(&failed_selected, "aria-label", &JsValue::from_str("Result"));
    tableInvoke(&failed_result_tab, "onClick", &JsValue::UNDEFINED);
    let failed_result = tableRender(&failed, &failed_component);
    let payload = tableFind(
        &failed_result,
        "className",
        &JsValue::from_str(
            "seekdeep-trajectory-table-payload seekdeep-trajectory-table-errorPayload",
        ),
    );
    assert!(!payload.is_undefined());
    assert!(tableText(&payload).contains("ToolError: non_zero_exit"));
}

#[wasm_bindgen_test]
fn folds_roles_request_runs_whitespace_and_resize_adapters_are_live() {
    let folded = makeTableBench("ordinary");
    tableSetCollapsedTurn(&folded, 2);
    let folded_component = component(&folded);
    let folded_tree = settled_render(&folded, &folded_component);
    assert!(
        !tableFind(
            &folded_tree,
            "data-collapsed-summary",
            &JsValue::from_str("turn")
        )
        .is_undefined()
    );
    assert_eq!(
        tableRowsContaining(&folded_tree, "Checking files").length(),
        1
    );

    let roles = makeTableBench("ordinary");
    tableSetTurns(&roles, &tableRoleTurns());
    let roles_component = component(&roles);
    let roles_tree = settled_render(&roles, &roles_component);
    assert!(
        !tableFind(
            &roles_tree,
            "data-role-icon",
            &JsValue::from_str("information")
        )
        .is_undefined()
    );
    assert!(
        !tableFind(
            &roles_tree,
            "data-role-icon",
            &JsValue::from_str("compacted")
        )
        .is_undefined()
    );

    let runs = makeTableBench("ordinary");
    tableSetTurns(&runs, &tableRequestRunTurns());
    let runs_component = component(&runs);
    let runs_tree = settled_render(&runs, &runs_component);
    for (request, run, offset) in [(1, 0, "0px"), (2, 1, "8px"), (3, 2, "16px")] {
        let marker = tableFind(
            &runs_tree,
            "aria-label",
            &JsValue::from_str(&format!("Request #{request}")),
        );
        assert_eq!(
            tableProp(&marker, "data-request-run-index").as_f64(),
            Some(f64::from(run))
        );
        assert_eq!(
            property(&tableProp(&marker, "style"), "--request-boundary-offset")
                .as_string()
                .as_deref(),
            Some(offset)
        );
    }

    let ordinary = makeTableBench("ordinary");
    let ordinary_component = component(&ordinary);
    let first = settled_render(&ordinary, &ordinary_component);
    let row = tableRowsContaining(&first, "Checking files").get(0);
    tableInvoke(&row, "onClick", &JsValue::UNDEFINED);
    let selected = tableRender(&ordinary, &ordinary_component);
    let separator = tableFind(
        &selected,
        "aria-label",
        &JsValue::from_str("Resize event details"),
    );
    let key = tableEvent(
        &separator,
        &separator,
        &object(&[("key", JsValue::from_str("ArrowLeft"))]),
    );
    tableInvoke(&separator, "onKeyDown", &key);
    let resized = tableRender(&ordinary, &ordinary_component);
    let details = tableFind(&resized, "role", &JsValue::from_str("complementary"));
    assert_eq!(
        property(&tableProp(&details, "style"), "width")
            .as_string()
            .as_deref(),
        Some("416px")
    );
    let pane = tablePane(&ordinary);
    tableInvoke(
        &pane,
        "onClick",
        &tableEvent(&pane, &pane, &JsValue::UNDEFINED),
    );
    let cleared = tableRender(&ordinary, &ordinary_component);
    assert!(tableFind(&cleared, "role", &JsValue::from_str("complementary")).is_undefined());
}
