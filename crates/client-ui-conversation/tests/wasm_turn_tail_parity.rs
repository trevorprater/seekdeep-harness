//! Live WASM coverage for finalized-turn tail composition.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_conversation::{
    configure_client_ui_conversation_message_actions, configure_client_ui_conversation_turn_tail,
    message_icon_actions_component, turn_tail_node_view_component,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let turnKeys = []
let chainReturn = null
let slotReturn = null
let chainCalls = []
let slotCalls = []
let forked = []
let selectorCalls = 0
export function installTurnTailBench() {
  turnKeys = []; chainReturn = null; slotReturn = null; chainCalls = []; slotCalls = []; forked = []; selectorCalls = 0
  globalThis.document = {
    head: { appendChild() {} }, createElement() { return { setAttribute() {} } }, querySelector() { return null },
  }
  const passthrough = callback => callback
  const React = {
    Fragment: 'Fragment', createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } },
    memo: passthrough, useCallback: passthrough, useEffect() {}, useId() { return 'unused' },
    useRef(value) { return { current: value } }, useState(value) { return [typeof value === 'function' ? value() : value, () => {}] },
  }
  const uiPrimitives = {
    Tooltip: 'Tooltip', IconBranchOutline16: 'IconBranchOutline16', IconCheckOutline16: 'IconCheckOutline16',
    IconCopyOutline16: 'IconCopyOutline16', writeClipboard() { return Promise.resolve(true) },
  }
  return { React, uiPrimitives }
}
export function tailObject(entries) { return Object.fromEntries(entries) }
export function tailSetTurnKeys(keys) { turnKeys = [...keys] }
export function tailSetChainReturn(value) { chainReturn = value }
export function tailSetSlotReturn(value) { slotReturn = value }
export function makeTailUseSession() { return selector => { selectorCalls += 1; return selector({ chat: { locations: { getTurn: () => turnKeys } } }) } }
export function makeTailRenderChain() { return (name, owner) => { chainCalls.push({ name, owner }); return chainReturn } }
export function makeTailRenderSlot() { return (name, owner) => { slotCalls.push({ name, owner }); return slotReturn } }
export function makeTailForkAt() { return seq => { forked.push(seq) } }
export function tailRender(component, props) { return component(props) }
export function tailChainCalls() { return chainCalls }
export function tailSlotCalls() { return slotCalls }
export function tailForked() { return forked }
export function tailSelectorCalls() { return selectorCalls }
export function tailMarker(name) { return { kind: name, props: {}, children: [] } }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installTurnTailBench)]
    fn install_turn_tail_bench() -> JsValue;
    #[wasm_bindgen(js_name = tailObject)]
    fn tail_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = tailSetTurnKeys)]
    fn tail_set_turn_keys(keys: &Array);
    #[wasm_bindgen(js_name = tailSetChainReturn)]
    fn tail_set_chain_return(value: &JsValue);
    #[wasm_bindgen(js_name = tailSetSlotReturn)]
    fn tail_set_slot_return(value: &JsValue);
    #[wasm_bindgen(js_name = makeTailUseSession)]
    fn make_tail_use_session() -> Function;
    #[wasm_bindgen(js_name = makeTailRenderChain)]
    fn make_tail_render_chain() -> Function;
    #[wasm_bindgen(js_name = makeTailRenderSlot)]
    fn make_tail_render_slot() -> Function;
    #[wasm_bindgen(js_name = makeTailForkAt)]
    fn make_tail_fork_at() -> Function;
    #[wasm_bindgen(js_name = tailRender)]
    fn tail_render(component: &JsValue, props: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = tailChainCalls)]
    fn tail_chain_calls() -> Array;
    #[wasm_bindgen(js_name = tailSlotCalls)]
    fn tail_slot_calls() -> Array;
    #[wasm_bindgen(js_name = tailForked)]
    fn tail_forked() -> Array;
    #[wasm_bindgen(js_name = tailSelectorCalls)]
    fn tail_selector_calls() -> u32;
    #[wasm_bindgen(js_name = tailMarker)]
    fn tail_marker(name: &str) -> JsValue;
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
    tail_object(&array).unchecked_into()
}

