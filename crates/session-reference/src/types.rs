//! Public session-reference request, candidate, and preparation records.

use seekdeep_core::session::SessionId;
use seekdeep_llm::{ContentBlock, UserMessage};
use serde::{Deserialize, Serialize};

/// The stable source discriminator for referenced-session context.
pub const SESSION_REFERENCE_SOURCE_KIND: &str = "session-reference";

/// Role of one text-only projected conversation item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReferencedConversationRole {
    /// Original human message.
    User,
    /// Original model message.
    Assistant,
}

/// Text-only projected conversation item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferencedConversationItem {
    /// Original message role.
    pub role: ReferencedConversationRole,
    /// Visible text retained from that message.
    pub text: String,
}

/// One source session selected by a host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionReferenceInput {
    /// Opaque source session identity.
    pub session_id: SessionId,
    /// Optional user-facing mention label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// One host-facing candidate from exact session metadata.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReferenceCandidate {
    /// Opaque source session identity.
    pub session_id: SessionId,
    /// Latest log-backed title, falling back to the opaque session id.
    pub label: String,
    /// Source session working directory, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Source session creation time in Unix epoch milliseconds.
    pub created_at: u64,
}

/// Per-reference retention facts serialized into the source envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReferenceFact {
    /// Durable source session id.
    pub session_id: SessionId,
    /// Display label.
    pub label: String,
    /// Highest captured log seq, or none for an empty log.
    pub captured_through_seq: Option<u64>,
    /// Whether the snapshot carried a compaction checkpoint.
    pub compacted: bool,
    /// Messages in the projected source conversation.
    pub original_messages: u64,
    /// Messages retained after budget fitting.
    pub retained_messages: u64,
    /// Messages dropped during budget fitting.
    pub omitted_messages: u64,
    /// UTF-8 bytes omitted during budget fitting.
    pub omitted_bytes: u64,
    /// Whether any content was omitted or truncated.
    pub truncated: bool,
    /// Reference position in the message's input order.
    pub input_index: u64,
}

/// Durable source-session snapshot facts carried by the message source.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionReferenceSource {
    /// Source form; always recall.
    pub form: String,
    /// Structural version; always 1.
    pub version: u64,
    /// Per-reference retention facts in input order.
    pub references: Vec<SessionReferenceFact>,
}

/// Direct message content and optional referenced-session context.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedReferencedMessage {
    /// Readable message content after host mention tokens are removed.
    pub content: Vec<ContentBlock>,
    /// Aggregated untrusted snapshot, absent when the message has no references.
    pub additional_context: Option<UserMessage>,
}
