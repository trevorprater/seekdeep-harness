//! First-party semantic text extraction for session-query consumers.

use seekdeep_core::session::SessionEvent;
use seekdeep_llm::{ContentBlock, JsonString};
use seekdeep_lossless_json::JsonRef;

/// Extracts searchable semantic text from one first-party session event.
#[must_use]
pub fn extract_session_event_text(event: &SessionEvent) -> JsonString {
    match event.event_type.as_str() {
        "user/message" => content_text(event.data.get("content")),
        "assistant/message" => content_text(
            event
                .data
                .get("message")
                .and_then(|message| message.get("content")),
        ),
        "tool/call" => join_text(&[
            str_value(event.data.get("name")),
            str_value(event.data.get("arguments")),
        ]),
        "tool/result" => join_text(&[
            content_text(
                event
                    .data
                    .get("message")
                    .and_then(|message| message.get("content")),
            ),
            str_value(event.data.get("error").and_then(|error| error.get("name"))),
            str_value(event.data.get("error").and_then(|error| error.get("code"))),
        ]),
        "todo/write" => {
            let mut parts = Vec::new();
            if let Some(todos) = event.data.get("todos").and_then(JsonRef::array_items) {
                for todo in todos {
                    parts.push(str_value(todo.get("status")));
                    parts.push(str_value(todo.get("content")));
                }
            }
            join_text(&parts)
        }
        "turn/end" => turn_end_text(event.data.get("reason")),
        _ => JsonString::default(),
    }
}

fn turn_end_text(reason: Option<JsonRef<'_>>) -> JsonString {
    let Some(reason) = reason else {
        return JsonString::default();
    };
    match reason
        .get("kind")
        .and_then(|kind| kind.deserialize::<String>().ok())
        .as_deref()
    {
        Some("error") => join_text(&[
            "error".into(),
            str_value(reason.get("error").and_then(|error| error.get("message"))),
        ]),
        Some("aborted") => "aborted".into(),
        Some("max-tokens" | "interrupted") => str_value(reason.get("kind")),
        _ => JsonString::default(),
    }
}

fn content_text(content: Option<JsonRef<'_>>) -> JsonString {
    let Some(content) = content else {
        return JsonString::default();
    };
    let blocks: Vec<ContentBlock> = content.deserialize().unwrap_or_default();
    let parts = blocks.iter().flat_map(block_text).collect::<Vec<_>>();
    join_text(&parts)
}

fn block_text(block: &ContentBlock) -> Vec<JsonString> {
    match block {
        ContentBlock::Text { text } => vec![text.clone()],
        ContentBlock::ToolCall {
            name, arguments, ..
        } => vec![name.clone().into(), arguments.clone().into()],
        ContentBlock::ToolResult { content, .. } => content.iter().flat_map(block_text).collect(),
        _ => Vec::new(),
    }
}

fn str_value(value: Option<JsonRef<'_>>) -> JsonString {
    value
        .and_then(|value| value.deserialize().ok())
        .unwrap_or_default()
}

fn join_text(parts: &[JsonString]) -> JsonString {
    let parts = parts
        .iter()
        .map(JsonString::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    JsonString::join(&parts, "\n")
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn event(event_type: &str, data: Value) -> SessionEvent {
        SessionEvent {
            event_type: event_type.to_owned(),
            seq: 0,
            time: 0,
            data: data.into(),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        }
    }

    #[test]
    fn extracts_text_blocks_and_tool_calls() {
        let text = event(
            "user/message",
            json!({"content": [{"type": "text", "text": "  hello  "}, {"type": "reasoning", "text": "skip"}]}),
        );
        assert_eq!(extract_session_event_text(&text), "hello");

        let call = event(
            "tool/call",
            json!({"name": "Bash", "arguments": "{\"a\":1}"}),
        );
        assert_eq!(extract_session_event_text(&call), "Bash\n{\"a\":1}");
    }

    #[test]
    fn structural_events_contribute_no_text() {
        for event_type in [
            "turn/start",
            "step/start",
            "step/end",
            "assistant/chunk",
            "request/header",
        ] {
            assert_eq!(
                extract_session_event_text(&event(event_type, json!({}))),
                "",
                "{event_type}"
            );
        }
        assert_eq!(
            extract_session_event_text(&event(
                "turn/end",
                json!({"reason": {"kind": "completed"}})
            )),
            ""
        );
    }

    #[test]
    fn turn_end_error_yields_the_detail() {
        let event = event(
            "turn/end",
            json!({"reason": {"kind": "error", "error": {"message": "boom"}}}),
        );
        assert_eq!(extract_session_event_text(&event), "error\nboom");
    }

    #[test]
    fn nested_tool_text_keeps_unpaired_surrogates_in_semantic_documents() {
        let mut event = event("tool/result", json!({}));
        event.data = seekdeep_lossless_json::JsonValue::parse(
            r#"{"message":{"content":[{"type":"tool-result","toolCallId":"call","content":[{"type":"text","text":"  \ud800  "},{"type":"text","text":"\\ud800"}]}]}}"#.into(),
        ).unwrap();
        let text = extract_session_event_text(&event);
        assert_eq!(text.as_raw(), r#""\ud800\n\\ud800""#);
        assert_eq!(serde_json::to_string(&text).unwrap(), text.as_raw());
    }
}
