//! Pure client-safe token projection values.

use serde::{Deserialize, Serialize};

/// Durable cumulative provider usage with disjoint buckets.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TokenUsageProjection {
    /// Uncached provider input.
    pub uncached_input_tokens: u64,
    /// Provider output, already including reasoning.
    pub output_tokens: u64,
    /// Cache-hit input.
    pub cache_read_tokens: u64,
    /// Cache-populated input.
    pub cache_write_tokens: u64,
}

/// Approximate next-request occupancy and newest route capacity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextPressureProjection {
    /// Prompt-side provider usage of the newest sampled request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure_tokens: Option<u64>,
    /// Provider anchor carried over signed current-surface movement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projected_tokens: Option<u64>,
    /// Newest advertised model capacity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
}

/// Fixed-heuristic composition of the next request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContextBreakdownProjection {
    /// Newest canonical envelope's system prompt.
    pub system_tokens: u64,
    /// Newest canonical envelope's tool schemas.
    pub tools_tokens: u64,
    /// Current model-visible conversation surface.
    pub message_tokens: u64,
}
