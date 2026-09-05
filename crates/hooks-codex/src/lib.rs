//! Codex command-hook bridge over `SeekDeep` agent and tool interception points.

mod bridge;
pub mod config;

pub use bridge::{Config, INJECT, NAME, apply, config_schema, plugin};
pub use config::{
    CODEX_EVENTS, CodexHookConfig, ParsedCodexConfig, SkippedHook, parse_codex_config,
};
