//! Live WASM coverage for one-node subscription and keyed chat dispatch.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_conversation::{
    chat_node_seat_component, configure_client_ui_conversation_chat_seat,
};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let hooks = []
let cursor = 0
let node
let slotCalls = []
const JsonBlock = props => ({ kind: 'JsonBlock', props, children: [] })
function sameDeps(left, right) { return left?.length === right?.length && left.every((value, index) => Object.is(value, right[index])) }
export function installSeatBench() {
  hooks = []; cursor = 0; node = undefined; slotCalls = []
  globalThis.document = { head: { appendChild() {} }, createElement() { return { setAttribute() {} } }, querySelector() { return null } }
  const React = { createElement(kind, props, ...children) { return { kind, props: props ?? {}, children } }, memo(component) { return component }, useMemo(factory, deps) { const index = cursor++; if (!(index in hooks) || !sameDeps(hooks[index].deps, deps)) hooks[index] = { value: factory(), deps: [...deps] }; return hooks[index].value } }
  return { React, uiPrimitives: { JsonBlock } }
}
export function seatSetNode(value) { node = value }
export function seatUseSession(selector) { return selector({ chat: { nodes: new Map(node === undefined ? [] : [[node.key, node]]) } }) }
export function seatRenderSlot(name, owner, options) { slotCalls.push({ name, owner, options }); return { kind: 'slot', props: {}, children: [] } }
export function seatCalls() { return slotCalls }
export function seatRender(component, props) { cursor = 0; return component(props) }
export function seatObject(entries) { return Object.fromEntries(entries) }
export function seatTranslate(key, vars) { if (key === 'message.unknownSurface') return '未知:' + vars.type; if (key === 'json.truncated') return '截断:' + vars.total; return key }
"#)]
extern "C" {
    fn installSeatBench() -> JsValue;
    fn seatSetNode(value: &JsValue);
    fn seatUseSession(selector: &Function) -> JsValue;
    fn seatRenderSlot(name: &str, owner: &JsValue, options: &JsValue) -> JsValue;
    fn seatCalls() -> Array;
    fn seatRender(component: &JsValue, props: &JsValue) -> JsValue;
    fn seatObject(entries: &Array) -> JsValue;
    fn seatTranslate(key: &str, vars: &JsValue) -> String;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}
fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    seatObject(&array).unchecked_into()
}
fn function0() -> JsValue {
    Closure::wrap(Box::new(move || {}) as Box<dyn FnMut()>).into_js_value()
}

fn setup() -> JsValue {
    let bench = installSeatBench();
    configure_client_ui_conversation_chat_seat(
        property(&bench, "React"),
        property(&bench, "uiPrimitives"),
    )
    .unwrap();
    chat_node_seat_component().unwrap()
}

fn props(node_key: &str) -> Object {
    let use_session = Closure::wrap(
        Box::new(move |selector: Function| seatUseSession(&selector))
            as Box<dyn FnMut(Function) -> JsValue>,
    );
    let render_slot = Closure::wrap(Box::new(
        move |name: String, owner: JsValue, options: JsValue| {
            seatRenderSlot(&name, &owner, &options)
        },
    ) as Box<dyn FnMut(String, JsValue, JsValue) -> JsValue>);
    let translate =
        Closure::wrap(
            Box::new(move |key: String, vars: JsValue| seatTranslate(&key, &vars))
                as Box<dyn FnMut(String, JsValue) -> String>,
        );
    object(&[
        ("nodeKey", JsValue::from_str(node_key)),
        ("selectedCallId", JsValue::from_str("call")),
        ("cwd", JsValue::from_str("/w")),
        ("openFile", function0()),
        ("inspectCall", function0()),
        ("forkAt", function0()),
        ("loadImage", function0()),
        ("fileMentions", function0()),
        ("useSession", use_session.into_js_value()),
        ("renderSlot", render_slot.into_js_value()),
        ("t", translate.into_js_value()),
    ])
}

#[wasm_bindgen_test]
fn missing_node_renders_nothing() {
    let component = setup();
    assert!(seatRender(&component, props("missing").as_ref()).is_null());
    assert_eq!(seatCalls().length(), 0);
}

#[wasm_bindgen_test]
fn keyed_dispatch_threads_owner_options_anchor_and_fallback() {
    let component = setup();
    let node = object(&[
        ("key", JsValue::from_str("node:1")),
        ("kind", JsValue::from_str("assistant-step")),
        ("data", object(&[("value", JsValue::from_f64(1.0))]).into()),
    ]);
    seatSetNode(node.as_ref());
    let tree = seatRender(&component, props("node:1").as_ref());
    assert_eq!(
        property(&property(&tree, "props"), "data-chat-anchor-key")
            .as_string()
            .as_deref(),
        Some("node:1")
    );
    assert_eq!(
        property(&property(&tree, "props"), "data-chat-flow-kind")
            .as_string()
            .as_deref(),
        Some("assistant-step")
    );
    let call = seatCalls().get(0);
    assert_eq!(
        property(&call, "name").as_string().as_deref(),
        Some("conversation.chat.node")
    );
    let owner = property(&call, "owner");
    assert!(Object::is(&property(&owner, "node"), node.as_ref()));
    assert_eq!(property(&owner, "cwd").as_string().as_deref(), Some("/w"));
    let options = property(&call, "options");
    assert_eq!(
        property(&options, "entryKey").as_string().as_deref(),
        Some("assistant-step")
    );
    assert_eq!(
        property(&options, "hookContext").as_string().as_deref(),
        Some("node:1")
    );
    let fallback = property(&options, "fallback");
    assert_eq!(
        property(&property(&fallback, "props"), "label")
            .as_string()
            .as_deref(),
        Some("未知:assistant-step")
    );
    let footer = property(&property(&fallback, "props"), "truncatedLabel")
        .dyn_into::<Function>()
        .unwrap();
    assert_eq!(
        footer
            .call1(&JsValue::UNDEFINED, &JsValue::from_f64(42.0))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("截断:42")
    );
}

#[wasm_bindgen_test]
fn owner_identity_is_stable_until_a_dependency_changes() {
    let component = setup();
    let node = object(&[
        ("key", JsValue::from_str("node:1")),
        ("kind", JsValue::from_str("unknown")),
        ("data", Object::new().into()),
    ]);
    seatSetNode(node.as_ref());
    let props = props("node:1");
    let _ = seatRender(&component, props.as_ref());
    let first = property(&seatCalls().get(0), "owner");
    let _ = seatRender(&component, props.as_ref());
    let second = property(&seatCalls().get(1), "owner");
    assert!(!Object::is(&first, &second));
    assert!(Object::is(
        &property(&first, "node"),
        &property(&second, "node")
    ));
}
