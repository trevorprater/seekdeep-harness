//! Live WASM coverage for message actions and local-midnight refresh.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_ui_conversation::{
    configure_client_ui_conversation_message_actions, message_icon_actions_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let nextId = 1
let nowMs = 0
let timers = new Map()
let nextTimer = 1
let writeMode = 'success'
let writeCount = 0
let pendingWrites = []
let branchCount = 0
let originals
function sameDeps(left, right) {
  return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index]))
}
function fakeSetTimeout(callback, delay) {
  const id = nextTimer++
  timers.set(id, { callback, at: nowMs + Math.max(Number(delay) || 0, 0) })
  return id
}
function fakeClearTimeout(id) { timers.delete(id) }
function runUntil(target) {
  while (true) {
    const due = [...timers.entries()].filter(([, timer]) => timer.at <= target).sort((a, b) => a[1].at - b[1].at || a[0] - b[0])[0]
    if (!due) break
    const [id, timer] = due
    timers.delete(id)
    nowMs = timer.at
    timer.callback()
  }
  nowMs = target
}
export function installActionsBench(initialNow) {
  hooks = []; cursor = 0; nextId = 1; timers = new Map(); nextTimer = 1
  nowMs = initialNow; writeMode = 'success'; writeCount = 0; pendingWrites = []; branchCount = 0
  originals = {
    dateNow: Date.now, setTimeout: globalThis.setTimeout, clearTimeout: globalThis.clearTimeout,
    window: globalThis.window, document: globalThis.document,
  }
  Date.now = () => nowMs
  globalThis.setTimeout = fakeSetTimeout
  globalThis.clearTimeout = fakeClearTimeout
  globalThis.window = { setTimeout: fakeSetTimeout }
  globalThis.document = {
    head: { appendChild() {} }, createElement() { return { setAttribute() {} } }, querySelector() { return null },
  }
  const React = {
    Fragment: 'Fragment',
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { type: 'state', value: typeof initial === 'function' ? initial() : initial }
      const set = update => { hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update }
      return [hooks[index].value, set]
    },
    useRef(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { type: 'ref', value: { current: initial } }
      return hooks[index].value
    },
    useId() {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { type: 'id', value: `action-reason-${nextId++}` }
      return hooks[index].value
    },
    useEffect(effect, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) {
        hooks[index]?.cleanup?.()
        hooks[index] = { type: 'effect', deps: [...deps], cleanup: effect() }
      }
    },
    useCallback(callback, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) hooks[index] = { type: 'callback', deps: [...deps], value: callback }
      return hooks[index].value
    },
  }
  const writeClipboard = text => {
    writeCount += 1
    if (writeMode === 'success') return Promise.resolve(true)
    if (writeMode === 'failure') return Promise.resolve(false)
    return new Promise(resolve => { pendingWrites.push(resolve) })
  }
  const uiPrimitives = {
    Tooltip: 'Tooltip', IconBranchOutline16: 'IconBranchOutline16', IconCheckOutline16: 'IconCheckOutline16',
    IconCopyOutline16: 'IconCopyOutline16', writeClipboard,
  }
  return { React, uiPrimitives }
}
export function actionsRestoreGlobals() {
  for (const hook of [...hooks].reverse()) hook?.cleanup?.()
  Date.now = originals.dateNow
  globalThis.setTimeout = originals.setTimeout
  globalThis.clearTimeout = originals.clearTimeout
  if (originals.window === undefined) delete globalThis.window; else globalThis.window = originals.window
  if (originals.document === undefined) delete globalThis.document; else globalThis.document = originals.document
}
export function actionsRender(component, props) { cursor = 0; return component(props) }
export function actionsObject(entries) { return Object.fromEntries(entries) }
export function actionsLocalDate(year, month, day, hours = 0, minutes = 0) { return new Date(year, month, day, hours, minutes).getTime() }
export function actionsAdvance(milliseconds) { runUntil(nowMs + milliseconds) }
export function actionsTimerCount() { return timers.size }
export function actionsSetWriteMode(mode) { writeMode = mode }
export function actionsWriteCount() { return writeCount }
export function actionsResolveWrite(ok) { pendingWrites.shift()?.(ok) }
export function actionsUnmount() {
  for (const hook of [...hooks].reverse()) hook?.cleanup?.()
  hooks = []
}
export function makeActionsTranslate() {
  const copy = {
    copy: '复制', copied: '复制成功', 'message.branch': '在新对话中分支',
    'message.branchUnavailable': '只能从对话末尾分支',
  }
  return (key, vars) => {
    if (key === 'clock.md') return `${vars.m}月${vars.d}日`
    if (key === 'clock.ymd') return `${vars.y}年${vars.m}月${vars.d}日`
    if (key === 'duration.seconds') return `${vars.seconds}秒`
    if (key === 'duration.minutes') return `${vars.minutes}分${vars.seconds}秒`
    if (key === 'message.ranFor') return `用时 ${vars.duration}`
    if (key === 'message.ttft') return `首 token ${vars.seconds}秒`
    if (key === 'message.tokensPerSecond') return `${vars.tps} tok/s`
    return copy[key] ?? key
  }
}
export function makeBranchAction() { return () => { branchCount += 1 } }
export function actionsBranchCount() { return branchCount }
export function actionsMarker() { return { kind: 'marker', props: {}, children: ['EXTRA'] } }
export function actionsText(value) {
  if (value === null || value === undefined || typeof value === 'boolean') return ''
  if (typeof value === 'string' || typeof value === 'number') return String(value)
  if (Array.isArray(value)) return value.map(actionsText).join('')
  return actionsText(value.children)
}
export function actionsFindButton(value, label) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.kind === 'button' && value.props?.['aria-label'] === label) return value
  for (const child of value.children ?? []) { const found = actionsFindButton(child, label); if (found) return found }
  return undefined
}
export function actionsFindId(value, id) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.props?.id === id) return value
  for (const child of value.children ?? []) { const found = actionsFindId(child, id); if (found) return found }
  return undefined
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installActionsBench)]
    fn install_actions_bench(initial_now: f64) -> JsValue;
    #[wasm_bindgen(js_name = actionsRestoreGlobals)]
    fn actions_restore_globals();
    #[wasm_bindgen(js_name = actionsRender)]
    fn actions_render(component: &JsValue, props: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = actionsObject)]
    fn actions_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = actionsLocalDate)]
    fn actions_local_date(year: u32, month: u32, day: u32, hours: u32, minutes: u32) -> f64;
    #[wasm_bindgen(js_name = actionsAdvance)]
    fn actions_advance(milliseconds: f64);
    #[wasm_bindgen(js_name = actionsTimerCount)]
    fn actions_timer_count() -> u32;
    #[wasm_bindgen(js_name = actionsSetWriteMode)]
    fn actions_set_write_mode(mode: &str);
    #[wasm_bindgen(js_name = actionsWriteCount)]
    fn actions_write_count() -> u32;
    #[wasm_bindgen(js_name = actionsResolveWrite)]
    fn actions_resolve_write(ok: bool);
    #[wasm_bindgen(js_name = actionsUnmount)]
    fn actions_unmount();
    #[wasm_bindgen(js_name = makeActionsTranslate)]
    fn make_actions_translate() -> Function;
    #[wasm_bindgen(js_name = makeBranchAction)]
    fn make_branch_action() -> Function;
    #[wasm_bindgen(js_name = actionsBranchCount)]
    fn actions_branch_count() -> u32;
    #[wasm_bindgen(js_name = actionsMarker)]
    fn actions_marker() -> JsValue;
    #[wasm_bindgen(js_name = actionsText)]
    fn actions_text(value: &JsValue) -> String;
    #[wasm_bindgen(js_name = actionsFindButton)]
    fn actions_find_button(value: &JsValue, label: &str) -> JsValue;
    #[wasm_bindgen(js_name = actionsFindId)]
    fn actions_find_id(value: &JsValue, id: &str) -> JsValue;
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
    actions_object(&array).unchecked_into()
}

