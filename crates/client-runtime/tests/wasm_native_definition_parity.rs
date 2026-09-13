//! Native Rust Conversation Definition to browser registry bridge parity.

#![cfg(target_arch = "wasm32")]

use std::rc::Rc;

use js_sys::{Function, Object, Reflect};
use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ConversationLocationData, ConversationLocationDataScope,
    native_conversation_node_definition_to_js,
};
use serde_json::{Value, json};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function nativeDefinitionContext() {
  return {
    key: 'fixture:1', kind: 'fixture', id: '1', matches: [],
    start: undefined, state: null, current: new Map(),
  }
}
export function nativeMatch() {
  return {
    event: { seq: 1, time: 1, type: 'fixture/start', data: {} },
    role: 'start',
    location: { kind: 'session' },
  }
}
export function followerContext(state) {
  return {
    key: 'follower:1', kind: 'follower', id: '1', matches: [],
    start: undefined, state, current: new Map(),
  }
}
export function readerWith(previous) {
  return { previous: () => previous }
}
export function previousContext(state) {
  return { key: 'fixture:1', kind: 'fixture', id: '1', startSeq: 1, state, matches: [] }
}
export function chunkMatches(count) {
  const matches = [];
  for (let seq = 2; seq < 2 + count; seq++) {
    matches.push({ event: { seq, time: seq, type: 'fixture/chunk', data: {} }, role: 'update', location: { kind: 'session' } });
  }
  return matches;
}
export function contextWith(state, matches) {
  return { key: 'fixture:1', kind: 'fixture', id: '1', matches, start: matches[0], state, current: new Map() };
}
export function withStart(matches) {
  return [{ event: { seq: 1, time: 1, type: 'fixture/start', data: {} }, role: 'start', location: { kind: 'session' } }, ...matches];
}
export function reversed(matches) {
  return [...matches].reverse();
}
"#)]
extern "C" {
    fn nativeDefinitionContext() -> JsValue;
    fn nativeMatch() -> JsValue;
    fn followerContext(state: &JsValue) -> JsValue;
    fn readerWith(previous: &JsValue) -> JsValue;
    fn previousContext(state: &JsValue) -> JsValue;
    fn chunkMatches(count: u32) -> JsValue;
    fn contextWith(state: &JsValue, matches: &JsValue) -> JsValue;
    fn withStart(matches: &JsValue) -> JsValue;
    fn reversed(matches: &JsValue) -> JsValue;
}

fn definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: "fixture".to_owned(),
        target: None,
        match_event: Rc::new(|_| Ok(None)),
        start: Rc::new(|_, _, _| Ok(None)),
        update: Rc::new(|_, _| Ok(None)),
        publication: None,
        build_location_data: Some(Rc::new(|_, scope| {
            let data = match scope {
                ConversationLocationDataScope::Turn => ConversationLocationData::Turn {
                    turn: 7,
                    key: "turn-fixture".to_owned(),
                    value: Rc::new(json!({ "value": "turn" }).into()),
                },
                ConversationLocationDataScope::Step => ConversationLocationData::Step {
                    turn: 7,
                    step: Some(9),
                    key: "step-fixture".to_owned(),
                    value: Rc::new(json!({ "value": "step" }).into()),
                },
            };
            Ok(Some(Rc::new(data)))
        })),
        build_view_node: None,
    }
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

