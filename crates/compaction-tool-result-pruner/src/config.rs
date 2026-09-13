//! Configuration resolution for deterministic tool-result pruning.

use crate::types::{ResolvedConfig, ToolResultPruneConfig};

/// Fixed marker substituted for every removed middle span.
pub const PRUNE_MARKER: &str = "\n\n[... tool result middle pruned ...]\n\n";

/// Low-friction defaults for coding-agent tool output.
pub const DEFAULTS: ResolvedConfig = ResolvedConfig {
    threshold_chars: 8192,
    head_chars: 4096,
    tail_chars: 1024,
};

/// Counts Unicode code points without splitting surrogate pairs.
#[must_use]
pub fn code_point_length(text: &str) -> usize {
    text.chars().count()
}

/// Resolves and validates pruning budgets.
///
/// # Errors
///
/// Returns a failure when the emitted head + marker + tail budget exceeds the
/// pruning threshold.
pub fn resolve_config(config: &ToolResultPruneConfig) -> anyhow::Result<ResolvedConfig> {
    let threshold = config
        .threshold_chars
        .unwrap_or(DEFAULTS.threshold_chars as u64);
    let head = config.head_chars.unwrap_or(DEFAULTS.head_chars as u64);
    let tail = config.tail_chars.unwrap_or(DEFAULTS.tail_chars as u64);
    if threshold < 1 {
        anyhow::bail!(
            "ToolResultPruneConfig: thresholdChars ({threshold}) must be a positive integer"
        );
    }
    let emitted = head
        .saturating_add(code_point_length(PRUNE_MARKER) as u64)
        .saturating_add(tail);
    if emitted > threshold {
        anyhow::bail!(
            "ToolResultPruneConfig: headChars + marker + tailChars ({emitted}) must be at most thresholdChars ({threshold})"
        );
    }
    Ok(ResolvedConfig {
        threshold_chars: usize::try_from(threshold).unwrap_or(usize::MAX),
        head_chars: usize::try_from(head).unwrap_or(usize::MAX),
        tail_chars: usize::try_from(tail).unwrap_or(usize::MAX),
    })
}
