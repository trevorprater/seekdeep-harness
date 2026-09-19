//! Live WASM coverage for browser decoration exports.

#![cfg(target_arch = "wasm32")]

use js_sys::{Array, Map, Object, Reflect};
use seekdeep_client_ui_conversation::{derive_decorations_browser, scan_text_refs_browser};
use wasm_bindgen::{JsCast as _, JsValue, prelude::wasm_bindgen};
use wasm_bindgen_test::wasm_bindgen_test;

#[wasm_bindgen(inline_js = r#"
export function decorationObject(entries) { return Object.fromEntries(entries) }
"#)]
extern "C" {
    #[wasm_bindgen(js_name = decorationObject)]
    fn decoration_object(entries: &Array) -> JsValue;
}

fn property(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap()
}

fn object(entries: &[(&str, JsValue)]) -> Object {
    let array = Array::new();
    for (key, value) in entries {
        array.push(&Array::of2(&JsValue::from_str(key), value));
    }
    decoration_object(&array).unchecked_into()
}

fn lexicon() -> Map {
    let value = Map::new();
    value.set(
        &JsValue::from_str("/"),
        &Array::of2(&JsValue::from_str("deploy"), &JsValue::from_str("goal")),
    );
    value.set(
        &JsValue::from_str("@"),
        &Array::of1(&JsValue::from_str("agent")),
    );
    value
}

#[wasm_bindgen_test]
fn browser_scan_preserves_utf16_offsets_triggers_and_hot_lexicon_membership() {
    let ranges = scan_text_refs_browser("😀 /deploy @agent x/goal".to_owned(), lexicon().into())
        .unwrap()
        .unchecked_into::<Array>();
    assert_eq!(ranges.length(), 2);
    assert_eq!(property(&ranges.get(0), "start").as_f64(), Some(3.0));
    assert_eq!(property(&ranges.get(0), "end").as_f64(), Some(10.0));
    assert_eq!(
        property(&ranges.get(0), "trigger").as_string().as_deref(),
        Some("/")
    );
    assert_eq!(property(&ranges.get(1), "start").as_f64(), Some(11.0));
    assert_eq!(
        property(&ranges.get(1), "trigger").as_string().as_deref(),
        Some("@")
    );
    assert_eq!(
        scan_text_refs_browser("/deploy".to_owned(), JsValue::UNDEFINED)
            .unwrap()
            .unchecked_into::<Array>()
            .length(),
        0
    );
}

#[wasm_bindgen_test]
fn browser_derivation_pins_claim_chips_hint_and_text_refs() {
    let state = object(&[
        ("draft", JsValue::from_str("/goal \u{FFFC} /deploy")),
        ("phase", JsValue::from_str("claimed")),
        (
            "claim",
            object(&[
                ("token", JsValue::from_str("/goal ")),
                ("hint", JsValue::from_str("目标")),
            ])
            .into(),
        ),
        (
            "occurrences",
            Array::of1(
                object(&[
                    ("occurrenceId", JsValue::from_f64(7.0)),
                    ("offset", JsValue::from_f64(3.0)),
                    ("label", JsValue::from_str("@agent")),
                    ("invalid", JsValue::TRUE),
                ])
                .as_ref(),
            )
            .into(),
        ),
    ]);
    let decorations = derive_decorations_browser(state.into(), lexicon().into()).unwrap();
    assert_eq!(
        property(&property(&decorations, "token"), "end").as_f64(),
        Some(6.0)
    );
    let chips = property(&decorations, "chips").unchecked_into::<Array>();
    assert_eq!(chips.length(), 1);
    assert_eq!(property(&chips.get(0), "occurrenceId").as_f64(), Some(7.0));
    assert_eq!(property(&chips.get(0), "invalid").as_bool(), Some(true));
    assert!(property(&decorations, "hint").is_null());
    assert_eq!(
        property(&decorations, "textRefs")
            .unchecked_into::<Array>()
            .length(),
        2
    );

    let blank = object(&[
        ("draft", JsValue::from_str("/goal \u{FEFF}\n")),
        ("phase", JsValue::from_str("submitting")),
        (
            "claim",
            object(&[
                ("token", JsValue::from_str("/goal ")),
                ("hint", JsValue::from_str("目标")),
            ])
            .into(),
        ),
        ("occurrences", Array::new().into()),
    ]);
    let blank = derive_decorations_browser(blank.into(), JsValue::UNDEFINED).unwrap();
    assert_eq!(
        property(&blank, "hint").as_string().as_deref(),
        Some("目标")
    );
}

#[wasm_bindgen_test]
fn malformed_lexicon_and_occurrence_fail_loudly() {
    let bad_lexicon = Map::new();
    bad_lexicon.set(&JsValue::from_str("/"), &JsValue::from_str("deploy"));
    assert!(scan_text_refs_browser("/deploy".to_owned(), bad_lexicon.into()).is_err());
    let state = object(&[
        ("draft", JsValue::from_str("x")),
        ("phase", JsValue::from_str("plain")),
        (
            "occurrences",
            Array::of1(object(&[("occurrenceId", JsValue::from_f64(1.5))]).as_ref()).into(),
        ),
    ]);
    assert!(derive_decorations_browser(state.into(), JsValue::UNDEFINED).is_err());
}
