//! Browser-safe durable retry event vocabulary.

use seekdeep_llm::{LlmFailure, ProviderId};
use serde::{Deserialize, Serialize};

use crate::brand::{RetryId, RetryPolicyKey};

/// Exhaustive retry mode recorded at the durable boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmRetryMode {
    /// Retry selected transient codes within a finite budget.
    Normal,
    /// Retry every request failure until cancellation or disposal.
    Always,
}

/// Durable payload written before one provider-routed backoff.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
pub enum LlmRetryEventData {
    /// One bounded retry attempt.
    Normal {
        /// Retry-chain identity.
        #[serde(rename = "retryId")]
        retry_id: RetryId,
        /// Owning turn.
        turn: u64,
        /// Owning step.
        step: u64,
        /// Provider route that served the failed request.
        provider: ProviderId,
        /// Canonical resolved-policy identity.
        #[serde(rename = "policyKey")]
        policy_key: RetryPolicyKey,
        /// One-based attempt number inside the provider-policy chain.
        retry: u64,
        /// Finite maximum number of retries.
        #[serde(rename = "maxRetries")]
        max_retries: u64,
        /// Scheduled wait in milliseconds.
        #[serde(rename = "delayMs")]
        delay_ms: f64,
        /// Complete provider-neutral failure.
        failure: LlmFailure,
    },
    /// One unbounded retry attempt.
    Always {
        /// Retry-chain identity.
        #[serde(rename = "retryId")]
        retry_id: RetryId,
        /// Owning turn.
        turn: u64,
        /// Owning step.
        step: u64,
        /// Provider route that served the failed request.
        provider: ProviderId,
        /// Canonical resolved-policy identity.
        #[serde(rename = "policyKey")]
        policy_key: RetryPolicyKey,
        /// One-based attempt number inside the provider-policy chain.
        retry: u64,
        /// Scheduled wait in milliseconds.
        #[serde(rename = "delayMs")]
        delay_ms: f64,
        /// Complete provider-neutral failure.
        failure: LlmFailure,
    },
}

impl LlmRetryEventData {
    /// Retry-chain identity.
    #[must_use]
    pub const fn retry_id(&self) -> &RetryId {
        match self {
            Self::Normal { retry_id, .. } | Self::Always { retry_id, .. } => retry_id,
        }
    }

    /// Owning turn.
    #[must_use]
    pub const fn turn(&self) -> u64 {
        match self {
            Self::Normal { turn, .. } | Self::Always { turn, .. } => *turn,
        }
    }

    /// Owning step.
    #[must_use]
    pub const fn step(&self) -> u64 {
        match self {
            Self::Normal { step, .. } | Self::Always { step, .. } => *step,
        }
    }

    /// Provider route.
    #[must_use]
    pub const fn provider(&self) -> &ProviderId {
        match self {
            Self::Normal { provider, .. } | Self::Always { provider, .. } => provider,
        }
    }

    /// Resolved-policy identity.
    #[must_use]
    pub const fn policy_key(&self) -> &RetryPolicyKey {
        match self {
            Self::Normal { policy_key, .. } | Self::Always { policy_key, .. } => policy_key,
        }
    }

    /// One-based retry number.
    #[must_use]
    pub const fn retry(&self) -> u64 {
        match self {
            Self::Normal { retry, .. } | Self::Always { retry, .. } => *retry,
        }
    }

    /// Recorded mode.
    #[must_use]
    pub const fn mode(&self) -> LlmRetryMode {
        match self {
            Self::Normal { .. } => LlmRetryMode::Normal,
            Self::Always { .. } => LlmRetryMode::Always,
        }
    }
}

/// Durable transition written after a retry wait completes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LlmRetryStartedEventData {
    /// Retry-chain identity.
    pub retry_id: RetryId,
    /// Owning turn.
    pub turn: u64,
    /// Owning step.
    pub step: u64,
    /// One-based retry number.
    pub retry: u64,
}
