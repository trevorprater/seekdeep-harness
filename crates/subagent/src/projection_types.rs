//! Pure client-safe subagent projection vocabulary.

use serde::{Deserialize, Serialize};

/// Durable active-turn timing for one descriptor-backed child session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentTimingProjection {
    /// Milliseconds accumulated across completed turns.
    pub settled_ms: u64,
    /// Same-cut bounds of the currently open turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<SubagentActiveTiming>,
}

/// Same-cut bounds of one open turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentActiveTiming {
    /// Start of the open turn.
    pub since: u64,
    /// Latest event time folded into this projection cut.
    pub through: u64,
}

/// Durable identity of one descriptor-backed subagent session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "mode",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum SubagentIdentityProjection {
    /// A terminal one-shot child.
    #[serde(rename = "one-shot")]
    OneShot {
        /// Optional durable creation label.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// Seq of the folded descriptor event.
        seq: u64,
    },
    /// A resumable conversation.
    Continuable {
        /// Durable creation label.
        label: String,
        /// Seq of the folded descriptor event.
        seq: u64,
    },
}