#[wasm_bindgen_test]
fn build_location_data_exports_both_scopes_and_rejects_unknown_values() {
    let wrapped = native_conversation_node_definition_to_js(definition()).unwrap();
    let build = property(&wrapped, "buildLocationData")
        .dyn_into::<Function>()
        .unwrap();
    let context = nativeDefinitionContext();

    let turn = build
        .call2(&JsValue::UNDEFINED, &context, &JsValue::from_str("turn"))
        .unwrap();
    assert_eq!(property(&turn, "kind").as_string().as_deref(), Some("turn"));
    assert_eq!(property(&turn, "turn").as_f64(), Some(7.0));
    assert_eq!(
        property(&turn, "key").as_string().as_deref(),
        Some("turn-fixture")
    );
    assert_eq!(
        property(&property(&turn, "value"), "value")
            .as_string()
            .as_deref(),
        Some("turn")
    );

    let step = build
        .call2(&JsValue::UNDEFINED, &context, &JsValue::from_str("step"))
        .unwrap();
    assert_eq!(property(&step, "kind").as_string().as_deref(), Some("step"));
    assert_eq!(property(&step, "turn").as_f64(), Some(7.0));
    assert_eq!(property(&step, "step").as_f64(), Some(9.0));
    assert_eq!(
        property(&step, "key").as_string().as_deref(),
        Some("step-fixture")
    );
    assert_eq!(
        property(&property(&step, "value"), "value")
            .as_string()
            .as_deref(),
        Some("step")
    );

    let error = build
        .call2(&JsValue::UNDEFINED, &context, &JsValue::from_str("session"))
        .unwrap_err();
    assert!(
        property(&error, "message")
            .as_string()
            .is_some_and(|message| message.contains("scope \"session\" is invalid"))
    );
}

/// A Definition whose State starts as a fixed object and whose Node exposes the State it holds.
fn stateful_definition(kind: &str, from_previous: Option<&'static str>) -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: kind.to_owned(),
        target: Some("chat".to_owned()),
        match_event: Rc::new(|_| Ok(None)),
        start: Rc::new(move |_, _, reader| {
            Ok(Some(match from_previous {
                Some(previous_kind) => reader.previous(previous_kind).map_or_else(
                    || Rc::new(json!({ "phase": "orphan" }).into()),
                    |previous| previous.state.clone(),
                ),
                None => Rc::new(json!({ "phase": "started", "n": 1 }).into()),
            }))
        }),
        update: Rc::new(|context, _| Ok(context.state.clone())),
        publication: None,
        build_location_data: None,
        build_view_node: Some(Rc::new(|context| {
            Ok(Some(Rc::new(
                seekdeep_client_runtime::ConversationViewNode {
                    key: context.key.clone(),
                    kind: context.kind.clone(),
                    id: context.id.clone(),
                    target: "chat".to_owned(),
                    data: context
                        .state
                        .clone()
                        .unwrap_or_else(|| Rc::new(Value::Null.into())),
                    placement: None,
                    chat: None,
                },
            )))
        })),
    }
}

fn method(value: &JsValue, name: &str) -> Function {
    property(value, name).dyn_into::<Function>().unwrap()
}

#[wasm_bindgen_test]
fn native_state_handles_resolve_for_the_owner_and_for_a_predecessor_reader() {
    let owner =
        native_conversation_node_definition_to_js(stateful_definition("fixture", None)).unwrap();
    let follower =
        native_conversation_node_definition_to_js(stateful_definition("follower", Some("fixture")))
            .unwrap();
    let handle = method(&owner, "start")
        .call3(
            &JsValue::UNDEFINED,
            &nativeDefinitionContext(),
            &nativeMatch(),
            &readerWith(&JsValue::UNDEFINED),
        )
        .unwrap();
    assert!(handle.is_object());
    assert_eq!(
        property(&handle, "key").as_string().as_deref(),
        Some("fixture:1")
    );
    // The engine never reads a State: it hands the handle back, and the owner recovers the Rc.
    let owner_node = method(&owner, "buildViewNode")
        .call1(&JsValue::UNDEFINED, &{
            let context = nativeDefinitionContext();
            Reflect::set(&context, &JsValue::from_str("state"), &handle).unwrap();
            context
        })
        .unwrap();
    assert_eq!(
        property(&property(&owner_node, "data"), "phase")
            .as_string()
            .as_deref(),
        Some("started")
    );
    // A predecessor's State reaches a later Definition through the same handle.
    let follower_handle = method(&follower, "start")
        .call3(
            &JsValue::UNDEFINED,
            &followerContext(&JsValue::NULL),
            &nativeMatch(),
            &readerWith(&previousContext(&handle)),
        )
        .unwrap();
    let follower_node = method(&follower, "buildViewNode")
        .call1(&JsValue::UNDEFINED, &followerContext(&follower_handle))
        .unwrap();
    let data = property(&follower_node, "data");
    assert_eq!(
        property(&data, "phase").as_string().as_deref(),
        Some("started")
    );
    assert_eq!(property(&data, "n").as_f64(), Some(1.0));
    // A handle this module never registered is refused instead of being read as JSON.
    let foreign = Object::new();
    Reflect::set(
        &foreign,
        &JsValue::from_str("$seekdeepNativeState"),
        &JsValue::from_f64(999.0),
    )
    .unwrap();
    Reflect::set(
        &foreign,
        &JsValue::from_str("key"),
        &JsValue::from_str("fixture:9"),
    )
    .unwrap();
    let error = method(&follower, "start")
        .call3(
            &JsValue::UNDEFINED,
            &followerContext(&JsValue::NULL),
            &nativeMatch(),
            &readerWith(&previousContext(&foreign)),
        )
        .unwrap_err();
    assert!(
        property(&error, "message")
            .as_string()
            .is_some_and(|message| message.contains("did not register"))
    );
}

