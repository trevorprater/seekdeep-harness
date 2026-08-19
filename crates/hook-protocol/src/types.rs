//! Dialect-neutral vocabulary shared by the Claude Code and Codex hook bridges.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The bridge that ran a hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HookDialect {
    /// The Claude Code bridge.
    ClaudeCode,
    /// The Codex bridge.
    Codex,
}

/// One configured command hook.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandHook {
    /// The shell command line to run.
    pub command: String,
    /// Per-hook timeout in seconds (the wire unit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_sec: Option<u64>,
}

/// One matcher group: a pattern plus the command hooks that run when it matches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatcherGroup {
    /// Absent / empty / * = match-all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    /// Command hooks.
    pub hooks: Vec<CommandHook>,
}

/// How a matcher pattern is interpreted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MatcherMode {
    /// Word-and-pipe patterns are literal alternatives.
    ClaudeCode,
    /// Every non-empty pattern is an unanchored regex.
    Codex,
}

/// The neutral permission decision a hook expressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookDecision {
    /// Permit.
    Approve,
    /// Permit (permissionDecision channel).
    Allow,
    /// Forbid.
    Block,
    /// Forbid (permissionDecision channel).
    Deny,
    /// Request confirmation.
    Ask,
}

/// The dialect-neutral outcome a hook produced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookOutput {
    /// Raw process exit code, absent when the hook could not run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Trimmed stderr.
    pub stderr: String,
    /// Trimmed stdout, verbatim.
    pub stdout: String,
    /// False means the hook asked to halt.
    #[serde(default, rename = "continue", skip_serializing_if = "Option::is_none")]
    pub continue_: Option<bool>,
    /// Human-readable reason shown when continue is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Neutral blocking decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<HookDecision>,
    /// Reason accompanying the decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Event discriminator claimed by hookSpecificOutput.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_event_name: Option<String>,
    /// Extra context to inject for the next model request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
    /// A warning surfaced to the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_message: Option<String>,
    /// A tool-input rewrite a hook requested (parsed but not honored).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_input: Option<Value>,
}
