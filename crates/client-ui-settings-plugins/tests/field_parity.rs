//! Settings card number/text conversion parity.

use seekdeep_client_ui_settings_plugins::{FieldWrite, number_field, text_field};
use serde_json::json;

#[test]
fn number_fields_format_clear_parse_and_reject_like_javascript_number() {
    let field = number_field("timeoutMs");
    assert_eq!(field.field, "timeoutMs");
    assert_eq!(field.format(&json!(60_000)), "60000");
    assert_eq!(field.format(&json!("60000")), "");
    assert_eq!(field.parse("  "), Some(FieldWrite::Clear));
    assert_eq!(field.parse("9000"), Some(FieldWrite::Set(json!(9000.0))));
    assert_eq!(field.parse("0x10"), Some(FieldWrite::Set(json!(16.0))));
    assert_eq!(field.parse("0b11"), Some(FieldWrite::Set(json!(3.0))));
    assert_eq!(field.parse("0o10"), Some(FieldWrite::Set(json!(8.0))));
    assert_eq!(field.parse("soon"), None);
    assert_eq!(field.parse("Infinity"), None);
}

#[test]
fn text_fields_format_trim_set_and_clear() {
    let field = text_field("baseURL");
    assert_eq!(field.field, "baseURL");
    assert_eq!(
        field.format(&json!("https://api.deepseek.com")),
        "https://api.deepseek.com"
    );
    assert_eq!(field.format(&json!(42)), "");
    assert_eq!(field.parse(" \n "), FieldWrite::Clear);
    assert_eq!(
        field.parse("  https://api.deepseek.com  "),
        FieldWrite::Set(json!("https://api.deepseek.com"))
    );
}
