//! Immutable message values, identity, and construction helpers.

use std::ops::Deref;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::{
    brand::{CallId, MessageId},
    types::{ContentBlock, StreamChunk, is_token_delta},
};

/// Maximum UTF-16 code-unit count for a notice summary.
pub const CONTEXT_SUMMARY_MAX_CHARS: usize = 120;
const ELLIPSIS_UTF16: u16 = 0x2026;

/// One named contribution to a snapshot-form runtime context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextSnapshotSection {
    /// Contributing subsystem name.
    pub name: String,
    /// Exact model-facing text.
    pub text: String,
}

/// Bounds one notice summary and appends a single ellipsis when truncated.
///
/// This native `str` wrapper reconstructs an isolated surrogate as U+FFFD.
/// Foreign-string bindings use [`bound_context_summary_units`] to retain the
/// exact ECMAScript code units.
#[must_use]
pub fn bound_context_summary(summary: &str) -> String {
    let units = summary.encode_utf16().collect::<Vec<_>>();
    String::from_utf16_lossy(&bound_context_summary_units(&units))
}

/// Exact ECMAScript-code-unit form of [`bound_context_summary`], including a
/// lone surrogate when `String.prototype.slice` cuts a valid pair.
#[must_use]
pub fn bound_context_summary_units(summary: &[u16]) -> Vec<u16> {
    if summary.len() <= CONTEXT_SUMMARY_MAX_CHARS {
        return summary.to_vec();
    }
    let mut bounded = summary[..CONTEXT_SUMMARY_MAX_CHARS - 1].to_vec();
    bounded.push(ELLIPSIS_UTF16);
    bounded
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
#[derive(Clone, Debug, PartialEq)]
pub struct MessageSource {
    /// Producer kind.
    pub kind: String,
    /// Kind-specific fields.
    pub fields: Map<String, Value>,
}

impl Serialize for MessageSource {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut object = Map::new();
        object.insert("kind".to_owned(), Value::String(self.kind.clone()));
        object.extend(
            self.fields
                .iter()
                .filter(|(field, _)| field.as_str() != "kind")
                .map(|(field, value)| (field.clone(), value.clone())),
        );
        Value::Object(object).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for MessageSource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let Value::Object(mut object) = Value::deserialize(deserializer)? else {
            return Err(D::Error::custom("message source must be an object"));
        };
        let kind = object
            .remove("kind")
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or_else(|| D::Error::custom("message source kind must be a string"))?;
        Ok(Self {
            kind,
            fields: object,
        })
    }
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
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Message {
    /// Stable identity.
    id: MessageId,
    /// Conversation role.
    role: MessageRole,
    /// Exact model-facing blocks.
    content: Vec<ContentBlock>,
    /// Producer attribution.
    source: MessageSource,
    /// Module-augmented message fields preserved by construction, persistence,
    /// and routing boundaries.
    #[serde(flatten)]
    fields: Map<String, Value>,
}

impl Serialize for Message {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut object = Map::new();
        let role = serde_json::to_value(self.role).map_err(serde::ser::Error::custom)?;
        let content = serde_json::to_value(&self.content).map_err(serde::ser::Error::custom)?;
        let source = serde_json::to_value(&self.source).map_err(serde::ser::Error::custom)?;
        match self.role {
            MessageRole::User if self.source.kind == "tool" => {
                object.insert("source".to_owned(), source);
                object.insert("content".to_owned(), content);
                object.extend(self.fields.clone());
                object.insert("role".to_owned(), role);
            }
            MessageRole::User => {
                object.insert("content".to_owned(), content);
                object.insert("source".to_owned(), source);
                object.extend(self.fields.clone());
                object.insert("role".to_owned(), role);
            }
            MessageRole::System | MessageRole::Assistant => {
                object.insert("role".to_owned(), role);
                object.insert("content".to_owned(), content);
                object.insert("source".to_owned(), source);
                object.extend(self.fields.clone());
            }
        }
        object.insert("id".to_owned(), Value::String(self.id.as_str().to_owned()));
        Value::Object(object).serialize(serializer)
    }
}

