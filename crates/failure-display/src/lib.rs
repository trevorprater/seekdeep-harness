//! Display-safe projection of durable failure values.

use seekdeep_lossless_json::{JsonRef, JsonString, JsonValue};
use serde_json::Value;

/// Converts a durable failure into copy safe to expose in a GUI.
#[must_use]
pub fn display_failure_message(failure: &Value) -> String {
    match failure {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => javascript_number(value),
        Value::String(value) => value.clone(),
        Value::Array(_) => JsonValue::from(failure.clone()).stringify(),
        Value::Object(record) => {
            if record.get("code").and_then(Value::as_str) == Some("AUTH") {
                return "API key is invalid".to_owned();
            }
            record.get("message").and_then(Value::as_str).map_or_else(
                || JsonValue::from(failure.clone()).stringify(),
                str::to_owned,
            )
        }
    }
}

/// Formats durable failure text without replacing UTF-16 code units.
///
/// AUTH failures conceal their diagnostic message. Other string messages are
/// returned directly; remaining objects and arrays use ECMAScript JSON formatting.
#[must_use]
pub fn display_failure_message_json(failure: &JsonValue) -> JsonString {
    if let Some(units) = failure.to_utf16() {
        return JsonString::from_utf16(&units);
    }
    if let Some(number) = failure.as_f64() {
        return ryu_js::Buffer::new().format(number).into();
    }
    if failure.get("code").is_some_and(|code| code == "AUTH") {
        return "API key is invalid".into();
    }
    failure
        .get("message")
        .and_then(JsonRef::to_utf16)
        .map_or_else(
            || failure.stringify().into(),
            |units| JsonString::from_utf16(&units),
        )
}

fn javascript_number(value: &serde_json::Number) -> String {
    let number = value.as_f64().unwrap_or_else(|| {
        value
            .to_string()
            .parse::<f64>()
            .expect("valid JSON numbers convert to ECMAScript numbers")
    });
    ryu_js::Buffer::new().format(number).to_owned()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn auth_is_redacted_and_other_failure_shapes_match_javascript_display() {
        for (failure, expected) in [
            (Value::Null, "null"),
            (json!(true), "true"),
            (json!("plain"), "plain"),
            (json!(-0.0), "0"),
            (
                json!({"code": "AUTH", "message": "key sk-secret failed"}),
                "API key is invalid",
            ),
            (
                json!({"code": "SERVER", "message": "provider unavailable"}),
                "provider unavailable",
            ),
        ] {
            assert_eq!(display_failure_message(&failure), expected);
        }
    }

    #[test]
    fn fallback_json_uses_javascript_number_and_property_order() {
        let failure = serde_json::from_str::<Value>(
            r#"{"later":1,"10":"ten","2":"two","01":"leading","nested":{"3":3,"1":1},"large":9007199254740993}"#,
        )
        .unwrap();
        assert_eq!(
            display_failure_message(&failure),
            r#"{"2":"two","10":"ten","later":1,"01":"leading","nested":{"1":1,"3":3},"large":9007199254740992}"#
        );
        assert_eq!(
            display_failure_message(&json!([{"message": 7}, null])),
            r#"[{"message":7},null]"#
        );
    }
}
