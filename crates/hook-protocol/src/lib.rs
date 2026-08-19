//! Shared, non-plugin hook protocol library.

pub mod matcher;
pub mod types;

pub use matcher::{matcher_diagnostic, matches_matcher};
pub use types::{CommandHook, HookDecision, HookDialect, HookOutput, MatcherGroup, MatcherMode};
