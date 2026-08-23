//! Immutable model-context generations reconstructed from surface replacements.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ConversationPromptSnapshot;

/// Operation that started a new append-only model context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConversationContextOriginKind {
    /// Provider compaction replacement.
    Compaction,
    /// Rewind to an earlier durable boundary.
    Rewind,
    /// Explicit context rewrite.
    Rewrite,
}

/// One immutable model-context generation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConversationContext {
    /// Zero-based generation identity within the Session.
    pub id: u64,
    /// Previous generation identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<u64>,
    /// Operation that created this generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ConversationContextOriginKind>,
    /// Replacement event sequence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_seq: Option<u64>,
    /// Replacement Unix epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
    /// Latest request header inherited by this generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<Box<ConversationPromptSnapshot>>,
    /// Frozen historical nodes or current folded tail nodes.
    pub nodes: Vec<Value>,
}
