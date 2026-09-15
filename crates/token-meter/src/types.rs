//! Public configuration and immutable measurement vocabulary.

use seekdeep_llm::TokenUsage;
use serde::{Deserialize, Serialize};

/// Fixed-estimator plugin configuration; no settings are supported.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenMeterConfig {}

/// Anchor from which signed surface movement produces current pressure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum TokenMeasurementBaseline {
    /// No envelope and no visible surface have been observed.
    None {
        /// Always zero.
        tokens: u64,
    },
    /// Complete heuristic envelope-plus-surface price.
    Estimated {
        /// Fixed-density token estimate.
        tokens: u64,
    },
    /// Provider usage for the matching successful request.
    Usage {
        /// Disjoint full provider token total.
        tokens: u64,
        /// Exact provider buckets anchoring the value.
        usage: TokenUsage,
    },
}

impl TokenMeasurementBaseline {
    /// Baseline token count.
    #[must_use]
    pub const fn tokens(&self) -> u64 {
        match self {
            Self::None { tokens } | Self::Estimated { tokens } | Self::Usage { tokens, .. } => {
                *tokens
            }
        }
    }
}

/// One priced node in current model-visible order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TokenSurfaceNode {
    /// Durable sequence number of the surface event.
    pub seq: u64,
    /// Heuristic price of that event's projected message.
    pub tokens: u64,
}

/// Detached request pressure and surface snapshot at one durable revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TokenMeasurement {
    /// Number of consumed events and next unread sequence.
    pub log_revision: u64,
    /// Provider or heuristic anchor.
    pub baseline: TokenMeasurementBaseline,
    /// Signed current-surface repricing relative to the anchor.
    pub surface_delta_tokens: i64,
    /// Non-negative request-and-response pressure.
    pub total_tokens: u64,
    /// Current heuristic surface total.
    pub surface_tokens: u64,
    /// Detached current positional surface.
    pub nodes: Vec<TokenSurfaceNode>,
}