/// A Definition that counts its updates and records the Match collection length each saw.
fn counting_definition() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: "fixture".to_owned(),
        target: Some("chat".to_owned()),
        match_event: Rc::new(|_| Ok(None)),
        start: Rc::new(|_, _, _| Ok(Some(Rc::new(json!({ "count": 0, "seen": [] }).into())))),
        update: Rc::new(|context, _| {
            let mut state = context
                .state
                .as_deref()
                .cloned()
                .unwrap_or(Value::Null.into());
            state
                .insert(
                    "count",
                    json!(state["count"].as_u64().unwrap_or(0) + 1).into(),
                )
                .unwrap();
            let mut seen = state["seen"].clone();
            seen.push(json!(context.matches.borrow().len()).into())
                .unwrap();
            state.insert("seen", seen).unwrap();
            Ok(Some(Rc::new(state)))
        }),
        publication: None,
        build_location_data: None,
        build_view_node: Some(Rc::new(|context| {
            Ok(Some(Rc::new(
                seekdeep_client_runtime::ConversationViewNode {
                    key: context.key.clone(),
                    kind: context.kind.clone(),
                    id: context.id.clone(),
                    target: "chat".to_owned(),
                    data: context
                        .state
                        .clone()
                        .unwrap_or_else(|| Rc::new(Value::Null.into())),
                    placement: None,
                    chat: None,
                },
            )))
        })),
    }
}

#[wasm_bindgen_test]
fn update_many_folds_each_match_against_the_collection_prefix_before_it() {
    let wrapped = native_conversation_node_definition_to_js(counting_definition()).unwrap();
    let chunks = chunkMatches(3);
    let matches = withStart(&chunks);
    let started = method(&wrapped, "start")
        .call3(
            &JsValue::UNDEFINED,
            &contextWith(&JsValue::NULL, &matches),
            &nativeMatch(),
            &readerWith(&JsValue::UNDEFINED),
        )
        .unwrap();
    let folded = method(&wrapped, "updateMany")
        .call2(
            &JsValue::UNDEFINED,
            &contextWith(&started, &matches),
            &chunks,
        )
        .unwrap();
    let node = method(&wrapped, "buildViewNode")
        .call1(&JsValue::UNDEFINED, &contextWith(&folded, &matches))
        .unwrap();
    let data = property(&node, "data");
    assert_eq!(property(&data, "count").as_f64(), Some(3.0));
    assert_eq!(
        js_sys::JSON::stringify(&property(&data, "seen"))
            .unwrap()
            .as_string()
            .as_deref(),
        Some("[2,3,4]"),
        "each step saw the collection as it stood before it"
    );
    // A run that is not the collection's tail is refused before any step folds (the Context
    // carries the latest handle, as the assembler always does).
    let error = method(&wrapped, "updateMany")
        .call2(
            &JsValue::UNDEFINED,
            &contextWith(&folded, &matches),
            &reversed(&chunks),
        )
        .unwrap_err();
    let message = property(&error, "message")
        .as_string()
        .unwrap_or_else(|| format!("{error:?}"));
    assert!(
        message.contains("must end the Match collection"),
        "unexpected refusal: {message}"
    );
}
