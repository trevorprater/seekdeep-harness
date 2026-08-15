//! Provider-owned request retry policy resolution.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::EMPTY_RESPONSE_CODE;

/// Largest millisecond delay scheduled without platform timer clamping.
pub const MAX_TIMER_DELAY_MS: f64 = 2_147_483_647.0;
const MAX_TIMER_DELAY_MS_INTEGER: u64 = 2_147_483_647;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
const DEFAULT_MAX_RETRIES: u64 = 2;
const DEFAULT_INITIAL_DELAY_MS: f64 = 500.0;
const DEFAULT_MAX_DELAY_MS: f64 = 10_000.0;
const DEFAULT_JITTER_RATIO: f64 = 0.1;
const DEFAULT_RETRYABLE_CODES: [&str; 5] = [
    EMPTY_RESPONSE_CODE,
    "RATE_LIMIT",
    "SERVER",
    "TIMEOUT",
    "TRANSPORT",
];

/// Fully resolved bounded or unbounded provider retry policy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum ResolvedRetryPolicy {
    /// Retry only configured transient failure codes.
    Normal {
        /// Maximum eligible retries after the first request.
        #[serde(rename = "maxRetries")]
        max_retries: u64,
        /// Stable eligible failure codes.
        #[serde(rename = "retryableCodes")]
        retryable_codes: Vec<String>,
        /// Initial exponential delay.
        #[serde(rename = "initialDelayMs")]
        initial_delay_ms: f64,
        /// Largest local or accepted provider delay.
        #[serde(rename = "maxDelayMs")]
        max_delay_ms: f64,
        /// Symmetric random multiplier range around one.
        #[serde(rename = "jitterRatio")]
        jitter_ratio: f64,
    },
    /// Retry every request failure until cancellation or disposal.
    Always {
        /// Initial exponential delay.
        #[serde(rename = "initialDelayMs")]
        initial_delay_ms: f64,
        /// Largest local or accepted provider delay.
        #[serde(rename = "maxDelayMs")]
        max_delay_ms: f64,
        /// Symmetric random multiplier range around one.
        #[serde(rename = "jitterRatio")]
        jitter_ratio: f64,
    },
}

/// Validates, defaults, and detaches one provider policy from JSON configuration.
///
/// # Errors
///
/// Returns a path-qualified error for unknown keys or invalid bounds.
pub fn resolve_retry_policy(
    config: Option<&Value>,
    path: &str,
) -> anyhow::Result<ResolvedRetryPolicy> {
    let Some(config) = config else {
        let backoff = resolve_backoff(None, &format!("{path}.backoff"))?;
        return Ok(ResolvedRetryPolicy::Normal {
            max_retries: DEFAULT_MAX_RETRIES,
            retryable_codes: default_codes(),
            initial_delay_ms: backoff.0,
            max_delay_ms: backoff.1,
            jitter_ratio: backoff.2,
        });
    };
    let object = config
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("{path} must be an object"))?;
    let mode = object
        .get("mode")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{path}.mode must be \"normal\" or \"always\""))?;
    match mode {
        "normal" => {
            validate_keys(
                object,
                &["mode", "maxRetries", "retryableCodes", "backoff"],
                path,
            )?;
            let max_retries = match object.get("maxRetries") {
                None => DEFAULT_MAX_RETRIES,
                Some(value) => {
                    let number = value.as_f64().unwrap_or(f64::NAN);
                    anyhow::ensure!(
                        number.is_finite()
                            && number.fract() == 0.0
                            && (0.0..=MAX_SAFE_INTEGER).contains(&number),
                        "{path}.maxRetries must be a non-negative safe integer"
                    );
                    format!("{number:.0}").parse::<u64>().map_err(|_| {
                        anyhow::anyhow!("{path}.maxRetries must be a non-negative safe integer")
                    })?
                }
            };
            let retryable_codes = match object.get("retryableCodes") {
                None => default_codes(),
                Some(Value::Array(values)) => values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .filter(|code| !code.is_empty())
                            .map(str::to_owned)
                    })
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(|| {
                        anyhow::anyhow!("{path}.retryableCodes must contain only non-empty strings")
                    })?,
                Some(_) => {
                    anyhow::bail!("{path}.retryableCodes must contain only non-empty strings")
                }
            };
            anyhow::ensure!(
                !retryable_codes.is_empty(),
                "{path}.retryableCodes must not be empty"
            );
            anyhow::ensure!(
                retryable_codes.iter().collect::<HashSet<_>>().len() == retryable_codes.len(),
                "{path}.retryableCodes must not contain duplicates"
            );
            let backoff = resolve_backoff(object.get("backoff"), &format!("{path}.backoff"))?;
            Ok(ResolvedRetryPolicy::Normal {
                max_retries,
                retryable_codes,
                initial_delay_ms: backoff.0,
                max_delay_ms: backoff.1,
                jitter_ratio: backoff.2,
            })
        }
        "always" => {
            validate_keys(object, &["mode", "backoff"], path)?;
            let backoff = resolve_backoff(object.get("backoff"), &format!("{path}.backoff"))?;
            Ok(ResolvedRetryPolicy::Always {
                initial_delay_ms: backoff.0,
                max_delay_ms: backoff.1,
                jitter_ratio: backoff.2,
            })
        }
        _ => anyhow::bail!("{path}.mode must be \"normal\" or \"always\""),
    }
}

