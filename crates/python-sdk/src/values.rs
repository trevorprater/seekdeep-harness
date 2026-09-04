//! Python projection rules for the owned run interval, including falsey text coercion.

use seekdeep_identity::{MessageId, SessionId};
use serde_json::{Map, Value, json};
use std::fmt::Write as _;

use crate::{Error, ErrorKind, Notification, Result};

/// Converts string input into a single text block; all other JSON input passes through.
pub fn normalize_input(input: Value) -> Value {
    match input {
        Value::String(text) => json!([{"type":"text","text":text}]),
        value => value,
    }
}

/// Matches the root-session inbox splice that accepts this prompt's message.
pub fn is_inbox_receipt(
    notification: &Notification,
    session_id: &SessionId,
    message_id: &MessageId,
) -> bool {
    if notification.method != "session.event"
        || notification
            .payload
            .get("sessionId")
            .and_then(Value::as_str)
            != Some(session_id.as_str())
    {
        return false;
    }
    let Some(event) = notification.payload.get("event").and_then(Value::as_object) else {
        return false;
    };
    event.get("type").and_then(Value::as_str) == Some("agent/inbox/spliced")
        && event
            .get("data")
            .and_then(Value::as_object)
            .and_then(|data| data.get("inserted"))
            .and_then(Value::as_array)
            .is_some_and(|messages| {
                messages.iter().any(|message| {
                    message.get("id").and_then(Value::as_str) == Some(message_id.as_str())
                })
            })
}

/// Extracts text from the last assistant/message with an object owner and content list.
pub fn final_response(events: &[Map<String, Value>]) -> String {
    for event in events.iter().rev() {
        if event.get("type").and_then(Value::as_str) != Some("assistant/message") {
            continue;
        }
        let Some(data) = event.get("data").and_then(Value::as_object) else {
            continue;
        };
        let owner = data
            .get("message")
            .and_then(Value::as_object)
            .unwrap_or(data);
        let Some(content) = owner.get("content").and_then(Value::as_array) else {
            continue;
        };
        return content
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .map(|block| {
                block
                    .get("text")
                    .filter(|text| truthy(text))
                    .map_or_else(String::new, python_str)
            })
            .collect();
    }
    String::new()
}

/// Returns the final turn/end reason kind, preserving unknown string kinds.
///
/// # Errors
/// The last turn/end must contain a string data.reason.kind; an earlier valid event cannot mask it.
pub fn finish_reason(events: &[Map<String, Value>]) -> Result<Option<String>> {
    for event in events.iter().rev() {
        if event.get("type").and_then(Value::as_str) != Some("turn/end") {
            continue;
        }
        let kind = event
            .get("data")
            .and_then(|value| value.get("reason"))
            .and_then(|value| value.get("kind"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Protocol,
                    "turn/end event requires a string data.reason.kind",
                )
            })?;
        return Ok(Some(kind.to_owned()));
    }
    Ok(None)
}

pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_none_or(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

pub(crate) fn is_whitespace(character: char) -> bool {
    character.is_whitespace() || matches!(character, '\u{1c}'..='\u{1f}')
}

pub(crate) fn python_str(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        _ => python_repr(value),
    }
}

pub(crate) fn python_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => {
            let quote = if value.contains('\'') && !value.contains('"') {
                '"'
            } else {
                '\''
            };
            let mut text = String::from(quote);
            for character in value.chars() {
                match character {
                    '\\' => text.push_str("\\\\"),
                    '\n' => text.push_str("\\n"),
                    '\r' => text.push_str("\\r"),
                    '\t' => text.push_str("\\t"),
                    character if character == quote => {
                        text.push('\\');
                        text.push(character);
                    }
                    character if character.is_control() => {
                        let code = u32::from(character);
                        if code <= 0xff {
                            let _ = write!(text, "\\x{code:02x}");
                        } else if code <= 0xffff {
                            let _ = write!(text, "\\u{code:04x}");
                        } else {
                            let _ = write!(text, "\\U{code:08x}");
                        }
                    }
                    character => text.push(character),
                }
            }
            text.push(quote);
            text
        }
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(python_repr)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!(
                    "{}: {}",
                    python_repr(&Value::String(key.clone())),
                    python_repr(value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}
