//! Character-budget policy and accounting types for tool-result pruning.

use seekdeep_llm::CallId;
use serde::{Deserialize, Serialize};

/// Character-budget policy for deterministic tool-result pruning.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct ToolResultPruneConfig {
    /// Prune when total text exceeds this many Unicode code points.
    pub threshold_chars: Option<u64>,
    /// Maximum leading Unicode code points retained.
    pub head_chars: Option<u64>,
    /// Maximum trailing Unicode code points retained.
    pub tail_chars: Option<u64>,
}

/// Validated, detached, deeply immutable pruning configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedConfig {
    /// Pruning threshold in Unicode code points.
    pub threshold_chars: usize,
    /// Leading Unicode code points retained.
    pub head_chars: usize,
    /// Trailing Unicode code points retained.
    pub tail_chars: usize,
}

/// Cited source event and size accounting for one landed surface replacement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrunedEntry {
    /// Full-fidelity tool-result event shadowed by the replacement.
    pub original_seq: u64,
    /// Newly appended pruned tool-result event.
    pub replacement_seq: u64,
    /// Tool call shared by the original and replacement.
    pub call_id: CallId,
    /// Original text size in Unicode code points.
    pub chars_before: usize,
    /// Replacement text size in Unicode code points.
    pub chars_after: usize,
}

/// Aggregate outcome of one stable-surface pruning pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PruneResult {
    /// Replacements in the snapshotted surface order.
    pub pruned: Vec<PrunedEntry>,
    /// Total Unicode code points removed across replacements.
    pub chars_removed: usize,
}
