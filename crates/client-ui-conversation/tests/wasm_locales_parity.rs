//! Live WASM coverage for the embedded conversation dictionaries.

#![cfg(target_arch = "wasm32")]

use js_sys::{Object, Reflect};
use seekdeep_client_ui_conversation::{
    conversation_en_browser, conversation_locales_browser, conversation_namespace_browser,
    conversation_zh_browser,
};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::wasm_bindgen_test;

fn text(value: &JsValue, key: &str) -> String {
    Reflect::get(value, &JsValue::from_str(key))
        .unwrap()
        .as_string()
        .unwrap()
}

#[wasm_bindgen_test]
fn dictionaries_preserve_namespace_identity_complete_keyset_and_exact_copy() {
    assert_eq!(conversation_namespace_browser(), "conversation");
    let locales = conversation_locales_browser().unwrap();
    assert!(Object::is(
        &locales,
        &conversation_locales_browser().unwrap()
    ));
    let zh = conversation_zh_browser().unwrap();
    let en = conversation_en_browser().unwrap();
    assert!(Object::is(
        &zh,
        &Reflect::get(&locales, &JsValue::from_str("zh")).unwrap()
    ));
    assert!(Object::is(
        &en,
        &Reflect::get(&locales, &JsValue::from_str("en")).unwrap()
    ));
    let zh_keys = Object::keys(&Object::from(zh.clone()));
    let en_keys = Object::keys(&Object::from(en.clone()));
    assert_eq!(zh_keys.length(), en_keys.length());
    for key in zh_keys.iter() {
        assert!(Reflect::has(&en, &key).unwrap());
    }
    assert_eq!(text(&zh, "view.chat"), "对话");
    assert_eq!(text(&en, "view.chat"), "Chat");
    assert_eq!(text(&zh, "hint.plan"), text(&zh, "placeholder.plan"));
    assert_eq!(text(&en, "hint.plan"), text(&en, "placeholder.plan"));
    assert_eq!(
        text(&en, "message.maxTokens.hint"),
        "The reply was cut off; earlier output is preserved in the conversation. Send \"continue\" to let the model resume."
    );
    assert_eq!(text(&zh, "queue.steerFailed"), "插话发送失败，请重试。");
}
