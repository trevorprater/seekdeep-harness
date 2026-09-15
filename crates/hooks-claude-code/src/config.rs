//! Claude Code seven-event hook configuration parser and substitutions.

use std::collections::BTreeMap;

use seekdeep_hook_protocol::{CommandHook, MatcherGroup, MatcherMode, matcher_diagnostic};
use serde_json::Value;

/// Seven supported Claude Code hook points.
pub const CLAUDE_EVENTS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Stop",
    "SubagentStart",
    "SubagentStop",
];

/// Parsed event-name to matcher groups.
pub type ClaudeCodeHookConfig = BTreeMap<String, Vec<MatcherGroup>>;

/// Unsupported non-command hook surfaced for diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedHook {
    /// Owning event.
    pub event: String,
    /// Unsupported type.
    pub hook_type: String,
}

/// Command substitution values.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubstitutionVars {
    /// `${CLAUDE_PLUGIN_ROOT}` replacement.
    pub plugin_root: Option<String>,
    /// `${CLAUDE_PROJECT_DIR}` replacement.
    pub project_dir: Option<String>,
}

/// Tolerant parse result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParsedClaudeConfig {
    /// Runnable groups.
    pub config: ClaudeCodeHookConfig,
    /// Unsupported hooks.
    pub skipped: Vec<SkippedHook>,
}

/// Applies all configured Claude placeholder substitutions.
#[must_use]
pub fn substitute_command(command: &str, variables: &SubstitutionVars) -> String {
    let mut output = command.to_owned();
    if let Some(root) = &variables.plugin_root {
        output = output.replace("${CLAUDE_PLUGIN_ROOT}", root);
    }
    if let Some(project) = &variables.project_dir {
        output = output.replace("${CLAUDE_PROJECT_DIR}", project);
    }
    output
}

/// Parses one wrapped or bare Claude Code event map.
///
/// # Errors
///
/// Returns a stable event-qualified invalid-regex failure.
pub fn parse_claude_code_config(
    raw: &Value,
    variables: &SubstitutionVars,
) -> anyhow::Result<ParsedClaudeConfig> {
    let Some(root) = raw.as_object() else {
        return Ok(ParsedClaudeConfig::default());
    };
    let hooks = root.get("hooks").and_then(Value::as_object).unwrap_or(root);
    let mut parsed = ParsedClaudeConfig::default();
    for event in CLAUDE_EVENTS {
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
                        hook_type: hook_type.to_owned(),
                    });
                    continue;
                }
                let Some(command) = hook.get("command").and_then(Value::as_str) else {
                    continue;
                };
                commands.push(CommandHook {
                    command: substitute_command(command, variables),
                    timeout_sec: hook.get("timeout").and_then(Value::as_f64),
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
            if let Some(diagnostic) =
                matcher_diagnostic(matcher.as_deref(), MatcherMode::ClaudeCode)
            {
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
