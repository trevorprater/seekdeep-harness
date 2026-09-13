//! Live WASM coverage for command lifecycle and compaction renderers.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_conversation::{
    command_node_view_component, compaction_command_card_component, compaction_item_component,
    configure_client_ui_conversation_command, generic_command_card_component,
    manual_compaction_node_view_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let slotCalls = []
function sameDeps(left, right) {
  return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index]))
}
export function installCommandBench() {
  hooks = []; cursor = 0; slotCalls = []
  globalThis.document = {
    head: { appendChild() {} },
    createElement() { return { setAttribute() {} } },
    querySelector() { return null },
  }
  const React = {
    Fragment: 'Fragment',
    createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    memo(component) { return component },
    useMemo(factory, deps) {
      const index = cursor++
      if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) {
        hooks[index] = { type: 'memo', value: factory(), deps: [...deps] }
      }
      return hooks[index].value
    },
    useState(initial) {
      const index = cursor++
      if (!(index in hooks)) hooks[index] = { type: 'state', value: typeof initial === 'function' ? initial() : initial }
      const set = update => { hooks[index].value = typeof update === 'function' ? update(hooks[index].value) : update }
      return [hooks[index].value, set]
    },
  }
  const uiPrimitives = {
    DisclosureRow: 'DisclosureRow', IconApiOutline14: 'IconApiOutline14', StateDot: 'StateDot',
    IconChevronDownOutline14: 'IconChevronDownOutline14',
    IconChevronRightOutline14: 'IconChevronRightOutline14', MarkdownText: 'MarkdownText',
  }
  return { React, uiPrimitives }
}
export function commandResetHooks() { hooks = []; cursor = 0 }
export function commandRender(component, props) { cursor = 0; return component(props) }
export function commandObject(entries) { return Object.fromEntries(entries) }
export function makeCommandTranslate() {
  const copy = {
    'command.running': '执行中…', 'command.failed': '命令失败', 'command.done': '已完成',
    'command.title': '命令', 'row.running': '运行中', 'row.failed': '失败',
    'message.compaction.running': '正在压缩…', 'message.compaction.expand': '查看压缩摘要',
    'message.compaction.unavailable': '压缩摘要不可用', 'message.compaction': '上下文已压缩',
  }
  return (key, vars) => key === 'message.compaction.completed'
    ? `已压缩 ${vars.items} 条历史记录（约 ${vars.tokens} tokens）`
    : copy[key] ?? key
}
export function makeCommandSlotRecorder() {
  slotCalls = []
  return (name, owner, options) => { slotCalls.push({ name, owner, options }); return { kind: 'slot', props: {}, children: [] } }
}
export function commandSlotCalls() { return slotCalls }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installCommandBench)]
    fn install_command_bench() -> JsValue;
    #[wasm_bindgen(js_name = commandResetHooks)]
    fn command_reset_hooks();
    #[wasm_bindgen(js_name = commandRender)]
    fn command_render(component: &JsValue, props: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = commandObject)]
    fn command_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = makeCommandTranslate)]
    fn make_command_translate() -> Function;
    #[wasm_bindgen(js_name = makeCommandSlotRecorder)]
    fn make_command_slot_recorder() -> Function;
    #[wasm_bindgen(js_name = commandSlotCalls)]
    fn command_slot_calls() -> Array;
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
    command_object(&array).unchecked_into()
}

fn setup() -> Function {
    let bench = install_command_bench();
    configure_client_ui_conversation_command(
        property(&bench, "React"),
        property(&bench, "uiPrimitives"),
    )
    .unwrap();
    make_command_translate()
}

fn command(name: JsValue, outcome: JsValue) -> Object {
    object(&[("name", name), ("outcome", outcome)])
}

fn outcome(kind: &str, text: JsValue) -> Object {
    object(&[("kind", JsValue::from_str(kind)), ("text", text)])
}

