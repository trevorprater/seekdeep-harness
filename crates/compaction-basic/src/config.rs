//! Load-time validation and routed-model policy resolution for compaction-basic.

use thiserror::Error;

use crate::types::{
    BasicCompactionConfig, CompactionTarget, ModelCompactPolicyConfig, ResolvedCompactSpec,
    ResolvedConfig, ResolvedRetention, ResolvedTargetPolicy,
};

/// Default request-pressure fraction for every routed model.
const DEFAULT_THRESHOLD_RATIO: f64 = 0.8;

/// Default verbatim-tail fraction for every routed model.
const DEFAULT_RETAIN_RATIO: f64 = 0.16;

const DEFAULT_MAX_TOKENS: u64 = 8192;
const DEFAULT_COMPACTION_RETRIES: u64 = 1;
const DEFAULT_MAX_OVERFLOW_RETRIES: u64 = 1;

/// Target-specific pressure configuration failure eligible for warning
/// suppression.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct TargetPressureConfigError {
    /// Exact provider/model route used as the warning key.
    pub target_key: String,
    /// Actionable configuration failure detail.
    pub message: String,
}

/// Resolves and validates service defaults plus exact-target partial overrides.
///
/// # Errors
///
/// Returns a failure for unknown keys, invalid policy fields, conflicting
/// retention forms, or duplicate exact-target policies.
pub fn resolve_config(config: &BasicCompactionConfig) -> anyhow::Result<ResolvedConfig> {
    validate_policy(
        config.threshold_ratio,
        config.retain_ratio,
        config.retain_tokens,
        config.max_tokens,
        config.summarization_provider.as_deref(),
        config.summarization_model.as_deref(),
        "BasicCompactionConfig",
    )?;
    let threshold_ratio = config.threshold_ratio.unwrap_or(DEFAULT_THRESHOLD_RATIO);
    let retention = resolve_retention(
        config.retain_tokens,
        config.retain_ratio,
        ResolvedRetention::Ratio(DEFAULT_RETAIN_RATIO),
    );
    validate_ratio_retention(threshold_ratio, retention, "BasicCompactionConfig")?;
    let model_policies = resolve_model_policies(&config.model_policies)?;
    for (index, policy) in model_policies.iter().enumerate() {
        let name = format!("BasicCompactionConfig: modelPolicies[{index}]");
        let policy_retention =
            resolve_retention(policy.retain_tokens, policy.retain_ratio, retention);
        validate_ratio_retention(
            policy.threshold_ratio.unwrap_or(threshold_ratio),
            policy_retention,
            &name,
        )?;
    }

    Ok(ResolvedConfig {
        threshold_ratio,
        retention,
        summarization_provider: config.summarization_provider.clone().unwrap_or_default(),
        summarization_model: config.summarization_model.clone().unwrap_or_default(),
        max_tokens: config.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        compaction_retries: config
            .compaction_retries
            .unwrap_or(DEFAULT_COMPACTION_RETRIES),
        max_overflow_retries: config
            .max_overflow_retries
            .unwrap_or(DEFAULT_MAX_OVERFLOW_RETRIES),
        model_policies,
        auto: config.auto.unwrap_or(true),
    })
}

/// Merges the exact provider/model override over the validated default policy.
#[must_use]
pub fn resolve_target_policy(
    config: &ResolvedConfig,
    target: &CompactionTarget,
) -> ResolvedTargetPolicy {
    let override_policy = config
        .model_policies
        .iter()
        .find(|policy| policy.provider == target.provider && policy.model == target.model);
    let inherited_retention = config.retention;
    let retention = override_policy.map_or(inherited_retention, |policy| {
        resolve_retention(
            policy.retain_tokens,
            policy.retain_ratio,
            inherited_retention,
        )
    });
    ResolvedTargetPolicy {
        target: target.clone(),
        threshold_ratio: override_policy.map_or(config.threshold_ratio, |policy| {
            policy.threshold_ratio.unwrap_or(config.threshold_ratio)
        }),
        retention,
        summarization_provider: override_policy.map_or(
            config.summarization_provider.clone(),
            |p| {
                p.summarization_provider
                    .clone()
                    .unwrap_or_else(|| config.summarization_provider.clone())
            },
        ),
        summarization_model: override_policy.map_or(config.summarization_model.clone(), |p| {
            p.summarization_model
                .clone()
                .unwrap_or_else(|| config.summarization_model.clone())
        }),
        max_tokens: override_policy.map_or(config.max_tokens, |p| {
            p.max_tokens.unwrap_or(config.max_tokens)
        }),
        compaction_retries: override_policy.map_or(config.compaction_retries, |p| {
            p.compaction_retries.unwrap_or(config.compaction_retries)
        }),
        max_overflow_retries: override_policy.map_or(config.max_overflow_retries, |p| {
            p.max_overflow_retries
                .unwrap_or(config.max_overflow_retries)
        }),
    }
}

