//! Structural JSON ⇄ JavaScript bridge: agrees with the JSON path and retains streamed text.

#![cfg(target_arch = "wasm32")]

use js_sys::{JSON, Object, Reflect};
use seekdeep_client_runtime::{js_to_value, js_to_value_reusing, value_to_js, value_to_js_reusing};
use serde_json::{Value, json};
use wasm_bindgen::{JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function deserializerSample() {
  return {
    n: 1, f: 1.5, neg: -3, big: 2 ** 53, zero: 0, s: 'x', u: undefined,
    arr: [null, undefined, true, 'y'],
    nested: { m: new Map([['k', 1]]), empty: {} },
  }
}
export function extendText(previous, suffix) {
  return { ...previous, text: previous.text + suffix, list: [...previous.list, 3] }
}
"#)]
extern "C" {
    fn deserializerSample() -> JsValue;
    fn extendText(previous: &JsValue, suffix: &str) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn stringify(value: &JsValue) -> String {
    JSON::stringify(value).unwrap().into()
}

fn sample() -> Value {
    json!({
        "n": 1, "neg": -3, "f": 1.5, "big": 9_007_199_254_740_993_u64, "flag": true, "none": null,
        "s": "héllo ✓", "list": [1, "two", [3.25], {"four": 4}], "empty": {}, "nested": {"z": [], "a": 0},
    })
}

fn long_text() -> String {
    "streamed reasoning text ".repeat(64)
}

#[wasm_bindgen_test]
fn structural_conversion_agrees_with_the_json_path() {
    let value = sample();
    let structural = value_to_js(&value).unwrap();
    let json = JSON::parse(&serde_json::to_string(&value).unwrap()).unwrap();
    assert_eq!(stringify(&structural), stringify(&json));
    assert_eq!(
        js_to_value(&structural).unwrap(),
        js_to_value(&json).unwrap()
    );
}

#[wasm_bindgen_test]
fn reusing_conversion_keeps_unchanged_subtrees_and_grows_retained_text() {
    let text = long_text();
    let previous = json!({"a": {"x": 1}, "text": text, "short": "old", "list": [1, 2]});
    let previous_js = value_to_js(&previous).unwrap();
    let next = json!({
        "a": {"x": 1},
        "text": format!("{text}more"),
        "short": "new",
        "list": [1, 2, 3],
        "added": {"y": 2},
    });
    let next_js = value_to_js_reusing(&previous, &previous_js, &next).unwrap();
    assert!(Object::is(
        &property(&next_js, "a"),
        &property(&previous_js, "a")
    ));
    assert_eq!(
        property(&next_js, "text").as_string().as_deref(),
        Some(format!("{text}more").as_str())
    );
    assert_eq!(
        property(&next_js, "short").as_string().as_deref(),
        Some("new")
    );
    assert_eq!(js_to_value(&next_js).unwrap(), next);
    assert!(Object::is(
        &value_to_js_reusing(&next, &next_js, &next).unwrap(),
        &next_js
    ));
}

#[wasm_bindgen_test]
fn parsing_matches_the_deserializer_semantics() {
    let parsed = js_to_value(&deserializerSample()).unwrap();
    assert_eq!(
        parsed,
        json!({
            "n": 1, "f": 1.5, "neg": -3, "big": 9_007_199_254_740_992.0_f64, "zero": 0, "s": "x",
            "u": null, "arr": [null, null, true, "y"], "nested": {"m": {"k": 1}, "empty": {}},
        })
    );
    assert_eq!(
        serde_json::to_string(&parsed).unwrap(),
        r#"{"n":1,"f":1.5,"neg":-3,"big":9007199254740992.0,"zero":0,"s":"x","u":null,"arr":[null,null,true,"y"],"nested":{"m":{"k":1},"empty":{}}}"#
    );
}

#[wasm_bindgen_test]
fn reusing_parse_clones_identical_values_and_extends_retained_text() {
    let text = long_text();
    let previous = json!({"a": {"x": 1}, "text": text, "list": [1, 2]});
    let previous_js = value_to_js(&previous).unwrap();
    assert_eq!(
        js_to_value_reusing(&previous, &previous_js, &previous_js).unwrap(),
        previous
    );
    let next_js = extendText(&previous_js, " and more");
    let next = js_to_value_reusing(&previous, &previous_js, &next_js).unwrap();
    assert_eq!(
        next,
        json!({"a": {"x": 1}, "text": format!("{text} and more"), "list": [1, 2, 3]})
    );
    let replaced = value_to_js(&json!({"a": {"x": 1}, "text": "different", "list": []})).unwrap();
    assert_eq!(
        js_to_value_reusing(&next, &next_js, &replaced).unwrap(),
        json!({"a": {"x": 1}, "text": "different", "list": []})
    );
}
