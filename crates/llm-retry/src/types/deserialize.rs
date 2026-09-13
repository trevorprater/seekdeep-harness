use seekdeep_core::session::JsonValue;
use seekdeep_llm::{LlmFailure, ProviderId};
use serde::{
    Deserialize, Deserializer,
    de::{DeserializeOwned, Error as _, IgnoredAny},
};

use super::{LlmRetryEventData, LlmRetryMode, RetryId, RetryPolicyKey};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RetryPayload {
    retry_id: RetryId,
    turn: u64,
    step: u64,
    provider: ProviderId,
    policy_key: RetryPolicyKey,
    retry: u64,
    delay_ms: f64,
    failure: LlmFailure,
    #[serde(rename = "mode")]
    _mode: LlmRetryMode,
    #[serde(default, rename = "maxRetries")]
    _max_retries: Option<IgnoredAny>,
}

impl<'de> Deserialize<'de> for LlmRetryEventData {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <JsonValue as Deserialize>::deserialize(deserializer)?;
        let mode = required_field(&value, "mode")?;
        if mode == LlmRetryMode::Always && value.get("maxRetries").is_some() {
            return Err(D::Error::unknown_field(
                "maxRetries",
                &[
                    "retryId",
                    "turn",
                    "step",
                    "provider",
                    "policyKey",
                    "retry",
                    "delayMs",
                    "failure",
                ],
            ));
        }
        let payload: RetryPayload = value.deserialize().map_err(D::Error::custom)?;
        Ok(match mode {
            LlmRetryMode::Normal => Self::Normal {
                retry_id: payload.retry_id,
                turn: payload.turn,
                step: payload.step,
                provider: payload.provider,
                policy_key: payload.policy_key,
                retry: payload.retry,
                max_retries: required_field(&value, "maxRetries")?,
                delay_ms: payload.delay_ms,
                failure: payload.failure,
            },
            LlmRetryMode::Always => Self::Always {
                retry_id: payload.retry_id,
                turn: payload.turn,
                step: payload.step,
                provider: payload.provider,
                policy_key: payload.policy_key,
                retry: payload.retry,
                delay_ms: payload.delay_ms,
                failure: payload.failure,
            },
        })
    }
}

fn required_field<T: DeserializeOwned, E: serde::de::Error>(
    value: &JsonValue,
    field: &'static str,
) -> Result<T, E> {
    value
        .get(field)
        .ok_or_else(|| E::missing_field(field))?
        .deserialize()
        .map_err(E::custom)
}
