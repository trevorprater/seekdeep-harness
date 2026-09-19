//! Replay-safe, model-free tool-result pruning service.

pub mod config;
pub mod index;
pub mod invariant;
pub mod types;

pub use config::{DEFAULTS, PRUNE_MARKER, code_point_length, resolve_config};
pub use index::{INJECT, NAME, TOOL_RESULT_PRUNER, ToolResultPruner, config_schema, plugin};
pub use invariant::register_invariant;
pub use types::{PruneResult, PrunedEntry, ResolvedConfig, ToolResultPruneConfig};
