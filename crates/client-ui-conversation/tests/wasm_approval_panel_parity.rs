//! Live WASM coverage for approval takeover and one-shot answering.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Promise, Reflect};
use seekdeep_client_ui_conversation::{
    approval_flow_component, approval_panel_component, command_of_browser,
    configure_client_ui_conversation_approval_panel,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let rootSnapshot = { chat: { nodes: new Map() } }
let constructorCount = 0
let answerMode = 'success'
let answers = []
function sameDeps(left, right) { return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index])) }
class PendingApprovalBench {
  constructor(wait) { this.wait = wait; constructorCount += 1 }
  get key() { return this.wait.key }
  get toolName() { return this.wait.toolName }
  get reason() { return this.wait.reason }
  get callId() { return this.wait.callId }
  answer(outcome) {
    answers.push(outcome)
    return answerMode === 'failure' ? Promise.reject(new Error('rejected')) : Promise.resolve()
  }
}
export function installApprovalBench() {
  hooks = []; cursor = 0; rootSnapshot = { chat: { nodes: new Map() } }
  constructorCount = 0; answerMode = 'success'; answers = []
  globalThis.document = {
    head: { appendChild() {} }, createElement() { return { setAttribute() {} } }, querySelector() { return null },
  }
  const React = {
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    useMemo(factory, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) hooks[index] = { type: 'memo', value: factory(), deps: [...deps] }
      return hooks[index].value
    },
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { type: 'state', value: initial }
      const set = update => { hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update }
      return [hooks[index].value, set]
    },
  }
  return { React, uiPrimitives: { Button: 'Button' }, PendingApprovalBench }
}
export function approvalResetHooks() { hooks = []; cursor = 0 }
export function approvalObject(entries) { return Object.fromEntries(entries) }
export function approvalEntry(key, value) { return [key, value] }
export function approvalSetRoot(callId, node) { rootSnapshot = { chat: { nodes: new Map([[`9:tool-call${callId}`, node]]) } } }
export function makeApprovalUseSession() { return selector => selector(rootSnapshot) }
export function makeApprovalTranslate() {
  const copy = {
    'approval.waiting': '等待审批', 'approval.detail.aria': '审批详情',
    'approval.reject': '拒绝', 'approval.allowOnce': '允许一次',
  }
  return (key, vars) => key === 'approval.escalation' ? `${vars.toolName} 请求权限` : copy[key] ?? key
}
export function approvalRender(component, props) { cursor = 0; return component(props) }
export function approvalMakeMatched(key, callId, toolName, reason) { return { key, callId, toolName, reason } }
export function approvalConstructorCount() { return constructorCount }
export function approvalSetAnswerMode(mode) { answerMode = mode }
export function approvalAnswers() { return answers }
export function approvalText(value) {
  if (value === null || value === undefined || typeof value === 'boolean') return ''
  if (typeof value === 'string' || typeof value === 'number') return String(value)
  if (Array.isArray(value)) return value.map(approvalText).join('')
  return approvalText(value.children)
}
export function approvalFindButton(value, label) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.kind === 'Button' && approvalText(value) === label) return value
  for (const child of value.children ?? []) { const found = approvalFindButton(child, label); if (found) return found }
  return undefined
}
export function approvalFindProp(value, key, expected) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.props?.[key] === expected) return value
  for (const child of value.children ?? []) { const found = approvalFindProp(child, key, expected); if (found) return found }
  return undefined
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installApprovalBench)]
    fn install_approval_bench() -> JsValue;
    #[wasm_bindgen(js_name = approvalResetHooks)]
    fn approval_reset_hooks();
    #[wasm_bindgen(js_name = approvalObject)]
    fn approval_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = approvalSetRoot)]
    fn approval_set_root(call_id: &str, node: &JsValue);
    #[wasm_bindgen(js_name = makeApprovalUseSession)]
    fn make_approval_use_session() -> Function;
    #[wasm_bindgen(js_name = makeApprovalTranslate)]
    fn make_approval_translate() -> Function;
    #[wasm_bindgen(js_name = approvalRender)]
    fn approval_render(component: &JsValue, props: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = approvalMakeMatched)]
    fn approval_make_matched(
        key: &str,
        call_id: JsValue,
        tool_name: &str,
        reason: JsValue,
    ) -> JsValue;
    #[wasm_bindgen(js_name = approvalConstructorCount)]
    fn approval_constructor_count() -> u32;
    #[wasm_bindgen(js_name = approvalSetAnswerMode)]
    fn approval_set_answer_mode(mode: &str);
    #[wasm_bindgen(js_name = approvalAnswers)]
    fn approval_answers() -> Array;
    #[wasm_bindgen(js_name = approvalText)]
    fn approval_text(value: &JsValue) -> String;
    #[wasm_bindgen(js_name = approvalFindButton)]
    fn approval_find_button(value: &JsValue, label: &str) -> JsValue;
    #[wasm_bindgen(js_name = approvalFindProp)]
    fn approval_find_prop(value: &JsValue, key: &str, expected: &JsValue) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    approval_object(&array).unchecked_into()
}