fn resolve_backoff(config: Option<&Value>, path: &str) -> anyhow::Result<(f64, f64, f64)> {
    let object = match config {
        None => None,
        Some(Value::Object(object)) => Some(object),
        Some(_) => anyhow::bail!("{path} must be an object"),
    };
    if let Some(object) = object {
        validate_keys(
            object,
            &["initialDelayMs", "maxDelayMs", "jitterRatio"],
            path,
        )?;
    }
    let number = |key: &str, default: f64| {
        object
            .and_then(|value| value.get(key))
            .and_then(Value::as_f64)
            .unwrap_or(default)
    };
    let initial = number("initialDelayMs", DEFAULT_INITIAL_DELAY_MS);
    let maximum = number("maxDelayMs", DEFAULT_MAX_DELAY_MS);
    let jitter = number("jitterRatio", DEFAULT_JITTER_RATIO);
    anyhow::ensure!(
        initial.is_finite() && initial > 0.0 && initial <= MAX_TIMER_DELAY_MS,
        "{path}.initialDelayMs must be a positive finite number no greater than {MAX_TIMER_DELAY_MS_INTEGER}"
    );
    anyhow::ensure!(
        maximum.is_finite() && maximum > 0.0 && maximum <= MAX_TIMER_DELAY_MS,
        "{path}.maxDelayMs must be a positive finite number no greater than {MAX_TIMER_DELAY_MS_INTEGER}"
    );
    anyhow::ensure!(
        initial <= maximum,
        "{path}.initialDelayMs must be less than or equal to maxDelayMs"
    );
    anyhow::ensure!(
        jitter.is_finite() && (0.0..=1.0).contains(&jitter),
        "{path}.jitterRatio must be between 0 and 1"
    );
    Ok((initial, maximum, jitter))
}

fn validate_keys(object: &Map<String, Value>, allowed: &[&str], path: &str) -> anyhow::Result<()> {
    for key in object.keys() {
        anyhow::ensure!(
            allowed.contains(&key.as_str()),
            "{path}: unknown key \"{key}\""
        );
    }
    Ok(())
}

fn default_codes() -> Vec<String> {
    DEFAULT_RETRYABLE_CODES
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn defaults_match_provider_policy() {
        assert_eq!(
            serde_json::to_value(
                resolve_retry_policy(None, "provider.retryPolicy").expect("defaults")
            )
            .expect("serialize"),
            json!({
                "mode": "normal",
                "maxRetries": 2,
                "retryableCodes": ["EMPTY_RESPONSE", "RATE_LIMIT", "SERVER", "TIMEOUT", "TRANSPORT"],
                "initialDelayMs": 500.0,
                "maxDelayMs": 10000.0,
                "jitterRatio": 0.1
            })
        );
    }

    #[test]
    fn rejects_unknown_and_invalid_configuration() {
        let unknown = json!({"mode": "normal", "maxRetires": 1});
        assert!(
            resolve_retry_policy(Some(&unknown), "policy")
                .is_err_and(|error| error.to_string().contains("unknown key \"maxRetires\""))
        );
        let duplicate = json!({"mode": "normal", "retryableCodes": ["SERVER", "SERVER"]});
        assert!(
            resolve_retry_policy(Some(&duplicate), "policy")
                .is_err_and(|error| error.to_string().contains("duplicates"))
        );
    }
}
