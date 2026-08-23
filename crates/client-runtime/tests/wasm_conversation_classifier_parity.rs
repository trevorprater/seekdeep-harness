//! Live JavaScript Assistant classifier field and identity parity.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Object, Reflect};
use seekdeep_client_runtime::{to_assistant_block_js, to_assistant_blocks_js};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::*;

fn set(object: &Object, key: &str, value: &JsValue) {
    assert!(Reflect::set(object, &JsValue::from_str(key), value).unwrap());
}

fn get(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn block(block_type: &str) -> Object {
    let block = Object::new();
    set(&block, "type", &JsValue::from_str(block_type));
    block
}

#[wasm_bindgen_test]
fn known_shapes_map_fields_and_unknown_shape_keeps_raw_object_identity() {
    let text = block("text");
    set(&text, "text", &JsValue::from_str("正文"));
    let tool = block("tool-call");
    set(&tool, "id", &JsValue::from_str("c1"));
    set(&tool, "name", &JsValue::from_str("echo"));
    set(&tool, "arguments", &JsValue::from_str("{}"));
    let future = block("future");
    let input = Array::new();
    input.push(&text);
    input.push(&tool);
    input.push(&future);
    let output = to_assistant_blocks_js(input).unwrap();
    assert_eq!(
        get(&output.get(0), "kind").as_string().as_deref(),
        Some("text")
    );
    assert_eq!(
        get(&output.get(0), "text").as_string().as_deref(),
        Some("正文")
    );
    assert_eq!(
        get(&output.get(1), "callId").as_string().as_deref(),
        Some("c1")
    );
    assert_eq!(
        get(&output.get(1), "argsRaw").as_string().as_deref(),
        Some("{}")
    );
    assert!(Object::is(&get(&output.get(2), "block"), &future));
    assert_eq!(
        get(&to_assistant_block_js(text.into()).unwrap(), "kind")
            .as_string()
            .as_deref(),
        Some("text")
    );
}
