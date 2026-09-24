//! Provider JSON conversion and pi-ai text normalization.

use seekdeep_llm::JsonString;
use seekdeep_lossless_json::JsonRef;
use serde::de::{DeserializeOwned, Error as _};
use serde_json::{Map, Value};

/// Applies pi-ai's provider normalization without modifying the durable string.
pub(crate) fn sanitize_surrogates(text: &JsonString) -> String {
    char::decode_utf16(text.utf16_units().iter().copied())
        .filter_map(Result::ok)
        .collect()
}

pub(crate) fn text_is_blank(text: &JsonString) -> bool {
    text.utf16_units().iter().copied().all(is_whitespace)
}

pub(crate) fn scalar_text_is_blank(text: &str) -> bool {
    text.encode_utf16().all(is_whitespace)
}

fn is_whitespace(unit: u16) -> bool {
    matches!(unit, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}

pub(crate) fn required<T: DeserializeOwned>(
    value: JsonRef<'_>,
    key: &'static str,
) -> serde_json::Result<T> {
    value
        .get(key)
        .ok_or_else(|| serde_json::Error::missing_field(key))?
        .deserialize()
}

pub(crate) fn optional<T: DeserializeOwned>(
    value: JsonRef<'_>,
    key: &str,
) -> serde_json::Result<Option<T>> {
    value.get(key).map_or(Ok(None), JsonRef::deserialize)
}

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
