//! Native Rust Conversation Definition to browser registry bridge parity.

#![cfg(target_arch = "wasm32")]

use std::rc::Rc;

use js_sys::{Function, Reflect};
use seekdeep_client_runtime::{
    AssemblerNodeDefinition, ConversationLocationData, ConversationLocationDataScope,
    native_conversation_node_definition_to_js,
};
use serde_json::json;
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function nativeDefinitionContext() {
  return {
    key: 'fixture:1', kind: 'fixture', id: '1', matches: [],
    start: undefined, state: null, current: new Map(),
  }
}
"#)]
extern "C" {
    fn nativeDefinitionContext() -> JsValue;
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
                    value: Rc::new(json!({ "value": "turn" })),
                },
                ConversationLocationDataScope::Step => ConversationLocationData::Step {
                    turn: 7,
                    step: Some(9),
                    key: "step-fixture".to_owned(),
                    value: Rc::new(json!({ "value": "step" })),
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
