//! Replay-aware basic compaction backend.

pub mod config;
pub mod index;
pub mod invariant;
pub mod region;
pub mod summarizer;
pub mod types;

pub use config::{
    TargetPressureConfigError, parse_config_value, resolve_compact_spec, resolve_config,
    resolve_target_policy,
};
pub use index::{
    BasicCompactionEngine, BasicCompactionInternals, CompactionAbortError, ManualFlush,
    config_schema, plugin,
};
pub use types::{
    BasicCompactionConfig, CompactionPolicyConfig, CompactionTarget, ModelCompactPolicyConfig,
    ResolvedCompactSpec, ResolvedConfig, ResolvedRetention, ResolvedTargetPolicy,
};
