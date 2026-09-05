//! ECMAScript-compatible compact JSON serialization for provider payload fields.

use serde_json::{Map, Value};

pub(crate) fn stringify_object(arguments: &Map<String, Value>) -> serde_json::Result<String> {
    stringify(&Value::Object(arguments.clone()))
}

pub(crate) fn stringify(value: &Value) -> serde_json::Result<String> {
    let mut output = String::new();
    write_json(value, &mut output)?;
    Ok(output)
}

fn write_json(value: &Value, output: &mut String) -> serde_json::Result<()> {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
        Value::Number(value) => {
            output.push_str(ryu_js::Buffer::new().format(value.as_f64().unwrap_or_default()));
        }
        Value::String(value) => output.push_str(&serde_json::to_string(value)?),
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_json(value, output)?;
            }
            output.push(']');
        }
        Value::Object(values) => {
            output.push('{');
            for (index, (key, value)) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key)?);
                output.push(':');
                write_json(value, output)?;
            }
            output.push('}');
        }
    }
    Ok(())
}