#[wasm_bindgen_test]
fn ordinary_command_dispatch_preserves_owner_entry_and_generic_fallback() {
    let translate = setup();
    let component = command_node_view_component().unwrap();
    let generic = generic_command_card_component().unwrap();
    let command_data = command(
        JsValue::from_str("plan"),
        outcome("success", JsValue::from_str("已进入 plan mode")).into(),
    );
    let node = object(&[("data", command_data.clone().into())]);
    let render_slot = make_command_slot_recorder();
    let tree = command_render(
        &component,
        object(&[
            ("node", node.into()),
            ("renderSlot", render_slot.into()),
            ("t", translate.into()),
        ])
        .as_ref(),
    );
    assert_eq!(
        property(&property(&tree, "props"), "className")
            .as_string()
            .as_deref(),
        Some("seekdeep-conversation-chat-callRow")
    );
    let call = command_slot_calls().get(0);
    assert_eq!(
        property(&call, "name").as_string().as_deref(),
        Some("conversation.chat.commandview")
    );
    assert!(Object::is(
        &property(&property(&call, "owner"), "node"),
        command_data.as_ref()
    ));
    let options = property(&call, "options");
    assert_eq!(
        property(&options, "entryKey").as_string().as_deref(),
        Some("plan")
    );
    let fallback = property(&options, "fallback");
    assert!(Object::is(&property(&fallback, "kind"), &generic));
    assert!(Object::is(
        &property(&property(&fallback, "props"), "node"),
        command_data.as_ref()
    ));

    let orphan = command(JsValue::NULL, outcome("success", JsValue::UNDEFINED).into());
    let render_slot = make_command_slot_recorder();
    let _ = command_render(
        &component,
        object(&[
            ("node", object(&[("data", orphan.into())]).into()),
            ("renderSlot", render_slot.into()),
            ("t", make_command_translate().into()),
        ])
        .as_ref(),
    );
    assert_eq!(
        property(
            &property(&command_slot_calls().get(0), "options"),
            "entryKey"
        )
        .as_string()
        .as_deref(),
        Some("")
    );
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)] // One source card's closed lifecycle matrix stays together.
fn generic_card_pins_running_error_and_multiline_disclosure_states() {
    let translate = setup();
    let component = generic_command_card_component().unwrap();
    let running = command(JsValue::from_str("plan"), JsValue::NULL);
    let running_tree = command_render(
        &component,
        object(&[("node", running.into()), ("t", translate.clone().into())]).as_ref(),
    );
    assert_eq!(
        property(&property(&running_tree, "props"), "data-state")
            .as_string()
            .as_deref(),
        Some("running")
    );
    let running_disclosure = child(&running_tree, 1);
    let running_icon = property(&property(&running_disclosure, "props"), "icon");
    assert_eq!(
        property(&property(&running_icon, "props"), "size").as_f64(),
        Some(14.0)
    );
    let running_collapsed = property(&property(&running_disclosure, "props"), "collapsedContent");
    assert_eq!(
        child(&child(&running_collapsed, 1), 0)
            .as_string()
            .as_deref(),
        Some("执行中…")
    );
    assert_eq!(
        child(&child(&running_tree, 0), 0).as_string().as_deref(),
        Some("运行中")
    );

    command_reset_hooks();
    let failed = command(
        JsValue::from_str("plan"),
        outcome("error", JsValue::UNDEFINED).into(),
    );
    let failed_tree = command_render(
        &component,
        object(&[("node", failed.into()), ("t", translate.clone().into())]).as_ref(),
    );
    assert_eq!(
        property(&property(&failed_tree, "props"), "data-state")
            .as_string()
            .as_deref(),
        Some("error")
    );
    let failed_disclosure = child(&failed_tree, 1);
    let failed_icon = property(&property(&failed_disclosure, "props"), "icon");
    assert_eq!(
        property(&property(&failed_icon, "props"), "state")
            .as_string()
            .as_deref(),
        Some("error")
    );
    let failed_collapsed = property(&property(&failed_disclosure, "props"), "collapsedContent");
    assert_eq!(
        child(&child(&failed_collapsed, 1), 0)
            .as_string()
            .as_deref(),
        Some("命令失败")
    );
    assert_eq!(
        child(&child(&failed_tree, 0), 0).as_string().as_deref(),
        Some("失败")
    );

    command_reset_hooks();
    let orphan = command(JsValue::NULL, outcome("success", JsValue::UNDEFINED).into());
    let orphan_tree = command_render(
        &component,
        object(&[("node", orphan.into()), ("t", translate.clone().into())]).as_ref(),
    );
    let orphan_disclosure = child(&orphan_tree, 0);
    assert_eq!(
        property(&property(&orphan_disclosure, "props"), "title")
            .as_string()
            .as_deref(),
        Some("命令")
    );
    let orphan_collapsed = property(&property(&orphan_disclosure, "props"), "collapsedContent");
    assert_eq!(
        child(&child(&orphan_collapsed, 1), 0)
            .as_string()
            .as_deref(),
        Some("已完成")
    );

    command_reset_hooks();
    let multiline = command(
        JsValue::from_str("plan"),
        outcome("success", JsValue::from_str("line one\nline two")).into(),
    );
    let props = object(&[("node", multiline.into()), ("t", translate.into())]);
    let collapsed = command_render(&component, props.as_ref());
    assert_eq!(
        property(&property(&collapsed, "props"), "data-state")
            .as_string()
            .as_deref(),
        Some("ok")
    );
    let disclosure = child(&collapsed, 0);
    assert_eq!(
        property(&property(&disclosure, "props"), "expandable").as_bool(),
        Some(true)
    );
    property(&property(&disclosure, "props"), "onToggle")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let expanded = command_render(&component, props.as_ref());
    assert_eq!(
        property(&property(&child(&expanded, 0), "props"), "open").as_bool(),
        Some(true)
    );
}