fn setup() -> (JsValue, JsValue) {
    let bench = install_approval_bench();
    configure_client_ui_conversation_approval_panel(
        property(&bench, "React"),
        property(&bench, "uiPrimitives"),
        property(&bench, "PendingApprovalBench")
            .dyn_into::<Function>()
            .unwrap(),
    )
    .unwrap();
    (
        approval_panel_component().unwrap(),
        approval_flow_component().unwrap(),
    )
}

fn outer_props(matched: JsValue) -> Object {
    object(&[
        ("matched", matched),
        ("useSession", make_approval_use_session().into()),
        ("t", make_approval_translate().into()),
    ])
}

fn running(call_id: &str, args_raw: &str) -> Object {
    object(&[
        ("callId", JsValue::from_str(call_id)),
        ("argsRaw", JsValue::from_str(args_raw)),
        ("subCalls", Array::new().into()),
    ])
}

fn tool_node(root: JsValue) -> Object {
    object(&[
        ("kind", JsValue::from_str("tool-call")),
        ("data", object(&[("root", root)]).into()),
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
fn command_extraction_soft_falls_for_absent_invalid_and_non_string_arguments() {
    assert!(
        command_of_browser(JsValue::UNDEFINED)
            .unwrap()
            .is_undefined()
    );
    assert_eq!(
        command_of_browser(running("c", "{\"command\":\"echo hi\"}").into())
            .unwrap()
            .as_string()
            .as_deref(),
        Some("echo hi")
    );
    assert!(
        command_of_browser(running("c", "{").into())
            .unwrap()
            .is_undefined()
    );
    assert!(
        command_of_browser(running("c", "{\"command\":3}").into())
            .unwrap()
            .is_undefined()
    );
}

#[wasm_bindgen_test]
fn outer_panel_memoizes_domain_face_keys_flow_and_pairs_only_matching_running_root() {
    let (component, flow) = setup();
    let matched = approval_make_matched(
        "approval:1",
        JsValue::from_str("call:1"),
        "bash",
        JsValue::from_str("Need to inspect files"),
    );
    let root = running("call:1", "{\"command\":\"rg TODO\"}");
    let tool = tool_node(root.into());
    approval_set_root("call:1", tool.as_ref());
    let props = outer_props(matched.clone());
    let first = approval_render(&component, props.as_ref());
    assert!(Object::is(&property(&first, "kind"), &flow));
    assert_eq!(
        property(&property(&first, "props"), "key")
            .as_string()
            .as_deref(),
        Some("approval:1")
    );
    assert_eq!(
        property(&property(&first, "props"), "command")
            .as_string()
            .as_deref(),
        Some("rg TODO")
    );
    let pending = property(&property(&first, "props"), "pending");
    let second = approval_render(&component, props.as_ref());
    assert!(Object::is(
        &pending,
        &property(&property(&second, "props"), "pending")
    ));
    assert_eq!(approval_constructor_count(), 1);

    let settled = object(&[
        ("kind", JsValue::from_str("tool-result")),
        ("callId", JsValue::from_str("call:1")),
        ("subCalls", Array::new().into()),
    ]);
    approval_set_root("call:1", tool_node(settled.into()).as_ref());
    let no_command = approval_render(&component, props.as_ref());
    assert!(property(&property(&no_command, "props"), "command").is_undefined());
}

#[wasm_bindgen_test]
fn flow_renders_accessible_scroll_region_reason_command_and_action_variants() {
    let (outer, flow) = setup();
    let matched = approval_make_matched(
        "approval:layout",
        JsValue::UNDEFINED,
        "bash",
        JsValue::UNDEFINED,
    );
    let outer_tree = approval_render(&outer, outer_props(matched).as_ref());
    let pending = property(&property(&outer_tree, "props"), "pending");
    approval_reset_hooks();
    let tree = approval_render(
        &flow,
        object(&[
            ("pending", pending),
            ("command", JsValue::from_str("cat very-long-file")),
            ("t", make_approval_translate().into()),
        ])
        .as_ref(),
    );
    assert_eq!(
        property(&property(&tree, "props"), "data-approval-key")
            .as_string()
            .as_deref(),
        Some("approval:layout")
    );
    let body = approval_find_prop(&tree, "data-approval-scroll", &JsValue::from_str(""));
    assert_eq!(
        property(&property(&body, "props"), "tabIndex").as_f64(),
        Some(0.0)
    );
    assert_eq!(
        property(&property(&body, "props"), "role")
            .as_string()
            .as_deref(),
        Some("group")
    );
    let text = approval_text(&tree);
    assert!(text.contains("等待审批"));
    assert!(text.contains("bash 请求权限"));
    assert!(text.contains("cat very-long-file"));
    let reject = approval_find_button(&tree, "拒绝");
    let allow = approval_find_button(&tree, "允许一次");
    assert_eq!(
        property(&property(&reject, "props"), "variant")
            .as_string()
            .as_deref(),
        Some("outline")
    );
    assert_eq!(
        property(&property(&allow, "props"), "variant")
            .as_string()
            .as_deref(),
        Some("primary")
    );
}

#[wasm_bindgen_test(async)]
async fn answer_latch_disables_both_buttons_rearms_on_rejection_and_stays_set_on_success() {
    let (outer, flow) = setup();
    let matched = approval_make_matched(
        "approval:answer",
        JsValue::UNDEFINED,
        "bash",
        JsValue::from_str("Need approval"),
    );
    let outer_tree = approval_render(&outer, outer_props(matched).as_ref());
    let pending = property(&property(&outer_tree, "props"), "pending");
    approval_reset_hooks();
    let props = object(&[
        ("pending", pending),
        ("t", make_approval_translate().into()),
    ]);
    approval_set_answer_mode("failure");
    let initial = approval_render(&flow, props.as_ref());
    property(
        &property(&approval_find_button(&initial, "拒绝"), "props"),
        "onClick",
    )
    .dyn_into::<Function>()
    .unwrap()
    .call0(&JsValue::UNDEFINED)
    .unwrap();
    let disabled = approval_render(&flow, props.as_ref());
    assert_eq!(
        property(
            &property(&approval_find_button(&disabled, "拒绝"), "props"),
            "disabled"
        )
        .as_bool(),
        Some(true)
    );
    assert_eq!(
        property(
            &property(&approval_find_button(&disabled, "允许一次"), "props"),
            "disabled"
        )
        .as_bool(),
        Some(true)
    );
    flush_microtasks().await;
    let rearmed = approval_render(&flow, props.as_ref());
    assert_eq!(
        property(
            &property(&approval_find_button(&rearmed, "拒绝"), "props"),
            "disabled"
        )
        .as_bool(),
        Some(false)
    );
    assert_eq!(
        approval_answers().get(0).as_string().as_deref(),
        Some("rejected")
    );

    approval_set_answer_mode("success");
    property(
        &property(&approval_find_button(&rearmed, "允许一次"), "props"),
        "onClick",
    )
    .dyn_into::<Function>()
    .unwrap()
    .call0(&JsValue::UNDEFINED)
    .unwrap();
    flush_microtasks().await;
    let latched = approval_render(&flow, props.as_ref());
    assert_eq!(
        property(
            &property(&approval_find_button(&latched, "允许一次"), "props"),
            "disabled"
        )
        .as_bool(),
        Some(true)
    );
    assert_eq!(
        approval_answers().get(1).as_string().as_deref(),
        Some("allowed-once")
    );
}
