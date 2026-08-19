//! Shared, non-plugin hook protocol library.

pub mod codec;
pub mod matcher;
pub mod types;

pub use codec::parse_hook_output;
pub use matcher::{matcher_diagnostic, matches_matcher};
pub use types::{CommandHook, HookDecision, HookDialect, HookOutput, MatcherGroup, MatcherMode};
