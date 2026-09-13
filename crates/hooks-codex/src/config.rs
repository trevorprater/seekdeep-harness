//! Codex five-event hook configuration parser.

use std::collections::BTreeMap;

use seekdeep_hook_protocol::{CommandHook, MatcherGroup, MatcherMode, matcher_diagnostic};
use serde_json::Value;

/// The five Codex hook points this bridge supports.
pub const CODEX_EVENTS: [&str; 5] = [
    "PreToolUse",
    "PostToolUse",
    "SessionStart",
    "UserPromptSubmit",
    "Stop",
];

/// Parsed event-name to matcher-group map.
pub type CodexHookConfig = BTreeMap<String, Vec<MatcherGroup>>;

/// One skipped non-command or asynchronous hook.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedHook {
    /// Owning event name.
    pub event: String,
    /// Stable skip reason.
    pub reason: String,
}

/// Complete tolerant parse result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedCodexConfig {
    /// Runnable groups.
    pub config: CodexHookConfig,
    /// Unsupported hooks to warn about.
    pub skipped: Vec<SkippedHook>,
}

/// Parses one wrapped or bare Codex event map.
///
/// Unknown events and malformed entries are ignored. Unsupported or async
/// hooks are recorded. Runnable invalid regexes reject the complete parse.
///
/// # Errors
///
/// Returns a stable invalid-regex diagnostic naming the owning event.
pub fn parse_codex_config(raw: &Value) -> anyhow::Result<ParsedCodexConfig> {
    let Some(root) = raw.as_object() else {
        return Ok(ParsedCodexConfig::default());
    };
    let hooks = root.get("hooks").and_then(Value::as_object).unwrap_or(root);
    let mut parsed = ParsedCodexConfig::default();
    for event in CODEX_EVENTS {
        let Some(groups) = hooks.get(event).and_then(Value::as_array) else {
            continue;
        };
        let mut accepted = Vec::new();
        for raw_group in groups {
            let Some(group) = raw_group.as_object() else {
                continue;
            };
            let Some(raw_hooks) = group.get("hooks").and_then(Value::as_array) else {
                continue;
            };
            let mut commands = Vec::new();
            for raw_hook in raw_hooks {
                let Some(hook) = raw_hook.as_object() else {
                    continue;
                };
                let hook_type = hook
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("command");
                if hook_type != "command" {
                    parsed.skipped.push(SkippedHook {
                        event: event.to_owned(),
                        reason: format!("unsupported \"{hook_type}\" hook"),
                    });
                    continue;
                }
                if hook.get("async") == Some(&Value::Bool(true)) {
                    parsed.skipped.push(SkippedHook {
                        event: event.to_owned(),
                        reason: "async hook".to_owned(),
                    });
                    continue;
                }
                let Some(command) = hook.get("command").and_then(Value::as_str) else {
                    continue;
                };
                let timeout_sec = hook
                    .get("timeout")
                    .and_then(Value::as_f64)
                    .or_else(|| hook.get("timeoutSec").and_then(Value::as_f64));
                commands.push(CommandHook {
                    command: command.to_owned(),
                    timeout_sec,
                });
            }
            if commands.is_empty() {
                continue;
            }
            let matcher = if matches!(event, "UserPromptSubmit" | "Stop") {
                None
            } else {
                group
                    .get("matcher")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            };
            if let Some(diagnostic) = matcher_diagnostic(matcher.as_deref(), MatcherMode::Codex) {
                anyhow::bail!(
                    "{diagnostic} on event {}",
                    serde_json::to_string(event).unwrap_or_default()
                );
            }
            accepted.push(MatcherGroup {
                matcher,
                hooks: commands,
            });
        }
        if !accepted.is_empty() {
            parsed.config.insert(event.to_owned(), accepted);
        }
    }
    Ok(parsed)
}
