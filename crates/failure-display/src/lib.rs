//! Display-safe projection of durable failure values.

use serde_json::{Map, Value};

/// Converts a durable failure into copy safe to expose in a GUI.
#[must_use]
pub fn display_failure_message(failure: &Value) -> String {
    match failure {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => javascript_number(value),
        Value::String(value) => value.clone(),
        Value::Array(_) => javascript_json_stringify(failure),
        Value::Object(record) => {
            if record.get("code").and_then(Value::as_str) == Some("AUTH") {
                return "API key is invalid".to_owned();
            }
            record
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| javascript_json_stringify(failure), str::to_owned)
        }
    }
}

fn javascript_json_stringify(value: &Value) -> String {
    let mut output = String::new();
    write_javascript_json(value, &mut output);
    output
}

fn write_javascript_json(value: &Value, output: &mut String) {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => output.push_str(&javascript_number(value)),
        Value::String(value) => {
            output.push_str(&serde_json::to_string(value).expect("strings always serialize"));
        }
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_javascript_json(value, output);
            }
            output.push(']');
        }
        Value::Object(values) => write_javascript_object(values, output),
    }
}

fn write_javascript_object(values: &Map<String, Value>, output: &mut String) {
    let mut indexed = values
        .iter()
        .filter_map(|(key, value)| javascript_array_index(key).map(|index| (index, key, value)))
        .collect::<Vec<_>>();
    indexed.sort_unstable_by_key(|(index, _, _)| *index);
    let ordinary = values
        .iter()
        .filter(|(key, _)| javascript_array_index(key).is_none());
    output.push('{');
    for (position, (key, value)) in indexed
        .into_iter()
        .map(|(_, key, value)| (key, value))
        .chain(ordinary)
        .enumerate()
    {
        if position > 0 {
            output.push(',');
        }
        output.push_str(&serde_json::to_string(key).expect("object keys always serialize"));
        output.push(':');
        write_javascript_json(value, output);
    }
    output.push('}');
}

fn javascript_array_index(key: &str) -> Option<u32> {
    let index = key.parse::<u32>().ok()?;
    (index != u32::MAX && index.to_string() == key).then_some(index)
}

fn javascript_number(value: &serde_json::Number) -> String {
    let number = value
        .as_f64()
        .expect("every serde_json number converts to a JavaScript number");
    let mut buffer = ryu_js::Buffer::new();
    buffer.format(number).to_owned()
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
