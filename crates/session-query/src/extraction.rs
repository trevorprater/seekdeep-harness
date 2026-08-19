//! First-party semantic text extraction for session-query consumers.

use seekdeep_core::session::SessionEvent;
use seekdeep_llm::ContentBlock;
use serde_json::Value;

/// Extracts searchable semantic text from one first-party session event.
#[must_use]
pub fn extract_session_event_text(event: &SessionEvent) -> String {
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
            if let Some(todos) = event.data.get("todos").and_then(Value::as_array) {
                for todo in todos {
                    parts.push(str_value(todo.get("status")));
                    parts.push(str_value(todo.get("content")));
                }
            }
            join_text(&parts)
        }
        "turn/end" => turn_end_text(event.data.get("reason")),
        _ => String::new(),
    }
}

fn turn_end_text(reason: Option<&Value>) -> String {
    let Some(reason) = reason else {
        return String::new();
    };
    match reason.get("kind").and_then(Value::as_str) {
        Some("error") => join_text(&[
            "error".to_owned(),
            str_value(reason.get("error").and_then(|error| error.get("message"))),
        ]),
        Some("aborted") => "aborted".to_owned(),
        Some("max-tokens" | "interrupted") => str_value(reason.get("kind")),
        _ => String::new(),
    }
}

fn content_text(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    let blocks: Vec<ContentBlock> = serde_json::from_value(content.clone()).unwrap_or_default();
    let parts = blocks.iter().flat_map(block_text).collect::<Vec<_>>();
    join_text(&parts)
}

fn block_text(block: &ContentBlock) -> Vec<String> {
    match block {
        ContentBlock::Text { text } => vec![text.clone()],
        ContentBlock::ToolCall {
            name, arguments, ..
        } => vec![name.clone(), arguments.clone()],
        ContentBlock::ToolResult { content, .. } => content.iter().flat_map(block_text).collect(),
        _ => Vec::new(),
    }
}

fn str_value(value: Option<&Value>) -> String {
    value.and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn join_text(parts: &[String]) -> String {
    parts
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn event(event_type: &str, data: Value) -> SessionEvent {
        SessionEvent {
            event_type: event_type.to_owned(),
            seq: 0,
            time: 0,
            data,
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
}
