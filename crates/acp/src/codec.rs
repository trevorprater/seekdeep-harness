//! Pure translation between Harness and baseline ACP content/reasons.

use std::fmt::Write as _;

use seekdeep_llm::JsonString;
use seekdeep_lossless_json::{JsonRef, JsonValue};
use seekdeep_subagent::SubagentStopReason;
use serde_json::Value;

use crate::types::AcpStopReason;

/// Maps one Harness turn-ending kind to ACP's prompt reason.
#[must_use]
pub fn turn_end_to_stop_reason(kind: &str) -> AcpStopReason {
    match kind {
        "interrupted" => AcpStopReason::Cancelled,
        _ => AcpStopReason::EndTurn,
    }
}

/// Concatenates baseline prompt blocks and renders resource links explicitly.
#[must_use]
pub fn acp_prompt_to_text(prompt: &[Value]) -> String {
    let mut text = String::new();
    for block in prompt {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(value) = block.get("text").and_then(Value::as_str) {
                    text.push_str(value);
                }
            }
            Some("resource_link") => {
                let name =
                    serde_json::to_string(block.get("name").and_then(Value::as_str).unwrap_or(""))
                        .unwrap_or_else(|_| "\"\"".to_owned());
                let uri =
                    serde_json::to_string(block.get("uri").and_then(Value::as_str).unwrap_or(""))
                        .unwrap_or_else(|_| "\"\"".to_owned());
                write!(&mut text, "\n[resource_link name={name} uri={uri}]\n")
                    .expect("writing to a String is infallible");
            }
            Some(_) | None => {}
        }
    }
    text
}

/// Whether any prompt block exceeds baseline text/resource-link support.
#[must_use]
pub fn prompt_has_unsupported_content(prompt: &[Value]) -> bool {
    prompt.iter().any(|block| {
        !matches!(
            block.get("type").and_then(Value::as_str),
            Some("text" | "resource_link")
        )
    })
}

/// Maps a remote ACP terminal to the shared subagent vocabulary.
#[must_use]
pub fn acp_stop_reason(reason: &AcpStopReason) -> SubagentStopReason {
    match reason {
        AcpStopReason::EndTurn => SubagentStopReason::Completed,
        AcpStopReason::MaxTokens => SubagentStopReason::MaxTokens,
        AcpStopReason::Refusal => SubagentStopReason::Refusal,
        AcpStopReason::Cancelled => SubagentStopReason::Aborted,
        AcpStopReason::MaxTurnRequests | AcpStopReason::Unknown(_) => SubagentStopReason::Error,
    }
}

/// Returns streamed text and ignores non-text update content.
#[must_use]
pub fn acp_content_text(content: &Value) -> &str {
    if content.get("type").and_then(Value::as_str) == Some("text") {
        content.get("text").and_then(Value::as_str).unwrap_or("")
    } else {
        ""
    }
}

/// Translates Harness prompt blocks to ACP text blocks, dropping non-text blocks.
#[must_use]
pub fn to_acp_prompt(prompt: &[seekdeep_llm::ContentBlock]) -> Vec<JsonValue> {
    prompt
        .iter()
        .filter_map(|block| match block {
            seekdeep_llm::ContentBlock::Text { text } => Some(
                JsonValue::from_serialize(&seekdeep_llm::ContentBlock::text(text.clone()))
                    .expect("text block is JSON"),
            ),
            _ => None,
        })
        .collect()
}

/// Reads text from an ACP update without converting its code units to UTF-8.
#[must_use]
pub fn acp_content_text_json(content: JsonRef<'_>) -> JsonString {
    if content
        .get("type")
        .and_then(|value| value.deserialize::<String>().ok())
        .as_deref()
        == Some("text")
    {
        content
            .get("text")
            .and_then(|value| value.deserialize().ok())
            .unwrap_or_default()
    } else {
        JsonString::default()
    }
}

/// Concatenates exact prompt text and renders resource links as JSON-quoted fields.
#[must_use]
pub fn acp_prompt_to_text_json(prompt: &[JsonValue]) -> JsonString {
    let mut text = JsonString::default();
    for block in prompt {
        match block
            .get("type")
            .and_then(|value| value.deserialize::<String>().ok())
            .as_deref()
        {
            Some("text") => text.push_utf16(acp_content_text_json(block.as_ref()).utf16_units()),
            Some("resource_link") => {
                let name: JsonString = block
                    .get("name")
                    .and_then(|value| value.deserialize().ok())
                    .unwrap_or_default();
                let uri: JsonString = block
                    .get("uri")
                    .and_then(|value| value.deserialize().ok())
                    .unwrap_or_default();
                text.push_str(&format!(
                    "\n[resource_link name={} uri={}]\n",
                    name.as_raw(),
                    uri.as_raw()
                ));
            }
            Some(_) | None => {}
        }
    }
    text
}

/// Whether an exact prompt contains content outside baseline ACP support.
#[must_use]
pub fn prompt_has_unsupported_content_json(prompt: &[JsonValue]) -> bool {
    prompt.iter().any(|block| {
        !matches!(
            block
                .get("type")
                .and_then(|value| value.deserialize::<String>().ok())
                .as_deref(),
            Some("text" | "resource_link"),
        )
    })
}
