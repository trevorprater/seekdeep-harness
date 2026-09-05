//! Shared, non-plugin hook protocol library.

pub mod codec;
pub mod detached;
pub mod events;
pub mod invariant;
pub mod matcher;
pub mod merge;
pub mod runner;
pub mod types;

pub use codec::parse_hook_output;
pub use detached::{DetachedRuns, create_detached_runs};
pub use events::{
    DEFAULT_STDERR_SUMMARY_MAX_CHARS, HookInvocation, HookResultRecord, append_hook_invoked,
    append_hook_result, summarize_stderr,
};
pub use matcher::{matcher_diagnostic, matches_matcher};
pub use merge::{MergedDecision, MergedHookOutcome, merge_hook_outputs};
pub use runner::{DEFAULT_HOOK_TIMEOUT_MS, RunHookOptions, RunHookResult, run_hook};
pub use types::{CommandHook, HookDecision, HookDialect, HookOutput, MatcherGroup, MatcherMode};
