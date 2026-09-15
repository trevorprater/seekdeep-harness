//! Provider-owned request retry policy resolution.

use std::{collections::HashSet, sync::OnceLock};

use seekdeep_schemastery::Schema;
use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use serde_json::{Map, Value, json};

use crate::error::EMPTY_RESPONSE_CODE;

/// Largest millisecond delay scheduled without platform timer clamping.
pub const MAX_TIMER_DELAY_MS: f64 = 2_147_483_647.0;
const MAX_TIMER_DELAY_MS_INTEGER: u64 = 2_147_483_647;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
const MAX_SAFE_INTEGER_U64: u64 = 9_007_199_254_740_991;
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

/// Cordis configuration schema embedded by concrete provider settings.
///
/// The process-local schema node is initialized once so every consumer sees
/// the same identity, matching the source's exported `RetryPolicySchema`
/// singleton.
#[must_use]
pub fn retry_policy_schema() -> Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA
        .get_or_init(|| {
            let backoff = || {
                Schema::object([
                    (
                        "initialDelayMs",
                        Schema::number()
                            .max(MAX_TIMER_DELAY_MS)
                            .with_default(DEFAULT_INITIAL_DELAY_MS),
                    ),
                    (
                        "maxDelayMs",
                        Schema::number()
                            .max(MAX_TIMER_DELAY_MS)
                            .with_default(DEFAULT_MAX_DELAY_MS),
                    ),
                    (
                        "jitterRatio",
                        Schema::number()
                            .min(0.0)
                            .max(1.0)
                            .with_default(DEFAULT_JITTER_RATIO),
                    ),
                ])
            };
            let defaults = DEFAULT_RETRYABLE_CODES
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            Schema::union([
                Schema::object([
                    ("mode", Schema::constant("normal")),
                    (
                        "maxRetries",
                        Schema::number()
                            .step(1.0)
                            .min(0.0)
                            .max(MAX_SAFE_INTEGER)
                            .with_default(DEFAULT_MAX_RETRIES),
                    ),
                    (
                        "retryableCodes",
                        Schema::array(Schema::string()).with_default(json!(defaults)),
                    ),
                    ("backoff", backoff()),
                ]),
                Schema::object([("mode", Schema::constant("always")), ("backoff", backoff())]),
            ])
        })
        .clone()
}

/// Fully resolved bounded or unbounded provider retry policy.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ResolvedRetryPolicy(ResolvedRetryPolicyValue);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase", deny_unknown_fields)]
enum ResolvedRetryPolicyValue {
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

/// Exhaustive mode of a resolved provider retry policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RetryPolicyMode {
    /// Retry only configured transient failures, up to a fixed count.
    Normal,
    /// Retry every request failure until cancellation or disposal.
    Always,
}

impl ResolvedRetryPolicy {
    /// Exhaustive retry mode.
    #[must_use]
    pub const fn mode(&self) -> RetryPolicyMode {
        match &self.0 {
            ResolvedRetryPolicyValue::Normal { .. } => RetryPolicyMode::Normal,
            ResolvedRetryPolicyValue::Always { .. } => RetryPolicyMode::Always,
        }
    }

    /// Maximum eligible retries for normal mode.
    #[must_use]
    pub const fn max_retries(&self) -> Option<u64> {
        match &self.0 {
            ResolvedRetryPolicyValue::Normal { max_retries, .. } => Some(*max_retries),
            ResolvedRetryPolicyValue::Always { .. } => None,
        }
    }

    /// Stable eligible failure codes for normal mode.
    #[must_use]
    pub fn retryable_codes(&self) -> Option<&[String]> {
        match &self.0 {
            ResolvedRetryPolicyValue::Normal {
                retryable_codes, ..
            } => Some(retryable_codes),
            ResolvedRetryPolicyValue::Always { .. } => None,
        }
    }

    /// Initial exponential delay in milliseconds.
    #[must_use]
    pub const fn initial_delay_ms(&self) -> f64 {
        match &self.0 {
            ResolvedRetryPolicyValue::Normal {
                initial_delay_ms, ..
            }
            | ResolvedRetryPolicyValue::Always {
                initial_delay_ms, ..
            } => *initial_delay_ms,
        }
    }

    /// Largest local or accepted provider delay in milliseconds.
    #[must_use]
    pub const fn max_delay_ms(&self) -> f64 {
        match &self.0 {
            ResolvedRetryPolicyValue::Normal { max_delay_ms, .. }
            | ResolvedRetryPolicyValue::Always { max_delay_ms, .. } => *max_delay_ms,
        }
    }

    /// Symmetric random multiplier range around one.
    #[must_use]
    pub const fn jitter_ratio(&self) -> f64 {
        match &self.0 {
            ResolvedRetryPolicyValue::Normal { jitter_ratio, .. }
            | ResolvedRetryPolicyValue::Always { jitter_ratio, .. } => *jitter_ratio,
        }
    }
}

impl<'de> Deserialize<'de> for ResolvedRetryPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = ResolvedRetryPolicyValue::deserialize(deserializer)?;
        validate_resolved_policy(&value).map_err(D::Error::custom)?;
        Ok(Self(value))
    }
}

