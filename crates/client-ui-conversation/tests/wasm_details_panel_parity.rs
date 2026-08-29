//! Live WASM coverage for selected Tool details presentation.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_conversation::{
    configure_client_ui_conversation_details_panel, details_panel_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let selection = null
let snapshot
let sessions
let slotCalls = []
let closeCount = 0
let shallowFace
let comparatorSeen = false
export function installDetailsBench() {
  selection = null; snapshot = { chat: { nodes: new Map() } }; sessions = { byId: {} }
  slotCalls = []; closeCount = 0; comparatorSeen = false
  globalThis.document = {
    head: { appendChild() {} }, createElement() { return { setAttribute() {} } }, querySelector() { return null },
  }
  const React = { Fragment: 'Fragment', createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } } }
  const uiPrimitives = { CodeBlock: 'CodeBlock' }
  shallowFace = (left, right) => {
    if (Object.is(left, right)) return true
    if (!left || !right || typeof left !== 'object' || typeof right !== 'object') return false
    const keys = Object.keys(left)
    return keys.length === Object.keys(right).length && keys.every(key => Object.is(left[key], right[key]))
  }
  return { React, uiPrimitives, shallowFace }
}
export function detailsObject(entries) { return Object.fromEntries(entries) }
export function detailsEntry(key, value) { return [key, value] }
export function detailsSetSelection(value) { selection = value }
export function detailsSetSnapshot(entries) { snapshot = { chat: { nodes: new Map(entries) } } }
export function detailsSetCwd(sessionId, cwd) { sessions = { byId: { [sessionId]: { cwd } } } }
export function makeDetailsUseStore() { return selector => selector({ selection }) }
export function makeDetailsUseSessions() { return selector => selector(sessions) }
export function makeDetailsUseSession() {
  return (selector, equality) => {
    comparatorSeen = equality === shallowFace
    const value = selector(snapshot)
    equality(value, value)
    return value
  }
}
export function makeDetailsRenderSlot() {
  return (name, owner, options) => { slotCalls.push({ name, owner, options }); return { kind: 'slot-result', props: {}, children: [] } }
}
export function makeDetailsClose() { return () => { closeCount += 1 } }
export function makeDetailsTranslate() {
  const copy = {
    'details.title': '详情', 'details.close': '关闭详情', 'details.empty': '未选择调用',
    'details.notInWindow': '该调用不在当前窗口内', 'details.input': '输入', 'details.output': '输出',
    'details.running': '仍在运行', copy: '复制', copied: '复制成功',
  }
  return key => copy[key] ?? key
}
export function detailsRender(component, props) { return component(props) }
export function detailsSlotCalls() { return slotCalls }
export function detailsCloseCount() { return closeCount }
export function detailsComparatorSeen() { return comparatorSeen }
export function detailsText(value) {
  if (value === null || value === undefined || typeof value === 'boolean') return ''
  if (typeof value === 'string' || typeof value === 'number') return String(value)
  if (Array.isArray(value)) return value.map(detailsText).join('')
  return detailsText(value.children)
}
export function detailsFindKind(value, kind) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.kind === kind) return value
  for (const child of value.children ?? []) { const found = detailsFindKind(child, kind); if (found) return found }
  return undefined
}
export function detailsFindButton(value, label) {
  if (value === null || value === undefined || typeof value !== 'object') return undefined
  if (value.kind === 'button' && value.props?.['aria-label'] === label) return value
  for (const child of value.children ?? []) { const found = detailsFindButton(child, label); if (found) return found }
  return undefined
}
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installDetailsBench)]
    fn install_details_bench() -> JsValue;
    #[wasm_bindgen(js_name = detailsObject)]
    fn details_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = detailsEntry)]
    fn details_entry(key: &str, value: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = detailsSetSelection)]
    fn details_set_selection(value: &JsValue);
    #[wasm_bindgen(js_name = detailsSetSnapshot)]
    fn details_set_snapshot(entries: &Array);
    #[wasm_bindgen(js_name = detailsSetCwd)]
    fn details_set_cwd(session_id: &str, cwd: &str);
    #[wasm_bindgen(js_name = makeDetailsUseStore)]
    fn make_details_use_store() -> Function;
    #[wasm_bindgen(js_name = makeDetailsUseSessions)]
    fn make_details_use_sessions() -> Function;
    #[wasm_bindgen(js_name = makeDetailsUseSession)]
    fn make_details_use_session() -> Function;
    #[wasm_bindgen(js_name = makeDetailsRenderSlot)]
    fn make_details_render_slot() -> Function;
    #[wasm_bindgen(js_name = makeDetailsClose)]
    fn make_details_close() -> Function;
    #[wasm_bindgen(js_name = makeDetailsTranslate)]
    fn make_details_translate() -> Function;
    #[wasm_bindgen(js_name = detailsRender)]
    fn details_render(component: &JsValue, props: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = detailsSlotCalls)]
    fn details_slot_calls() -> Array;
    #[wasm_bindgen(js_name = detailsCloseCount)]
    fn details_close_count() -> u32;
    #[wasm_bindgen(js_name = detailsComparatorSeen)]
    fn details_comparator_seen() -> bool;
    #[wasm_bindgen(js_name = detailsText)]
    fn details_text(value: &JsValue) -> String;
    #[wasm_bindgen(js_name = detailsFindKind)]
    fn details_find_kind(value: &JsValue, kind: &str) -> JsValue;
    #[wasm_bindgen(js_name = detailsFindButton)]
    fn details_find_button(value: &JsValue, label: &str) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    details_object(&array).unchecked_into()
}

