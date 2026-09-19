//! Claude Code command-hook bridge over `SeekDeep` interception points.

mod bridge;
pub mod config;

pub use bridge::{Config, INJECT, NAME, apply, config_schema, plugin};
pub use config::{
    CLAUDE_EVENTS, ClaudeCodeHookConfig, ParsedClaudeConfig, SkippedHook, SubstitutionVars,
    parse_claude_code_config, substitute_command,
};
