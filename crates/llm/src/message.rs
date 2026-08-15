//! Immutable message values, identity, and construction helpers.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::{
    brand::{CallId, MessageId},
    types::{ContentBlock, StreamChunk, is_token_delta},
};

/// Maximum UTF-16 code-unit count for a notice summary.
pub const CONTEXT_SUMMARY_MAX_CHARS: usize = 120;

/// Bounds one notice summary and appends a single ellipsis when truncated.
#[must_use]
pub fn bound_context_summary(summary: &str) -> String {
    if summary.encode_utf16().count() <= CONTEXT_SUMMARY_MAX_CHARS {
        return summary.to_owned();
    }
    let mut used = 0;
    let mut boundary = 0;
    for (index, character) in summary.char_indices() {
        let width = character.len_utf16();
        if used + width > CONTEXT_SUMMARY_MAX_CHARS - 1 {
            break;
        }
        used += width;
        boundary = index + character.len_utf8();
    }
    format!("{}…", &summary[..boundary])
}

/// Provider-neutral conversation role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    /// System instruction.
    System,
    /// Human, context, or tool input.
    User,
    /// Model output.
    Assistant,
}

/// Merge-extensible producer attribution retained as lossless JSON fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageSource {
    /// Producer kind.
    pub kind: String,
    /// Kind-specific fields.
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

impl MessageSource {
    /// Direct human input.
    #[must_use]
    pub fn user() -> Self {
        Self {
            kind: "user".to_owned(),
            fields: Map::new(),
        }
    }

    /// Plugin-produced context.
    #[must_use]
    pub fn plugin(plugin: impl Into<String>) -> Self {
        let mut fields = Map::new();
        fields.insert("plugin".to_owned(), Value::String(plugin.into()));
        Self {
            kind: "plugin".to_owned(),
            fields,
        }
    }

    /// Routed model output.
    #[must_use]
    pub fn model(provider: impl Into<String>, model: impl Into<String>) -> Self {
        let mut fields = Map::new();
        fields.insert("provider".to_owned(), Value::String(provider.into()));
        fields.insert("model".to_owned(), Value::String(model.into()));
        Self {
            kind: "model".to_owned(),
            fields,
        }
    }

    /// Tool result input.
    #[must_use]
    pub fn tool(call_id: &CallId) -> Self {
        let mut fields = Map::new();
        fields.insert(
            "callId".to_owned(),
            Value::String(call_id.as_str().to_owned()),
        );
        Self {
            kind: "tool".to_owned(),
            fields,
        }
    }
}

/// One immutable identified provider-neutral message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Stable identity.
    pub id: MessageId,
    /// Conversation role.
    pub role: MessageRole,
    /// Exact model-facing blocks.
    pub content: Vec<ContentBlock>,
    /// Producer attribution.
    pub source: MessageSource,
}

impl Message {
    /// Creates an identified message.
    #[must_use]
    pub fn new(role: MessageRole, content: Vec<ContentBlock>, source: MessageSource) -> Self {
        Self {
            id: MessageId::new(Uuid::new_v4().to_string()),
            role,
            content,
            source,
        }
    }

    /// Creates direct or plugin-produced user input.
    #[must_use]
    pub fn user(content: Vec<ContentBlock>, source: MessageSource) -> Self {
        Self::new(MessageRole::User, content, source)
    }

    /// Creates routed model output and fixes the assistant role.
    #[must_use]
    pub fn assistant(
        content: Vec<ContentBlock>,
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self::new(
            MessageRole::Assistant,
            content,
            MessageSource::model(provider, model),
        )
    }

    /// Creates one user-role tool result with call correlation in source and content.
    #[must_use]
    pub fn tool_result(call_id: &CallId, content: Vec<ContentBlock>, is_error: bool) -> Self {
        Self::user(
            vec![ContentBlock::ToolResult {
                tool_call_id: call_id.clone(),
                content,
                is_error: Some(is_error),
            }],
            MessageSource::tool(call_id),
        )
    }
}

/// Re-export of the canonical first-token predicate beside message helpers.
#[must_use]
pub fn chunk_is_token_delta(chunk: &StreamChunk) -> bool {
    is_token_delta(chunk)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_summary_uses_utf16_bound() {
        let summary = "a".repeat(CONTEXT_SUMMARY_MAX_CHARS + 10);
        let bounded = bound_context_summary(&summary);
        assert_eq!(bounded.encode_utf16().count(), CONTEXT_SUMMARY_MAX_CHARS);
        assert!(bounded.ends_with('…'));
    }

    #[test]
    fn tool_result_reuses_call_identity() {
        let message = Message::tool_result(
            &CallId::new("call-1"),
            vec![ContentBlock::Text {
                text: "done".to_owned(),
            }],
            false,
        );
        assert_eq!(message.role, MessageRole::User);
        assert_eq!(message.source.fields["callId"], "call-1");
        let ContentBlock::ToolResult { tool_call_id, .. } = &message.content[0] else {
            panic!("tool result block");
        };
        assert_eq!(tool_call_id.as_str(), "call-1");
    }
}