fn map_entries(entries: &[(&str, &JsValue)]) -> Array {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&details_entry(key, value));
    }
    array
}

fn block(call_id: &str, children: &[JsValue]) -> Object {
    let sub_calls = Array::new();
    for child in children {
        sub_calls.push(child);
    }
    object(&[
        ("callId", JsValue::from_str(call_id)),
        ("subCalls", sub_calls.into()),
    ])
}

fn tool_node(root: JsValue) -> Object {
    object(&[
        ("kind", JsValue::from_str("tool-call")),
        ("data", object(&[("root", root)]).into()),
    ])
}

fn setup() -> JsValue {
    let bench = install_details_bench();
    configure_client_ui_conversation_details_panel(
        property(&bench, "React"),
        property(&bench, "uiPrimitives"),
        property(&bench, "shallowFace")
            .dyn_into::<Function>()
            .unwrap(),
    )
    .unwrap();
    details_panel_component().unwrap()
}

fn props() -> Object {
    object(&[
        ("useSession", make_details_use_session().into()),
        ("useSessions", make_details_use_sessions().into()),
        ("sessionId", JsValue::from_str("s1")),
        ("useStore", make_details_use_store().into()),
        ("renderSlot", make_details_render_slot().into()),
        ("closeDetails", make_details_close().into()),
        ("t", make_details_translate().into()),
    ])
}