/// User-role specialization of the shared immutable message representation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UserMessage(Message);

impl UserMessage {
    /// Creates an identified user-role message.
    #[must_use]
    pub fn new(content: Vec<ContentBlock>, source: MessageSource) -> Self {
        Self(Message::new(MessageRole::User, content, source))
    }

    /// Validates and specializes an existing message without changing identity.
    ///
    /// # Errors
    ///
    /// Returns the original message when it does not have the user role.
    pub fn try_from_message(message: Message) -> Result<Self, Box<Message>> {
        if message.role == MessageRole::User {
            Ok(Self(message))
        } else {
            Err(Box::new(message))
        }
    }

    /// Consumes the specialization and returns the shared representation.
    #[must_use]
    pub fn into_message(self) -> Message {
        self.0
    }
}

impl Deref for UserMessage {
    type Target = Message;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Message {
    /// Stable identity preserved across every representation boundary.
    #[must_use]
    pub const fn id(&self) -> &MessageId {
        &self.id
    }

    /// Provider-neutral conversation role.
    #[must_use]
    pub const fn role(&self) -> MessageRole {
        self.role
    }

    /// Exact model-facing blocks.
    #[must_use]
    pub fn content(&self) -> &[ContentBlock] {
        &self.content
    }

    /// Required producer attribution.
    #[must_use]
    pub const fn source(&self) -> &MessageSource {
        &self.source
    }

    /// Module-augmented message fields.
    #[must_use]
    pub const fn fields(&self) -> &Map<String, Value> {
        &self.fields
    }

    /// Creates an identified message.
    #[must_use]
    pub fn new(role: MessageRole, content: Vec<ContentBlock>, source: MessageSource) -> Self {
        Self::new_with_fields(role, content, source, Map::new())
    }

    /// Creates an identified message while preserving module-augmented fields.
    #[must_use]
    pub fn new_with_fields(
        role: MessageRole,
        content: Vec<ContentBlock>,
        source: MessageSource,
        mut fields: Map<String, Value>,
    ) -> Self {
        remove_reserved_message_fields(&mut fields);
        Self {
            id: MessageId::new(Uuid::new_v4().to_string()),
            role,
            content,
            source,
            fields,
        }
    }

    /// Detaches a complete message that already has a durable identity.
    #[must_use]
    pub fn from_existing(
        id: MessageId,
        role: MessageRole,
        content: Vec<ContentBlock>,
        source: MessageSource,
        mut fields: Map<String, Value>,
    ) -> Self {
        remove_reserved_message_fields(&mut fields);
        Self {
            id,
            role,
            content,
            source,
            fields,
        }
    }

    /// Replaces producer attribution while retaining all other immutable facts.
    #[must_use]
    pub fn with_source(mut self, source: MessageSource) -> Self {
        self.source = source;
        self
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

fn remove_reserved_message_fields(fields: &mut Map<String, Value>) {
    for field in ["id", "role", "content", "source"] {
        fields.remove(field);
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

        let split_pair = format!("{}😀x", "a".repeat(118))
            .encode_utf16()
            .collect::<Vec<_>>();
        let exact = bound_context_summary_units(&split_pair);
        assert_eq!(exact.len(), CONTEXT_SUMMARY_MAX_CHARS);
        assert_eq!(exact[118], 0xd83d);
        assert_eq!(exact[119], ELLIPSIS_UTF16);
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
        assert_eq!(message.role(), MessageRole::User);
        assert_eq!(message.source().fields["callId"], "call-1");
        let ContentBlock::ToolResult { tool_call_id, .. } = &message.content()[0] else {
            panic!("tool result block");
        };
        assert_eq!(tool_call_id.as_str(), "call-1");
    }
}