fn setup(now: f64) -> (JsValue, Function) {
    let bench = install_actions_bench(now);
    configure_client_ui_conversation_message_actions(
        property(&bench, "React"),
        property(&bench, "uiPrimitives"),
    )
    .unwrap();
    (
        message_icon_actions_component().unwrap(),
        make_actions_translate(),
    )
}

fn base_props(translate: &Function) -> Object {
    object(&[
        ("text", JsValue::from_str("answer")),
        ("clock", JsValue::from_str("end")),
        ("t", translate.clone().into()),
    ])
}

async fn flush_microtasks() {
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
    JsFuture::from(Promise::resolve(&JsValue::UNDEFINED))
        .await
        .unwrap();
}

#[wasm_bindgen_test]
fn clock_metrics_extra_action_and_class_composition_preserve_order() {
    let now = actions_local_date(2026, 6, 29, 10, 0);
    let (component, translate) = setup(now);
    let time = actions_local_date(2026, 6, 29, 14, 24);
    let tree = actions_render(
        &component,
        object(&[
            ("text", JsValue::from_str("answer")),
            ("time", JsValue::from_f64(time)),
            ("runMs", JsValue::from_f64(19_000.0)),
            ("ttftMs", JsValue::from_f64(1_200.0)),
            ("tokensPerSecond", JsValue::from_f64(20.0)),
            ("clock", JsValue::from_str("end")),
            ("className", JsValue::from_str("parent-actions")),
            ("extraActions", actions_marker()),
            ("t", translate.into()),
        ])
        .as_ref(),
    );
    assert_eq!(
        property(&property(&tree, "props"), "className")
            .as_string()
            .as_deref(),
        Some("seekdeep-conversation-messageActions-actions parent-actions")
    );
    assert_eq!(
        property(&child(&tree, 5), "kind").as_string().as_deref(),
        Some("span")
    );
    assert_eq!(
        actions_text(&child(&tree, 5)),
        "14:24 · 用时 19秒 · 首 token 1.2秒 · 20 tok/s"
    );
    assert_eq!(actions_text(&child(&tree, 2)), "EXTRA");
    assert!(child(&tree, 0).is_null());
    actions_unmount();
    assert_eq!(actions_timer_count(), 0);
    actions_restore_globals();
}

