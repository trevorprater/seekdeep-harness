//! Authoritative transient queue projection and durable steering handoff.

use std::rc::Rc;

use seekdeep_identity::MessageId;
use serde_json::Value;

const QUEUE_PREVIEW_CHARS: usize = 200;

/// Queue placement selected by the Host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuePlacement {
    /// Next Turn.
    Queued,
    /// Next Step steering.
    Steering,
}

/// One authoritative Host queue item.
#[derive(Clone, Debug, PartialEq)]
pub struct QueueItemInput {
    /// Queue item identity.
    pub id: MessageId,
    /// Durable message identity.
    pub message_id: MessageId,
    /// Placement.
    pub placement: QueuePlacement,
    /// Complete message content blocks.
    pub content: Vec<Value>,
}

/// One Client queue row.
#[derive(Clone, Debug, PartialEq)]
pub struct QueuedMessage {
    /// Queue item identity.
    pub id: MessageId,
    /// Durable message identity.
    pub message_id: MessageId,
    /// Host-selected placement.
    pub placement: QueuePlacement,
    /// Complete content blocks.
    pub content: Vec<Value>,
    /// Whitespace-flattened and code-point-capped preview.
    pub preview: String,
    /// Complete editable text when every block is text.
    pub text: Option<String>,
}

/// Current immutable queue mirror.
#[derive(Default)]
pub struct SessionQueueMirror {
    current: Rc<Vec<QueuedMessage>>,
}

impl SessionQueueMirror {
    /// Reference-stable current queue projection.
    #[must_use]
    pub fn snapshot(&self) -> Rc<Vec<QueuedMessage>> {
        self.current.clone()
    }

    /// Clears a stale connection generation before its replacement baseline.
    pub fn reset(&mut self) -> bool {
        if self.current.is_empty() {
            return false;
        }
        self.current = Rc::new(Vec::new());
        true
    }

    /// Replaces the complete authoritative Host queue snapshot.
    pub fn replace(&mut self, items: &[QueueItemInput]) {
        self.current = Rc::new(
            items
                .iter()
                .map(|item| QueuedMessage {
                    id: item.id.clone(),
                    message_id: item.message_id.clone(),
                    placement: item.placement,
                    content: item.content.clone(),
                    preview: preview_of(&item.content),
                    text: text_of(&item.content),
                })
                .collect(),
        );
    }

    /// Retires exactly one steering row when its durable message enters the log.
    pub fn accept_durable_user_message(&mut self, message_id: &MessageId) -> bool {
        let Some(index) = self.current.iter().position(|item| {
            item.placement == QueuePlacement::Steering && item.message_id == *message_id
        }) else {
            return false;
        };
        self.current = Rc::new(
            self.current
                .iter()
                .enumerate()
                .filter(|(candidate, _)| *candidate != index)
                .map(|(_, item)| item.clone())
                .collect(),
        );
        true
    }
}

fn preview_of(content: &[Value]) -> String {
    let flat = content
        .iter()
        .map(|block| match block.get("type").and_then(Value::as_str) {
            Some("text") => block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            Some(block_type) => format!("[{block_type}]"),
            None => "[unknown]".to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = flat.chars();
    let prefix = chars.by_ref().take(QUEUE_PREVIEW_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn text_of(content: &[Value]) -> Option<String> {
    content
        .iter()
        .map(|block| {
            (block.get("type").and_then(Value::as_str) == Some("text")).then(|| {
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
            })
        })
        .collect::<Option<Vec<_>>>()
        .map(|blocks| blocks.concat())
}
