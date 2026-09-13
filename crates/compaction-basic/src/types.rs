//! Configuration vocabulary for the replay-aware basic compaction backend.

use serde::{Deserialize, Serialize};

/// Policy fields shared by the default policy and exact model overrides.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct CompactionPolicyConfig {
    /// Compact at this fraction of the model's context window.
    pub threshold_ratio: Option<f64>,
    /// Recent context retained as a fraction of the model's window.
    pub retain_ratio: Option<f64>,
    /// Absolute recent-context budget; mutually exclusive with `retain_ratio`.
    pub retain_tokens: Option<u64>,
    /// Summary provider; set together with `summarization_model`.
    pub summarization_provider: Option<String>,
    /// Summary model; set together with `summarization_provider`.
    pub summarization_model: Option<String>,
    /// Provider generation cap for summarization.
    pub max_tokens: Option<u64>,
    /// Extra attempts after the first compaction when pressure remains.
    pub compaction_retries: Option<u64>,
    /// Maximum retries after canonical context overflow.
    pub max_overflow_retries: Option<u64>,
}

/// Exact provider/model override merged over the default compaction policy.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCompactPolicyConfig {
    /// Registered provider route to match.
    pub provider: String,
    /// Exact routed model id to match within `provider`.
    pub model: String,
    /// Compact at this fraction of the model's context window.
    pub threshold_ratio: Option<f64>,
    /// Recent context retained as a fraction of the model's window.
    pub retain_ratio: Option<f64>,
    /// Absolute recent-context budget; mutually exclusive with `retain_ratio`.
    pub retain_tokens: Option<u64>,
    /// Summary provider; set together with `summarization_model`.
    pub summarization_provider: Option<String>,
    /// Summary model; set together with `summarization_provider`.
    pub summarization_model: Option<String>,
    /// Provider generation cap for summarization.
    pub max_tokens: Option<u64>,
    /// Extra attempts after the first compaction when pressure remains.
    pub compaction_retries: Option<u64>,
    /// Maximum retries after canonical context overflow.
    pub max_overflow_retries: Option<u64>,
}

/// Basic compaction configuration with an optional exact-target policy table.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct BasicCompactionConfig {
    /// Compact at this fraction of the model's context window.
    pub threshold_ratio: Option<f64>,
    /// Recent context retained as a fraction of the model's window.
    pub retain_ratio: Option<f64>,
    /// Absolute recent-context budget; mutually exclusive with `retain_ratio`.
    pub retain_tokens: Option<u64>,
    /// Summary provider; set together with `summarization_model`.
    pub summarization_provider: Option<String>,
    /// Summary model; set together with `summarization_provider`.
    pub summarization_model: Option<String>,
    /// Provider generation cap for summarization.
    pub max_tokens: Option<u64>,
    /// Extra attempts after the first compaction when pressure remains.
    pub compaction_retries: Option<u64>,
    /// Maximum retries after canonical context overflow.
    pub max_overflow_retries: Option<u64>,
    /// Exact provider/model overrides; duplicate targets fail plugin load.
    pub model_policies: Vec<ModelCompactPolicyConfig>,
    /// Enable automatic step-boundary pressure listeners.
    pub auto: Option<bool>,
}

/// Exactly one validated retention form.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResolvedRetention {
    /// Fraction of the model window retained.
    Ratio(f64),
    /// Absolute recent-context budget.
    Tokens(u64),
}

/// Validated immutable config whose target-specific defaults remain unresolved.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedConfig {
    /// Request-pressure fraction.
    pub threshold_ratio: f64,
    /// Capacity-independent retention form.
    pub retention: ResolvedRetention,
    /// Summary provider.
    pub summarization_provider: String,
    /// Summary model.
    pub summarization_model: String,
    /// Provider generation cap for summarization.
    pub max_tokens: u64,
    /// Extra attempts after the first compaction.
    pub compaction_retries: u64,
    /// Maximum overflow-recovery retries.
    pub max_overflow_retries: u64,
    /// Validated exact-target overrides.
    pub model_policies: Vec<ModelCompactPolicyConfig>,
    /// Automatic step-boundary listeners enabled.
    pub auto: bool,
}

/// One routed conversation target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactionTarget {
    /// Provider route.
    pub provider: String,
    /// Model id.
    pub model: String,
}

/// Fully merged policy for one routed conversation target, before capacity
/// scaling.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedTargetPolicy {
    /// Exact durable target.
    pub target: CompactionTarget,
    /// Request-pressure fraction.
    pub threshold_ratio: f64,
    /// Capacity-independent retention form.
    pub retention: ResolvedRetention,
    /// Summary provider.
    pub summarization_provider: String,
    /// Summary model.
    pub summarization_model: String,
    /// Provider generation cap for summarization.
    pub max_tokens: u64,
    /// Extra attempts after the first compaction.
    pub compaction_retries: u64,
    /// Maximum overflow-recovery retries.
    pub max_overflow_retries: u64,
}

/// One routed model's concrete pressure and retention budget.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedCompactSpec {
    /// Exact durable target.
    pub target: CompactionTarget,
    /// Adapter-owned capacity for that target.
    pub context_window: u64,
    /// Request-pressure fraction.
    pub threshold_ratio: f64,
    /// Concrete pressure threshold in tokens.
    pub threshold_tokens: u64,
    /// Concrete retained tail in tokens.
    pub retain_tokens: u64,
    /// Summary provider.
    pub summarization_provider: String,
    /// Summary model.
    pub summarization_model: String,
    /// Provider generation cap for summarization.
    pub max_tokens: u64,
    /// Extra attempts after the first compaction.
    pub compaction_retries: u64,
    /// Maximum overflow-recovery retries.
    pub max_overflow_retries: u64,
}