fn validate_resolved_policy(policy: &ResolvedRetryPolicyValue) -> anyhow::Result<()> {
    let (initial_delay_ms, max_delay_ms, jitter_ratio) = match policy {
        ResolvedRetryPolicyValue::Normal {
            max_retries,
            retryable_codes,
            initial_delay_ms,
            max_delay_ms,
            jitter_ratio,
        } => {
            anyhow::ensure!(
                *max_retries <= MAX_SAFE_INTEGER_U64,
                "maxRetries must be a non-negative safe integer"
            );
            anyhow::ensure!(
                !retryable_codes.is_empty(),
                "retryableCodes must not be empty"
            );
            anyhow::ensure!(
                retryable_codes.iter().all(|code| !code.is_empty()),
                "retryableCodes must contain only non-empty strings"
            );
            anyhow::ensure!(
                retryable_codes.iter().collect::<HashSet<_>>().len() == retryable_codes.len(),
                "retryableCodes must not contain duplicates"
            );
            (*initial_delay_ms, *max_delay_ms, *jitter_ratio)
        }
        ResolvedRetryPolicyValue::Always {
            initial_delay_ms,
            max_delay_ms,
            jitter_ratio,
        } => (*initial_delay_ms, *max_delay_ms, *jitter_ratio),
    };
    anyhow::ensure!(
        initial_delay_ms.is_finite()
            && initial_delay_ms > 0.0
            && initial_delay_ms <= MAX_TIMER_DELAY_MS,
        "initialDelayMs must be a positive finite timer delay"
    );
    anyhow::ensure!(
        max_delay_ms.is_finite() && max_delay_ms > 0.0 && max_delay_ms <= MAX_TIMER_DELAY_MS,
        "maxDelayMs must be a positive finite timer delay"
    );
    anyhow::ensure!(
        initial_delay_ms <= max_delay_ms,
        "initialDelayMs must be less than or equal to maxDelayMs"
    );
    anyhow::ensure!(
        jitter_ratio.is_finite() && (0.0..=1.0).contains(&jitter_ratio),
        "jitterRatio must be between 0 and 1"
    );
    Ok(())
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
        return Ok(ResolvedRetryPolicy(ResolvedRetryPolicyValue::Normal {
            max_retries: DEFAULT_MAX_RETRIES,
            retryable_codes: default_codes(),
            initial_delay_ms: backoff.0,
            max_delay_ms: backoff.1,
            jitter_ratio: backoff.2,
        }));
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
            Ok(ResolvedRetryPolicy(ResolvedRetryPolicyValue::Normal {
                max_retries,
                retryable_codes,
                initial_delay_ms: backoff.0,
                max_delay_ms: backoff.1,
                jitter_ratio: backoff.2,
            }))
        }
        "always" => {
            validate_keys(object, &["mode", "backoff"], path)?;
            let backoff = resolve_backoff(object.get("backoff"), &format!("{path}.backoff"))?;
            Ok(ResolvedRetryPolicy(ResolvedRetryPolicyValue::Always {
                initial_delay_ms: backoff.0,
                max_delay_ms: backoff.1,
                jitter_ratio: backoff.2,
            }))
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
    let number = |key: &str, default: f64| match object.and_then(|value| value.get(key)) {
        None => default,
        Some(value) => value.as_f64().unwrap_or(f64::NAN),
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
    fn exported_schema_is_singleton_and_materializes_source_defaults() {
        let schema = retry_policy_schema();
        assert_eq!(schema.uid(), retry_policy_schema().uid());
        assert_eq!(
            schema.resolve(&json!({"mode": "normal"})).unwrap(),
            json!({
                "mode": "normal",
                "maxRetries": 2,
                "retryableCodes": [
                    "EMPTY_RESPONSE", "RATE_LIMIT", "SERVER", "TIMEOUT", "TRANSPORT"
                ],
                "backoff": {
                    "initialDelayMs": 500.0,
                    "maxDelayMs": 10_000.0,
                    "jitterRatio": 0.1
                }
            })
        );
        assert_eq!(
            schema.resolve(&json!({"mode": "always"})).unwrap(),
            json!({
                "mode": "always",
                "backoff": {
                    "initialDelayMs": 500.0,
                    "maxDelayMs": 10_000.0,
                    "jitterRatio": 0.1
                }
            })
        );
        assert!(
            schema
                .resolve(&json!({"mode": "normal", "maxRetries": 1.5}))
                .is_err()
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
        for invalid in [
            json!({
                "mode": "normal",
                "maxRetries": 9_007_199_254_740_992_u64,
                "retryableCodes": ["SERVER"],
                "initialDelayMs": 1.0,
                "maxDelayMs": 2.0,
                "jitterRatio": 0.0
            }),
            json!({
                "mode": "normal",
                "maxRetries": 1,
                "retryableCodes": [],
                "initialDelayMs": 1.0,
                "maxDelayMs": 2.0,
                "jitterRatio": 0.0
            }),
            json!({
                "mode": "always",
                "initialDelayMs": 3.0,
                "maxDelayMs": 2.0,
                "jitterRatio": 0.0
            }),
        ] {
            assert!(serde_json::from_value::<ResolvedRetryPolicy>(invalid).is_err());
        }
    }
}