#[wasm_bindgen_test]
fn branch_action_stays_focusable_with_a_linked_unavailable_reason() {
    let now = actions_local_date(2026, 6, 29, 10, 0);
    let (component, translate) = setup(now);
    let branch = make_branch_action();
    let unavailable_props = object(&[
        ("text", JsValue::from_str("answer")),
        ("clock", JsValue::from_str("end")),
        ("onBranch", branch.clone().into()),
        ("branchUnavailable", JsValue::TRUE),
        ("t", translate.clone().into()),
    ]);
    let unavailable = actions_render(&component, unavailable_props.as_ref());
    let button = actions_find_button(&unavailable, "在新对话中分支");
    assert_eq!(
        property(&property(&button, "props"), "aria-disabled").as_bool(),
        Some(true)
    );
    assert!(property(&property(&button, "props"), "onClick").is_undefined());
    let reason_id = property(&property(&button, "props"), "aria-describedby")
        .as_string()
        .unwrap();
    assert_eq!(
        actions_text(&actions_find_id(&unavailable, &reason_id)),
        "只能从对话末尾分支"
    );

    let available = actions_render(
        &component,
        object(&[
            ("text", JsValue::from_str("answer")),
            ("clock", JsValue::from_str("end")),
            ("onBranch", branch.into()),
            ("branchUnavailable", JsValue::FALSE),
            ("t", translate.into()),
        ])
        .as_ref(),
    );
    let button = actions_find_button(&available, "在新对话中分支");
    assert!(property(&property(&button, "props"), "aria-disabled").is_undefined());
    property(&property(&button, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert_eq!(actions_branch_count(), 1);
    actions_unmount();
    actions_restore_globals();
}

#[wasm_bindgen_test(async)]
async fn copy_feedback_gates_pending_and_copied_clicks_and_ignores_post_unmount_settlement() {
    let now = actions_local_date(2026, 6, 29, 10, 0);
    let (component, translate) = setup(now);
    let props = base_props(&translate);
    let initial = actions_render(&component, props.as_ref());
    let copy = actions_find_button(&initial, "复制")
        .dyn_into::<Object>()
        .unwrap();
    let click = property(copy.as_ref(), "props");
    let click = property(&click, "onClick").dyn_into::<Function>().unwrap();
    click.call0(&JsValue::UNDEFINED).unwrap();
    click.call0(&JsValue::UNDEFINED).unwrap();
    assert_eq!(actions_write_count(), 1);
    flush_microtasks().await;
    let copied = actions_render(&component, props.as_ref());
    let copied_button = actions_find_button(&copied, "复制成功");
    assert_eq!(
        property(&child(&copied_button, 0), "kind")
            .as_string()
            .as_deref(),
        Some("IconCheckOutline16")
    );
    property(&property(&copied_button, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert_eq!(actions_write_count(), 1);
    actions_advance(1_000.0);
    let reset = actions_render(&component, props.as_ref());
    assert!(!actions_find_button(&reset, "复制").is_undefined());

    actions_set_write_mode("failure");
    let rejected = actions_find_button(&reset, "复制");
    property(&property(&rejected, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    flush_microtasks().await;
    let unchanged = actions_render(&component, props.as_ref());
    assert!(!actions_find_button(&unchanged, "复制").is_undefined());
    assert_eq!(actions_timer_count(), 1, "only the calendar timer remains");

    actions_set_write_mode("pending");
    let pending = actions_find_button(&unchanged, "复制");
    property(&property(&pending, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert_eq!(actions_write_count(), 3);
    actions_unmount();
    actions_resolve_write(true);
    flush_microtasks().await;
    assert_eq!(actions_timer_count(), 0);
    actions_restore_globals();
}

#[wasm_bindgen_test]
fn calendar_hook_refreshes_the_clock_at_local_midnight_and_cleans_its_timer() {
    let now = actions_local_date(2026, 6, 29, 23, 50);
    let (component, translate) = setup(now);
    let time = actions_local_date(2026, 6, 29, 14, 24);
    let props = object(&[
        ("text", JsValue::from_str("night bubble")),
        ("time", JsValue::from_f64(time)),
        ("clock", JsValue::from_str("start")),
        ("t", translate.into()),
    ]);
    let before = actions_render(&component, props.as_ref());
    assert_eq!(actions_text(&child(&before, 0)), "14:24");
    assert_eq!(actions_timer_count(), 1);
    actions_advance(10.0 * 60_000.0 + 1.0);
    let after = actions_render(&component, props.as_ref());
    assert_eq!(actions_text(&child(&after, 0)), "7月29日 14:24");
    assert_eq!(actions_timer_count(), 1, "the next midnight is re-armed");
    actions_unmount();
    assert_eq!(actions_timer_count(), 0);
    actions_restore_globals();
}
