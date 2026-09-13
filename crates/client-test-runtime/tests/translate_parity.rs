//! Behavioral parity for the Client feature-test translation helper.

#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;

use seekdeep_client_test_runtime::{TestTranslateValue, TestTranslator};

fn dictionary(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

#[test]
fn first_dictionary_wins_unknown_keys_stay_visible_and_parameters_interpolate() {
    let translator = TestTranslator::new([
        dictionary(&[("known", "Hello {name}, {missing}!"), ("shared", "first")]),
        dictionary(&[("shared", "second"), ("fallback", "Fallback")]),
    ]);
    assert_eq!(translator.translate("shared", None), "first");
    assert_eq!(translator.translate("fallback", None), "Fallback");
    assert_eq!(translator.translate("unknown", None), "unknown");
    let params = BTreeMap::from([("name".to_owned(), TestTranslateValue::from("Ada"))]);
    assert_eq!(
        translator.translate("known", Some(&params)),
        "Hello Ada, {missing}!"
    );
}

#[test]
fn interpolation_matches_javascript_string_conversion_and_word_names() {
    let translator = TestTranslator::new([dictionary(&[(
        "values",
        "{count}|{flag}|{nil}|{array}|{object}|{not-word}",
    )])]);
    let params = BTreeMap::from([
        ("count".to_owned(), TestTranslateValue::Number(3.0)),
        ("flag".to_owned(), TestTranslateValue::Bool(true)),
        ("nil".to_owned(), TestTranslateValue::Null),
        (
            "array".to_owned(),
            TestTranslateValue::Array(vec![
                TestTranslateValue::from("a"),
                TestTranslateValue::Null,
                TestTranslateValue::from("b"),
            ]),
        ),
        ("object".to_owned(), TestTranslateValue::Object),
        ("not-word".to_owned(), TestTranslateValue::from("ignored")),
    ]);
    assert_eq!(
        translator.translate("values", Some(&params)),
        "3|true|null|a,,b|[object Object]|{not-word}"
    );
}