fn setup() -> (JsValue, JsValue) {
    let bench = install_turn_tail_bench();
    let react = property(&bench, "React");
    configure_client_ui_conversation_message_actions(
        react.clone(),
        property(&bench, "uiPrimitives"),
    )
    .unwrap();
    let message_actions = message_icon_actions_component().unwrap();
    configure_client_ui_conversation_turn_tail(react).unwrap();
    (turn_tail_node_view_component().unwrap(), message_actions)
}

fn props(node: Object) -> Object {
    let noop = Function::new_no_args("");
    object(&[
        ("node", node.into()),
        ("openFile", noop.clone().into()),
        ("forkAt", make_tail_fork_at().into()),
        ("renderSlot", make_tail_render_slot().into()),
        ("renderSlotChain", make_tail_render_chain().into()),
        ("t", noop.into()),
        ("useSession", make_tail_use_session().into()),
    ])
}

fn location(kind: &str, turn: Option<&Object>) -> Object {
    let mut entries = vec![("kind", JsValue::from_str(kind))];
    if let Some(turn) = turn {
        entries.push(("turn", turn.clone().into()));
    }
    object(&entries)
}

fn node(location: Object, data: Object) -> Object {
    object(&[
        ("key", JsValue::from_str("tail:1")),
        ("location", location.into()),
        ("data", data.into()),
    ])
}

fn empty_keys() -> Array {
    Array::new()
}

#[wasm_bindgen_test]
fn unsupported_location_returns_null_after_the_one_turn_selector() {
    let (component, _) = setup();
    tail_set_turn_keys(&empty_keys());
    let data = object(&[
        ("turn", JsValue::from_f64(1.0)),
        ("seq", JsValue::from_f64(8.0)),
        ("closing", JsValue::NULL),
    ]);
    let tree = tail_render(
        &component,
        props(node(location("event", None), data)).as_ref(),
    );
    assert!(tree.is_null());
    assert_eq!(tail_selector_calls(), 1);
    assert_eq!(tail_chain_calls().length(), 0);
}

#[wasm_bindgen_test]
fn open_turn_tail_uses_data_seq_and_wraps_only_a_non_null_chain_result() {
    let (component, _) = setup();
    let turn = object(&[]);
    let data = object(&[
        ("turn", JsValue::from_f64(1.0)),
        ("seq", JsValue::from_f64(8.0)),
        ("closing", JsValue::NULL),
    ]);
    let tail_node = node(location("turn", Some(&turn)), data);
    assert!(tail_render(&component, props(tail_node.clone()).as_ref()).is_null());
    let call = tail_chain_calls().get(0);
    assert_eq!(
        property(&call, "name").as_string().as_deref(),
        Some("conversation.chat.turnTail")
    );
    assert!(Object::is(
        &property(&property(&call, "owner"), "turn"),
        turn.as_ref()
    ));
    assert_eq!(
        property(&property(&call, "owner"), "seq").as_f64(),
        Some(8.0)
    );

    let marker = tail_marker("tail-content");
    tail_set_chain_return(&marker);
    let wrapped = tail_render(&component, props(tail_node).as_ref());
    assert_eq!(
        property(&property(&wrapped, "props"), "className")
            .as_string()
            .as_deref(),
        Some("seekdeep-conversation-turnTail-root")
    );
    assert!(property(&property(&wrapped, "props"), "data-turn-tail").is_undefined());
    assert!(Object::is(&child(&wrapped, 0), &marker));
}

