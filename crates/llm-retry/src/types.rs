//! Browser-safe durable retry event vocabulary.

use num_traits::ToPrimitive as _;
use seekdeep_llm::{LlmFailure, ProviderId};
use serde::{Deserialize, Serialize, Serializer, ser::SerializeMap as _};

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
#[derive(Clone, Debug, PartialEq, Deserialize)]
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

impl Serialize for LlmRetryEventData {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Normal {
                retry_id,
                turn,
                step,
                provider,
                policy_key,
                retry,
                max_retries,
                delay_ms,
                failure,
            } => {
                let mut map = serializer.serialize_map(Some(10))?;
                map.serialize_entry("retryId", retry_id)?;
                map.serialize_entry("turn", turn)?;
                map.serialize_entry("step", step)?;
                map.serialize_entry("provider", provider)?;
                map.serialize_entry("mode", "normal")?;
                map.serialize_entry("policyKey", policy_key)?;
                map.serialize_entry("retry", retry)?;
                map.serialize_entry("maxRetries", max_retries)?;
                map.serialize_entry("delayMs", &JavascriptNumber(*delay_ms))?;
                map.serialize_entry("failure", failure)?;
                map.end()
            }
            Self::Always {
                retry_id,
                turn,
                step,
                provider,
                policy_key,
                retry,
                delay_ms,
                failure,
            } => {
                let mut map = serializer.serialize_map(Some(9))?;
                map.serialize_entry("retryId", retry_id)?;
                map.serialize_entry("turn", turn)?;
                map.serialize_entry("step", step)?;
                map.serialize_entry("provider", provider)?;
                map.serialize_entry("mode", "always")?;
                map.serialize_entry("policyKey", policy_key)?;
                map.serialize_entry("retry", retry)?;
                map.serialize_entry("delayMs", &JavascriptNumber(*delay_ms))?;
                map.serialize_entry("failure", failure)?;
                map.end()
            }
        }
    }
}

struct JavascriptNumber(f64);

impl Serialize for JavascriptNumber {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if self.0 == 0.0 {
            return serializer.serialize_u64(0);
        }
        if self.0.is_finite()
            && self.0 >= 0.0
            && self.0.fract() == 0.0
            && let Some(value) = self.0.to_u64()
        {
            return serializer.serialize_u64(value);
        }
        serializer.serialize_f64(self.0)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_event_uses_source_json_field_order_and_number_rendering() {
        let event = LlmRetryEventData::Normal {
            retry_id: RetryId::new("chain"),
            turn: 1,
            step: 2,
            provider: ProviderId::new("mock"),
            policy_key: RetryPolicyKey::new("policy"),
            retry: 1,
            max_retries: 2,
            delay_ms: 1.0,
            failure: LlmFailure {
                message: "busy".to_owned(),
                code: "RATE_LIMIT".to_owned(),
                status: None,
                provider_retry_after_ms: None,
                request_id: None,
            },
        };
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            r#"{"retryId":"chain","turn":1,"step":2,"provider":"mock","mode":"normal","policyKey":"policy","retry":1,"maxRetries":2,"delayMs":1,"failure":{"message":"busy","code":"RATE_LIMIT"}}"#
        );
    }
}
