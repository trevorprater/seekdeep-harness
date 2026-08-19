//! Compaction vocabulary: the stable transaction identity, the result shape,
//! and the backend-independent checkpoint provenance marker.

use seekdeep_commands::CommandId;
use seekdeep_llm::{ContentBlock, MessageSource};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

seekdeep_util::string_brand!(
    /// Stable identity shared by one compact start/summary/checkpoint/end transaction.
    pub struct CompactionId;
);

/// Backend-independent checkpoint marker plugin carried by checkpoint sources.
pub const COMPACT_CHECKPOINT_PLUGIN: &str = "compact";

/// The surface-boundary pair shadowed by one compaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShadowedRange {
    /// First surface-node seq of the replaced range.
    pub start: u64,
    /// Last surface-node seq of the replaced range.
    pub end: u64,
}

/// Result of a successful compaction operation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompactionResult {
    /// Stable identity shared by this compaction's complete durable lifecycle.
    pub compaction_id: CompactionId,
    /// Human command that initiated this compaction, when it was manual.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_command_id: Option<CommandId>,
    /// The seq of the appended compaction/start event.
    pub start_seq: u64,
    /// The seq of the appended compaction/summary event.
    pub summary_seq: u64,
    /// The seq of the appended compaction/end event.
    pub end_seq: u64,
    /// The summary content blocks produced by the backend.
    pub summary: Vec<ContentBlock>,
    /// The surface-boundary pair that was shadowed.
    pub shadowed_range: ShadowedRange,
    /// The seqs of all shadowed surface nodes, in surface order.
    pub shadowed_seqs: Vec<u64>,
    /// Estimated token count of the shadowed content.
    pub shadowed_token_count: u64,
}

/// Creates checkpoint provenance correlated with one compaction transaction.
#[must_use]
pub fn compact_checkpoint_source(
    compaction_id: &CompactionId,
    source_command_id: Option<&CommandId>,
) -> MessageSource {
    let mut fields = Map::new();
    fields.insert("plugin".to_owned(), json!(COMPACT_CHECKPOINT_PLUGIN));
    fields.insert("compactionId".to_owned(), json!(compaction_id.as_str()));
    if let Some(command_id) = source_command_id {
        fields.insert("sourceCommandId".to_owned(), json!(command_id.as_str()));
    }
    MessageSource {
        kind: "plugin".to_owned(),
        fields,
    }
}

/// Tests whether a persisted message source identifies a compaction checkpoint.
#[must_use]
pub fn is_compact_checkpoint_source(source: &MessageSource) -> bool {
    source.kind == "plugin"
        && source.fields.get("plugin").and_then(Value::as_str) == Some(COMPACT_CHECKPOINT_PLUGIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_source_round_trips_marker_and_correlation() {
        let compaction_id = CompactionId::new("c1");
        let command_id = CommandId::new("cmd1");
        let source = compact_checkpoint_source(&compaction_id, Some(&command_id));
        assert!(is_compact_checkpoint_source(&source));
        assert_eq!(source.fields["compactionId"], "c1");
        assert_eq!(source.fields["sourceCommandId"], "cmd1");
    }

    #[test]
    fn checkpoint_predicate_rejects_non_compact_sources() {
        let plugin = MessageSource::plugin("other");
        assert!(!is_compact_checkpoint_source(&plugin));
        let user = MessageSource::user();
        assert!(!is_compact_checkpoint_source(&user));
        let bare = compact_checkpoint_source(&CompactionId::new("c2"), None);
        assert!(is_compact_checkpoint_source(&bare));
        assert!(!bare.fields.contains_key("sourceCommandId"));
    }
}