#[wasm_bindgen_test]
#[allow(clippy::too_many_lines)] // One finalized-turn owner and child-prop matrix stays together.
fn closed_turn_composes_tail_metrics_actions_and_fork_currency() {
    let (component, message_actions) = setup();
    tail_set_turn_keys(&Array::of2(
        &JsValue::from_str("earlier"),
        &JsValue::from_str("tail:1"),
    ));
    let tail = tail_marker("tail-content");
    let assistant_action = tail_marker("assistant-action");
    tail_set_chain_return(&tail);
    tail_set_slot_return(&assistant_action);
    let turn = object(&[
        (
            "start",
            object(&[("time", JsValue::from_f64(1_000.0))]).into(),
        ),
        (
            "end",
            object(&[("time", JsValue::from_f64(20_000.0))]).into(),
        ),
    ]);
    let final_node = object(&[
        ("seq", JsValue::from_f64(9.0)),
        ("messageId", JsValue::from_str("message:9")),
    ]);
    let blocks = Array::of3(
        &object(&[
            ("kind", JsValue::from_str("text")),
            ("text", JsValue::from_str("answer")),
        ]),
        &object(&[
            ("kind", JsValue::from_str("reasoning")),
            ("text", JsValue::from_str("hidden")),
        ]),
        &object(&[
            ("kind", JsValue::from_str("text")),
            ("text", JsValue::from_str(" end")),
        ]),
    );
    let closing = object(&[
        ("finalNode", final_node.into()),
        ("blocks", blocks.into()),
        ("time", JsValue::from_f64(20_000.0)),
    ]);
    let data = object(&[
        ("turn", JsValue::from_f64(1.0)),
        ("seq", JsValue::from_f64(8.0)),
        ("closing", closing.into()),
        ("ttftMs", JsValue::from_f64(1_200.0)),
        ("tokensPerSecond", JsValue::from_f64(20.0)),
        ("branchUnavailable", JsValue::FALSE),
    ]);
    let tree = tail_render(
        &component,
        props(node(location("step", Some(&turn)), data)).as_ref(),
    );
    assert_eq!(
        property(&property(&tree, "props"), "data-turn-tail").as_f64(),
        Some(1.0)
    );
    assert_eq!(
        property(&property(&tree, "props"), "data-time-hover-root").as_bool(),
        Some(true)
    );
    assert!(Object::is(&child(&tree, 0), &tail));
    let actions = child(&tree, 1);
    assert!(Object::is(&property(&actions, "kind"), &message_actions));
    let action_props = property(&actions, "props");
    assert_eq!(
        property(&action_props, "text").as_string().as_deref(),
        Some("answer end")
    );
    assert_eq!(property(&action_props, "runMs").as_f64(), Some(19_000.0));
    assert_eq!(property(&action_props, "ttftMs").as_f64(), Some(1_200.0));
    assert_eq!(
        property(&action_props, "tokensPerSecond").as_f64(),
        Some(20.0)
    );
    assert_eq!(
        property(&action_props, "branchUnavailable").as_bool(),
        Some(false)
    );
    assert!(Object::is(
        &property(&action_props, "extraActions"),
        &assistant_action
    ));
    property(&action_props, "onBranch")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert_eq!(tail_forked().get(0).as_f64(), Some(9.0));
    let slot = tail_slot_calls().get(0);
    assert_eq!(
        property(&slot, "name").as_string().as_deref(),
        Some("conversation.chat.assistant-actions")
    );
    assert_eq!(
        property(&property(&slot, "owner"), "messageId")
            .as_string()
            .as_deref(),
        Some("message:9")
    );
}

#[wasm_bindgen_test]
fn later_node_disables_branch_and_message_less_partial_skips_action_slot() {
    let (component, _) = setup();
    tail_set_turn_keys(&Array::of2(
        &JsValue::from_str("tail:1"),
        &JsValue::from_str("later"),
    ));
    let turn = object(&[]);
    let final_node = object(&[("seq", JsValue::from_f64(11.0))]);
    let closing = object(&[
        ("finalNode", final_node.into()),
        ("blocks", Array::new().into()),
        ("time", JsValue::from_f64(21_000.0)),
    ]);
    let data = object(&[
        ("turn", JsValue::from_f64(2.0)),
        ("seq", JsValue::from_f64(10.0)),
        ("closing", closing.into()),
        ("branchUnavailable", JsValue::FALSE),
    ]);
    let tree = tail_render(
        &component,
        props(node(location("turn", Some(&turn)), data)).as_ref(),
    );
    let action_props = property(&child(&tree, 1), "props");
    assert_eq!(
        property(&action_props, "branchUnavailable").as_bool(),
        Some(true)
    );
    assert!(property(&action_props, "runMs").is_undefined());
    assert!(property(&action_props, "extraActions").is_null());
    assert_eq!(tail_slot_calls().length(), 0);
}
