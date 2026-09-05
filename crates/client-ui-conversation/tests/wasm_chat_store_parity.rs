//! Live WASM coverage for the declarative chat store spec.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Function, Object, Reflect};
use seekdeep_client_ui_conversation::{
    configure_client_ui_conversation_chat_store, create_chat_store_browser,
};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
let declarations = []
let receivers = []
export function installDefineStore() {
  declarations = []; receivers = []
  return function defineStore(spec) {
    declarations.push(spec); receivers.push(this)
    return { kind: 'handle', spec }
  }
}
export function storeDeclarations() { return declarations }
export function storeReceivers() { return receivers }
export function storeObject(entries) { return Object.fromEntries(entries) }
export function storeFrozen(entries) { return Object.freeze(Object.fromEntries(entries)) }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = installDefineStore)]
    fn install_define_store() -> Function;
    #[wasm_bindgen(js_name = storeDeclarations)]
    fn store_declarations() -> Array;
    #[wasm_bindgen(js_name = storeReceivers)]
    fn store_receivers() -> Array;
    #[wasm_bindgen(js_name = storeObject)]
    fn store_object(entries: &Array) -> JsValue;
    #[wasm_bindgen(js_name = storeFrozen)]
    fn store_frozen(entries: &Array) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    store_object(&array).unchecked_into()
}

fn frozen(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    store_frozen(&array).unchecked_into()
}

fn setup() {
    configure_client_ui_conversation_chat_store(install_define_store());
}

#[wasm_bindgen_test]
fn declaration_pins_init_shape_compatibility_key_and_free_define_store_call() {
    setup();
    let handle = create_chat_store_browser().unwrap();
    assert_eq!(
        property(&handle, "kind").as_string().as_deref(),
        Some("handle")
    );
    let declaration = store_declarations().get(0);
    assert_eq!(
        property(&declaration, "persist").as_string().as_deref(),
        Some("dsh.conversation.chat")
    );
    let initial = property(&declaration, "init")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert!(property(&initial, "selection").is_null());
    assert_eq!(property(&initial, "draft").as_string().as_deref(), Some(""));
    assert!(property(&initial, "view").is_null());
    assert!(property(&initial, "inspect").is_null());
    assert!(store_receivers().get(0).is_undefined());
}

#[wasm_bindgen_test]
fn all_four_actions_mutate_only_their_declared_field_and_return_undefined() {
    setup();
    let declaration = property(&create_chat_store_browser().unwrap(), "spec");
    let actions = property(&declaration, "actions");
    let draft = object(&[
        ("selection", JsValue::NULL),
        ("draft", JsValue::from_str("")),
        ("view", JsValue::NULL),
        ("inspect", JsValue::NULL),
        ("sentinel", JsValue::from_str("keep")),
    ]);
    for (name, field, value) in [
        (
            "select",
            "selection",
            object(&[("turnSeq", JsValue::from_f64(3.0))]).into(),
        ),
        ("setDraft", "draft", JsValue::from_str("hello")),
        ("setView", "view", JsValue::from_str("chat")),
        (
            "setInspect",
            "inspect",
            object(&[("callId", JsValue::from_str("c1"))]).into(),
        ),
    ] {
        let result = property(&actions, name)
            .dyn_into::<Function>()
            .unwrap()
            .call2(&JsValue::UNDEFINED, draft.as_ref(), &value)
            .unwrap();
        assert!(result.is_undefined());
        assert!(Object::is(&property(draft.as_ref(), field), &value));
        assert_eq!(
            property(draft.as_ref(), "sentinel").as_string().as_deref(),
            Some("keep")
        );
    }
    property(&actions, "select")
        .dyn_into::<Function>()
        .unwrap()
        .call2(&JsValue::UNDEFINED, draft.as_ref(), &JsValue::NULL)
        .unwrap();
    assert!(property(draft.as_ref(), "selection").is_null());
    property(&actions, "setInspect")
        .dyn_into::<Function>()
        .unwrap()
        .call2(&JsValue::UNDEFINED, draft.as_ref(), &JsValue::NULL)
        .unwrap();
    assert!(property(draft.as_ref(), "inspect").is_null());
    let frozen = frozen(&[("draft", JsValue::from_str("fixed"))]);
    assert!(
        property(&actions, "setDraft")
            .dyn_into::<Function>()
            .unwrap()
            .call2(
                &JsValue::UNDEFINED,
                frozen.as_ref(),
                &JsValue::from_str("rejected")
            )
            .is_err()
    );
}

#[wasm_bindgen_test]
fn every_factory_call_builds_fresh_init_actions_and_handle_identity() {
    setup();
    let first = create_chat_store_browser().unwrap();
    let second = create_chat_store_browser().unwrap();
    assert!(!Object::is(&first, &second));
    let first_spec = property(&first, "spec");
    let second_spec = property(&second, "spec");
    assert!(!Object::is(&first_spec, &second_spec));
    assert!(!Object::is(
        &property(&first_spec, "init"),
        &property(&second_spec, "init")
    ));
    assert!(!Object::is(
        &property(&first_spec, "actions"),
        &property(&second_spec, "actions")
    ));
    let first_state = property(&first_spec, "init")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    let second_state = property(&second_spec, "init")
        .dyn_into::<Function>()
        .unwrap()
        .call0(&JsValue::UNDEFINED)
        .unwrap();
    assert!(!Object::is(&first_state, &second_state));
}
