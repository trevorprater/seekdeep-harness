//! Canonical selection of a child's final assistant output.

use seekdeep_core::session::SessionEvent;
use seekdeep_llm::ContentBlock;

/// Incremental fold of the final-output selection rule.
#[derive(Default)]
pub struct AssistantOutputFold {
    message: Option<Vec<ContentBlock>>,
    partial: Vec<String>,
}

impl AssistantOutputFold {
    /// Creates an empty fold.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Folds one session event into the selection.
    pub fn push(&mut self, event: &SessionEvent) {
        if event.event_type == "assistant/message" {
            let content = event
                .data
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array());
            if let Some(content) = content {
                let blocks = content
                    .iter()
                    .filter_map(|block| serde_json::from_value::<ContentBlock>(block.clone()).ok())
                    .collect::<Vec<_>>();
                if !blocks.is_empty() {
                    self.message = Some(blocks);
                }
            }
        } else if event.event_type == "assistant/chunk" {
            let chunk = event.data.get("chunk");
            if chunk.and_then(|c| c.get("type")).and_then(|t| t.as_str()) == Some("text-delta")
                && let Some(text) = chunk.and_then(|c| c.get("text")).and_then(|t| t.as_str())
            {
                self.push_text(text);
            }
        }
    }

    /// Extends the streamed fallback with raw text.
    pub fn push_text(&mut self, text: &str) {
        if !text.is_empty() {
            self.partial.push(text.to_owned());
        }
    }

    /// Selects the final output folded so far.
    #[must_use]
    pub fn collect(&self) -> Option<Vec<ContentBlock>> {
        if let Some(message) = &self.message {
            return Some(message.clone());
        }
        let text = self.partial.concat();
        if text.is_empty() {
            None
        } else {
            Some(vec![ContentBlock::Text { text }])
        }
    }
}

/// Applies the selection rule to one complete child-owned event suffix.
#[must_use]
pub fn final_assistant_output(events: &[SessionEvent]) -> Option<Vec<ContentBlock>> {
    let mut fold = AssistantOutputFold::new();
    for event in events {
        fold.push(event);
    }
    fold.collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn event(event_type: &str, data: serde_json::Value) -> SessionEvent {
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
    fn selects_last_non_empty_assistant_message() {
        let events = vec![
            event(
                "assistant/message",
                json!({"message": {"content": [{"type": "text", "text": "first"}]}}),
            ),
            event(
                "assistant/message",
                json!({"message": {"content": [{"type": "text", "text": "second"}]}}),
            ),
        ];
        let output = final_assistant_output(&events).expect("output");
        assert_eq!(
            output,
            vec![ContentBlock::Text {
                text: "second".to_owned()
            }]
        );
    }

    #[test]
    fn falls_back_to_streamed_text() {
        let chunk = event(
            "assistant/chunk",
            json!({"chunk": {"type": "text-delta", "text": "hel"}}),
        );
        let mut fold = AssistantOutputFold::new();
        fold.push(&chunk);
        fold.push_text("lo");
        let output = fold.collect().expect("output");
        assert_eq!(
            output,
            vec![ContentBlock::Text {
                text: "hello".to_owned()
            }]
        );
    }
}