/// Scales one routed policy into concrete token budgets for its model capacity.
///
/// # Errors
///
/// Returns a target-pressure failure for a non-positive context window or a
/// retention budget that is not below the pressure threshold.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub fn resolve_compact_spec(
    policy: &ResolvedTargetPolicy,
    context_window: u64,
) -> Result<ResolvedCompactSpec, TargetPressureConfigError> {
    let target_key = format!("{}/{}", policy.target.provider, policy.target.model);
    if context_window == 0 {
        return Err(TargetPressureConfigError {
            target_key,
            message: format!(
                "BasicCompactionConfig: contextWindow ({context_window}) must be a positive integer"
            ),
        });
    }
    let threshold_tokens = (context_window as f64 * policy.threshold_ratio).floor() as u64;
    let retain_tokens = match policy.retention {
        ResolvedRetention::Ratio(ratio) => (context_window as f64 * ratio).floor() as u64,
        ResolvedRetention::Tokens(tokens) => tokens,
    };
    if retain_tokens >= threshold_tokens {
        return Err(TargetPressureConfigError {
            target_key,
            message: format!(
                "BasicCompactionConfig: {}/{} retainTokens ({retain_tokens}) must be less than threshold tokens {threshold_tokens}",
                policy.target.provider, policy.target.model
            ),
        });
    }
    Ok(ResolvedCompactSpec {
        target: policy.target.clone(),
        context_window,
        threshold_ratio: policy.threshold_ratio,
        threshold_tokens,
        retain_tokens,
        summarization_provider: policy.summarization_provider.clone(),
        summarization_model: policy.summarization_model.clone(),
        max_tokens: policy.max_tokens,
        compaction_retries: policy.compaction_retries,
        max_overflow_retries: policy.max_overflow_retries,
    })
}

/// Chooses an explicit retention form or inherits the already-resolved fallback.
fn resolve_retention(
    retain_tokens: Option<u64>,
    retain_ratio: Option<f64>,
    fallback: ResolvedRetention,
) -> ResolvedRetention {
    if let Some(tokens) = retain_tokens {
        ResolvedRetention::Tokens(tokens)
    } else if let Some(ratio) = retain_ratio {
        ResolvedRetention::Ratio(ratio)
    } else {
        fallback
    }
}

fn validate_ratio_retention(
    threshold_ratio: f64,
    retention: ResolvedRetention,
    name: &str,
) -> anyhow::Result<()> {
    if let ResolvedRetention::Ratio(ratio) = retention
        && ratio >= threshold_ratio
    {
        anyhow::bail!(
            "{name}: retainRatio ({ratio}) must be less than the resolved thresholdRatio ({threshold_ratio})"
        );
    }
    Ok(())
}

fn resolve_model_policies(
    configured: &[ModelCompactPolicyConfig],
) -> anyhow::Result<Vec<ModelCompactPolicyConfig>> {
    let mut seen = std::collections::HashSet::new();
    let mut policies = Vec::with_capacity(configured.len());
    for (index, source) in configured.iter().enumerate() {
        let name = format!("BasicCompactionConfig: modelPolicies[{index}]");
        assert_model_policy(source, &name)?;
        let key = format!("{}\0{}", source.provider, source.model);
        if !seen.insert(key) {
            anyhow::bail!(
                "BasicCompactionConfig: duplicate model policy for {}/{}",
                source.provider,
                source.model
            );
        }
        policies.push(source.clone());
    }
    Ok(policies)
}

fn assert_model_policy(source: &ModelCompactPolicyConfig, name: &str) -> anyhow::Result<()> {
    assert_non_empty_string(&format!("{name}.provider"), &source.provider)?;
    assert_non_empty_string(&format!("{name}.model"), &source.model)?;
    validate_policy(
        source.threshold_ratio,
        source.retain_ratio,
        source.retain_tokens,
        source.max_tokens,
        source.summarization_provider.as_deref(),
        source.summarization_model.as_deref(),
        name,
    )
}

#[allow(clippy::too_many_arguments)]
fn validate_policy(
    threshold_ratio: Option<f64>,
    retain_ratio: Option<f64>,
    retain_tokens: Option<u64>,
    max_tokens: Option<u64>,
    summarization_provider: Option<&str>,
    summarization_model: Option<&str>,
    name: &str,
) -> anyhow::Result<()> {
    if let Some(value) = threshold_ratio {
        assert_ratio(&format!("{name}.thresholdRatio"), value)?;
    }
    if let Some(value) = retain_ratio {
        assert_ratio(&format!("{name}.retainRatio"), value)?;
    }
    if retain_ratio.is_some() && retain_tokens.is_some() {
        anyhow::bail!("{name}: retainRatio and retainTokens are mutually exclusive");
    }
    if let Some(value) = max_tokens {
        assert_positive_integer(&format!("{name}.maxTokens"), value)?;
    }
    validate_summarization_pair(summarization_provider, summarization_model, name)
}

fn validate_summarization_pair(
    provider: Option<&str>,
    model: Option<&str>,
    name: &str,
) -> anyhow::Result<()> {
    if provider.is_none() && model.is_none() {
        return Ok(());
    }
    if provider.is_none()
        || model.is_none()
        || provider.is_some_and(str::is_empty) != model.is_some_and(str::is_empty)
    {
        anyhow::bail!(
            "{name}: summarizationProvider and summarizationModel must be set together as an empty or non-empty pair"
        );
    }
    Ok(())
}

fn assert_non_empty_string(name: &str, value: &str) -> anyhow::Result<()> {
    if value.is_empty() {
        anyhow::bail!("{name} must be a non-empty string");
    }
    Ok(())
}

fn assert_positive_integer(name: &str, value: u64) -> anyhow::Result<()> {
    if value == 0 {
        anyhow::bail!("{name} ({value}) must be a positive integer");
    }
    Ok(())
}

fn assert_ratio(name: &str, value: f64) -> anyhow::Result<()> {
    if !value.is_finite() || value <= 0.0 || value > 1.0 {
        anyhow::bail!("{name} ({value}) must be a number in (0, 1]");
    }
    Ok(())
}
