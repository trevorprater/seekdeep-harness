//! Live WASM coverage for keyed and nested Tool call readers.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Object, Reflect};
use seekdeep_client_ui_conversation::{find_tool_call_browser, root_tool_call_browser};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function readerObject(entries) { return Object.fromEntries(entries) }
export function readerEntry(key, value) { return [key, value] }
export function readerSnapshot(entries) { return { chat: { nodes: new Map(entries) } } }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = readerObject)]
    fn reader_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = readerEntry)]
    fn reader_entry(key: &str, value: &JsValue) -> JsValue;
    #[wasm_bindgen(js_name = readerSnapshot)]
    fn reader_snapshot(entries: &Array) -> JsValue;
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    reader_object(&array).unchecked_into()
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

fn node(kind: &str, root: JsValue) -> Object {
    object(&[
        ("kind", JsValue::from_str(kind)),
        ("data", object(&[("root", root)]).into()),
    ])
}

fn snapshot(entries: &[(&str, &JsValue)]) -> JsValue {
    let values = Array::new();
    for (key, value) in entries {
        values.push(&reader_entry(key, value));
    }
    reader_snapshot(&values)
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

#[wasm_bindgen_test]
fn root_reader_uses_the_utf16_length_prefixed_tool_context_key() {
    let root = block("root:1", &[]);
    let tool = node("tool-call", root.clone().into());
    let other = node("unknown", Object::new().into());
    let snapshot = snapshot(&[
        ("9:tool-callroot:1", tool.as_ref()),
        ("9:tool-callwrong", other.as_ref()),
    ]);
    assert!(Object::is(
        &root_tool_call_browser(snapshot.clone(), "root:1".into()).unwrap(),
        root.as_ref()
    ));
    assert!(
        root_tool_call_browser(snapshot.clone(), "wrong".into())
            .unwrap()
            .is_undefined()
    );
    assert!(
        root_tool_call_browser(snapshot, "missing".into())
            .unwrap()
            .is_undefined()
    );
}

#[wasm_bindgen_test]
fn nested_reader_returns_the_original_depth_first_block_identity() {
    let leaf = block("root:code:read", &[]);
    let sibling = block("root:sibling", &[]);
    let child = block("root:code", &[leaf.clone().into()]);
    let root = block("root", &[child.clone().into(), sibling.into()]);
    let tool = node("tool-call", root.clone().into());
    let snapshot = snapshot(&[("9:tool-callroot", tool.as_ref())]);
    assert!(Object::is(
        &find_tool_call_browser(snapshot.clone(), "root".into()).unwrap(),
        root.as_ref()
    ));
    assert!(Object::is(
        &find_tool_call_browser(snapshot.clone(), "root:code".into()).unwrap(),
        child.as_ref()
    ));
    assert!(Object::is(
        &find_tool_call_browser(snapshot, "root:code:read".into()).unwrap(),
        leaf.as_ref()
    ));
}

#[wasm_bindgen_test]
fn node_insertion_order_wins_for_duplicate_nested_ids_and_non_tools_are_skipped() {
    let first = block("duplicate", &[]);
    let second = block("duplicate", &[]);
    let non_tool = node("context", first.clone().into());
    let first_tool = node(
        "tool-call",
        block("first-root", &[first.clone().into()]).into(),
    );
    let second_tool = node("tool-call", block("second-root", &[second.into()]).into());
    let snapshot = snapshot(&[
        ("context", non_tool.as_ref()),
        ("first", first_tool.as_ref()),
        ("second", second_tool.as_ref()),
    ]);
    assert!(Object::is(
        &find_tool_call_browser(snapshot.clone(), "duplicate".into()).unwrap(),
        first.as_ref()
    ));
    assert!(
        find_tool_call_browser(snapshot, "ghost".into())
            .unwrap()
            .is_undefined()
    );
}

#[wasm_bindgen_test]
fn root_reader_returns_undefined_when_a_tool_node_has_no_root_property() {
    let rootless = object(&[
        ("kind", JsValue::from_str("tool-call")),
        ("data", Object::new().into()),
    ]);
    let snapshot = snapshot(&[("9:tool-callrootless", rootless.as_ref())]);
    assert!(
        root_tool_call_browser(snapshot, "rootless".into())
            .unwrap()
            .is_undefined()
    );
    assert!(property(rootless.as_ref(), "data").is_object());
}

#[wasm_bindgen_test]
fn readers_distinguish_surrogate_call_ids_from_replacement_characters() {
    let raw_id = js_sys::JSON::parse(r#""\ud800""#).unwrap();
    let replacement = block("�", &[]);
    let exact = object(&[
        ("callId", raw_id.clone()),
        ("subCalls", Array::new().into()),
    ]);
    let entries = Array::new();
    entries.push(&Array::of2(
        &JsValue::from_str("9:tool-call�"),
        &node("tool-call", replacement.clone().into()),
    ));
    entries.push(&Array::of2(
        &js_sys::JsString::from("9:tool-call").concat(&raw_id),
        &node("tool-call", exact.clone().into()),
    ));
    let snapshot = reader_snapshot(&entries);
    assert!(Object::is(
        &find_tool_call_browser(snapshot.clone(), raw_id.clone().unchecked_into()).unwrap(),
        exact.as_ref()
    ));
    assert!(Object::is(
        &find_tool_call_browser(snapshot.clone(), "�".into()).unwrap(),
        replacement.as_ref()
    ));
    assert!(Object::is(
        &root_tool_call_browser(snapshot, raw_id.unchecked_into()).unwrap(),
        exact.as_ref()
    ));
}