#[wasm_bindgen_test]
fn empty_selection_renders_default_title_and_close_action() {
    let component = setup();
    let tree = details_render(&component, props().as_ref());
    assert!(details_text(&tree).contains("详情"));
    assert!(details_text(&tree).contains("未选择调用"));
    let close = details_find_button(&tree, "关闭详情");
    property(&property(&close, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert_eq!(details_close_count(), 1);
    assert_eq!(details_slot_calls().length(), 0);
    assert!(details_comparator_seen());
}

#[wasm_bindgen_test]
fn missing_material_uses_tool_name_then_default_title_and_window_copy() {
    let component = setup();
    details_set_selection(
        object(&[
            ("callId", JsValue::from_str("ghost")),
            ("toolName", JsValue::from_str("bash")),
        ])
        .as_ref(),
    );
    let named = details_render(&component, props().as_ref());
    assert!(details_text(&named).contains("bash"));
    assert!(details_text(&named).contains("该调用不在当前窗口内"));

    details_set_selection(object(&[("callId", JsValue::from_str("ghost"))]).as_ref());
    let unnamed = details_render(&component, props().as_ref());
    assert!(details_text(&unnamed).starts_with("详情"));
}

#[wasm_bindgen_test]
fn nested_settled_material_prettifies_args_and_forwards_block_cwd_and_fallback() {
    let component = setup();
    details_set_selection(
        object(&[
            ("callId", JsValue::from_str("root:leaf")),
            ("toolName", JsValue::from_str("ignored")),
        ])
        .as_ref(),
    );
    details_set_cwd("s1", "/workspace");
    let leaf = object(&[
        ("kind", JsValue::from_str("tool-result")),
        ("callId", JsValue::from_str("root:leaf")),
        (
            "call",
            object(&[
                ("name", JsValue::from_str("read")),
                (
                    "argsRaw",
                    JsValue::from_str("{\"path\":\"notes/demo.txt\"}"),
                ),
            ])
            .into(),
        ),
        (
            "content",
            Array::of1(&object(&[
                ("type", JsValue::from_str("text")),
                ("text", JsValue::from_str("file body")),
            ]))
            .into(),
        ),
        ("isError", JsValue::FALSE),
        ("subCalls", Array::new().into()),
    ]);
    let root = block("root", &[leaf.clone().into()]);
    let tool = tool_node(root.into());
    details_set_snapshot(&map_entries(&[("9:tool-callroot", tool.as_ref())]));
    let tree = details_render(&component, props().as_ref());
    assert!(details_text(&tree).starts_with("read"));
    let code = details_find_kind(&tree, "CodeBlock");
    assert_eq!(
        property(&property(&code, "props"), "code")
            .as_string()
            .as_deref(),
        Some("{\n  \"path\": \"notes/demo.txt\"\n}")
    );
    let call = details_slot_calls().get(0);
    assert_eq!(
        property(&call, "name").as_string().as_deref(),
        Some("conversation.details.tool")
    );
    assert!(Object::is(
        &property(&property(&call, "owner"), "block"),
        leaf.as_ref()
    ));
    assert_eq!(
        property(&property(&call, "owner"), "cwd")
            .as_string()
            .as_deref(),
        Some("/workspace")
    );
    let fallback = property(&property(&call, "options"), "fallback");
    assert_eq!(details_text(&fallback), "file body");
    assert_eq!(
        property(&property(&fallback, "props"), "data-error").as_bool(),
        None
    );
}

#[wasm_bindgen_test]
fn running_and_headless_error_fallbacks_preserve_verbatim_and_failure_copy() {
    let component = setup();
    let running = object(&[
        ("callId", JsValue::from_str("run")),
        ("name", JsValue::from_str("bash")),
        ("argsRaw", JsValue::from_str("streaming {")),
        ("subCalls", Array::new().into()),
    ]);
    details_set_selection(object(&[("callId", JsValue::from_str("run"))]).as_ref());
    let tool = tool_node(running.into());
    details_set_snapshot(&map_entries(&[("9:tool-callrun", tool.as_ref())]));
    let tree = details_render(&component, props().as_ref());
    assert_eq!(
        property(
            &property(&details_find_kind(&tree, "CodeBlock"), "props"),
            "code"
        )
        .as_string()
        .as_deref(),
        Some("streaming {")
    );
    assert_eq!(
        details_text(&property(
            &property(&details_slot_calls().get(0), "options"),
            "fallback"
        )),
        "仍在运行"
    );

    let component = setup();
    let failed = object(&[
        ("kind", JsValue::from_str("tool-result")),
        ("callId", JsValue::from_str("failed")),
        ("call", JsValue::NULL),
        ("content", Array::new().into()),
        ("isError", JsValue::TRUE),
        (
            "error",
            object(&[
                ("name", JsValue::from_str("ToolError")),
                ("code", JsValue::from_str("E_FAIL")),
            ])
            .into(),
        ),
        ("subCalls", Array::new().into()),
    ]);
    details_set_selection(object(&[("callId", JsValue::from_str("failed"))]).as_ref());
    let tool = tool_node(failed.into());
    details_set_snapshot(&map_entries(&[("9:tool-callfailed", tool.as_ref())]));
    let tree = details_render(&component, props().as_ref());
    assert!(details_find_kind(&tree, "CodeBlock").is_undefined());
    let fallback = property(
        &property(&details_slot_calls().get(0), "options"),
        "fallback",
    );
    assert_eq!(details_text(&fallback), "ToolError: E_FAIL");
    assert_eq!(
        property(&property(&fallback, "props"), "data-error").as_bool(),
        Some(true)
    );
}