#[wasm_bindgen_test]
fn compaction_adapter_selects_running_generic_settlement_and_checkpoint() {
    let translate = setup();
    let adapter = compaction_command_card_component().unwrap();
    let generic = generic_command_card_component().unwrap();
    let item = compaction_item_component().unwrap();
    let running = command(JsValue::from_str("compact"), JsValue::NULL);
    let running_tree = command_render(
        &adapter,
        object(&[("node", running.into()), ("t", translate.clone().into())]).as_ref(),
    );
    assert!(Object::is(&property(&running_tree, "kind"), &generic));
    assert_eq!(
        property(&property(&running_tree, "props"), "runningSummary")
            .as_string()
            .as_deref(),
        Some("正在压缩…")
    );

    let settled = command(
        JsValue::from_str("compact"),
        outcome("success", JsValue::from_str("No compactable history yet.")).into(),
    );
    let settled_tree = command_render(
        &adapter,
        object(&[
            ("node", settled.clone().into()),
            ("t", translate.clone().into()),
        ])
        .as_ref(),
    );
    assert!(Object::is(&property(&settled_tree, "kind"), &generic));
    assert!(property(&property(&settled_tree, "props"), "runningSummary").is_undefined());

    let checkpoint = object(&[("summary", JsValue::from_str("# 压缩摘要"))]);
    let checkpoint_tree = command_render(
        &adapter,
        object(&[
            ("node", settled.into()),
            ("compaction", checkpoint.clone().into()),
            ("t", translate.into()),
        ])
        .as_ref(),
    );
    assert!(Object::is(&property(&checkpoint_tree, "kind"), &item));
    assert!(Object::is(
        &property(&property(&checkpoint_tree, "props"), "node"),
        checkpoint.as_ref()
    ));
    assert_eq!(
        property(&property(&checkpoint_tree, "props"), "fallbackSummary")
            .as_string()
            .as_deref(),
        Some("No compactable history yet.")
    );
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)] // One checkpoint disclosure and manual adapter share the fixture.
fn compaction_item_toggles_summary_and_manual_node_omits_null_checkpoint() {
    let translate = setup();
    let item = compaction_item_component().unwrap();
    let checkpoint = object(&[
        ("summary", JsValue::from_str("# 压缩摘要\n\n保留的事实。")),
        ("shadowedItemCount", JsValue::from_f64(16.0)),
        ("shadowedTokenCount", JsValue::from_f64(11_309.0)),
    ]);
    let props = object(&[
        ("node", checkpoint.into()),
        ("title", JsValue::from_str("compact")),
        ("fallbackSummary", JsValue::NULL),
        ("t", translate.clone().into()),
    ]);
    let collapsed = command_render(&item, props.as_ref());
    let button = child(&collapsed, 0);
    assert_eq!(
        property(&property(&button, "props"), "aria-expanded").as_bool(),
        Some(false)
    );
    assert_eq!(
        child(&button, 3).as_string().as_deref(),
        None,
        "summary text is nested below its span"
    );
    assert_eq!(
        child(&child(&button, 3), 0).as_string().as_deref(),
        Some("已压缩 16 条历史记录（约 11309 tokens）")
    );
    assert_eq!(
        property(
            &property(&child(&child(&button, 0), 1), "props"),
            "data-compaction-disclosure"
        )
        .as_string()
        .as_deref(),
        Some("collapsed")
    );
    property(&property(&button, "props"), "onClick")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let expanded = command_render(&item, props.as_ref());
    assert_eq!(
        property(&property(&child(&expanded, 0), "props"), "aria-expanded").as_bool(),
        Some(true)
    );
    assert_eq!(
        property(
            &property(&child(&child(&child(&expanded, 0), 0), 1), "props"),
            "data-compaction-disclosure"
        )
        .as_string()
        .as_deref(),
        Some("expanded")
    );
    assert_eq!(
        property(&property(&child(&child(&expanded, 1), 0), "props"), "text")
            .as_string()
            .as_deref(),
        Some("# 压缩摘要\n\n保留的事实。")
    );

    command_reset_hooks();
    let manual = manual_compaction_node_view_component().unwrap();
    let command = command(JsValue::from_str("compact"), JsValue::NULL);
    let manual_tree = command_render(
        &manual,
        object(&[
            (
                "node",
                object(&[(
                    "data",
                    object(&[("command", command.into()), ("compaction", JsValue::NULL)]).into(),
                )])
                .into(),
            ),
            ("t", translate.clone().into()),
        ])
        .as_ref(),
    );
    assert!(property(&property(&child(&manual_tree, 0), "props"), "compaction").is_undefined());

    command_reset_hooks();
    let unavailable = object(&[
        ("summary", JsValue::NULL),
        ("shadowedItemCount", JsValue::NULL),
        ("shadowedTokenCount", JsValue::NULL),
    ]);
    let unavailable_tree = command_render(
        &item,
        object(&[("node", unavailable.into()), ("t", translate.into())]).as_ref(),
    );
    let unavailable_button = child(&unavailable_tree, 0);
    assert_eq!(
        property(&property(&unavailable_button, "props"), "disabled").as_bool(),
        Some(true)
    );
    assert!(property(&property(&unavailable_button, "props"), "aria-expanded").is_undefined());
    assert_eq!(
        child(&child(&unavailable_button, 3), 0)
            .as_string()
            .as_deref(),
        Some("压缩摘要不可用")
    );
}
