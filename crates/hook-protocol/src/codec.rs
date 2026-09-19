//! Decode hook process outcomes for both dialects.

use serde_json::{Map, Value};

use crate::types::{HookDecision, HookOutput};

/// The exit code a hook uses to signal a blocking error.
const BLOCKING_EXIT_CODE: i32 = 2;

fn str_field<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}

/// The legacy top-level decision is only approve/block.
fn top_level_decision_of(value: Option<&str>) -> Option<HookDecision> {
    match value {
        Some("approve") => Some(HookDecision::Approve),
        Some("block") => Some(HookDecision::Block),
        _ => None,
    }
}

/// A hookSpecificOutput.permissionDecision is allow/deny/ask only.
fn permission_decision_of(value: Option<&str>) -> Option<HookDecision> {
    match value {
        Some("allow") => Some(HookDecision::Allow),
        Some("deny") => Some(HookDecision::Deny),
        Some("ask") => Some(HookDecision::Ask),
        _ => None,
    }
}

/// Decodes process output into a dialect-neutral hook outcome.
#[must_use]
pub fn parse_hook_output(
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
    expected_event_name: Option<&str>,
) -> HookOutput {
    let trimmed_err = stderr.trim();
    let trimmed_out = stdout.trim();
    let mut output = HookOutput {
        exit_code,
        stderr: trimmed_err.to_owned(),
        stdout: trimmed_out.to_owned(),
        continue_: None,
        stop_reason: None,
        decision: None,
        reason: None,
        hook_event_name: None,
        additional_context: None,
        system_message: None,
        updated_input: None,
    };

    if exit_code == Some(BLOCKING_EXIT_CODE) {
        output.decision = Some(HookDecision::Block);
        if !trimmed_err.is_empty() {
            output.reason = Some(trimmed_err.to_owned());
        }
    }

    if exit_code == Some(0)
        && trimmed_out.starts_with('{')
        && let Ok(parsed) = serde_json::from_str::<Value>(trimmed_out)
        && let Some(object) = parsed.as_object()
    {
        apply_structured(&mut output, object, expected_event_name);
    }

    output
}

/// Folds a parsed structured-stdout object into output.
fn apply_structured(
    output: &mut HookOutput,
    parsed: &Map<String, Value>,
    expected_event_name: Option<&str>,
) {
    if let Some(continue_) = parsed.get("continue").and_then(Value::as_bool) {
        output.continue_ = Some(continue_);
    }
    if let Some(stop_reason) = str_field(parsed, "stopReason") {
        output.stop_reason = Some(stop_reason.to_owned());
    }
    if let Some(system_message) = str_field(parsed, "systemMessage") {
        output.system_message = Some(system_message.to_owned());
    }

    if let Some(decision) = top_level_decision_of(str_field(parsed, "decision")) {
        output.decision = Some(decision);
    }
    if let Some(reason) = str_field(parsed, "reason") {
        output.reason = Some(reason.to_owned());
    }

    if let Some(hso) = parsed.get("hookSpecificOutput").and_then(Value::as_object) {
        if let Some(event_name) = str_field(hso, "hookEventName") {
            output.hook_event_name = Some(event_name.to_owned());
        }
        if let Some(expected) = expected_event_name
            && str_field(hso, "hookEventName") != Some(expected)
        {
            return;
        }
        if let Some(permission) = permission_decision_of(str_field(hso, "permissionDecision")) {
            output.decision = Some(permission);
        }
        if let Some(permission_reason) = str_field(hso, "permissionDecisionReason") {
            output.reason = Some(permission_reason.to_owned());
        }
        if let Some(additional_context) = str_field(hso, "additionalContext") {
            output.additional_context = Some(additional_context.to_owned());
        }
        if let Some(updated) = hso.get("updatedInput").and_then(Value::as_object) {
            output.updated_input = Some(Value::Object(updated.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_two_blocks_with_stderr_reason() {
        let output = parse_hook_output(Some(2), "", "  blocked  ", None);
        assert_eq!(output.decision, Some(HookDecision::Block));
        assert_eq!(output.reason.as_deref(), Some("blocked"));
    }

    #[test]
    fn clean_structured_stdout_folds_fields() {
        let output = parse_hook_output(
            Some(0),
            r#"{"continue": false, "stopReason": "no", "decision": "approve", "reason": "ok"}"#,
            "",
            None,
        );
        assert_eq!(output.continue_, Some(false));
        assert_eq!(output.stop_reason.as_deref(), Some("no"));
        assert_eq!(output.decision, Some(HookDecision::Approve));
        assert_eq!(output.reason.as_deref(), Some("ok"));
    }

    #[test]
    fn malformed_json_stays_plain_stdout() {
        let output = parse_hook_output(Some(0), "{not json", "", None);
        assert_eq!(output.stdout, "{not json");
        assert_eq!(output.decision, None);
    }

    #[test]
    fn permission_decision_overrides_top_level_and_event_guard() {
        let output = parse_hook_output(
            Some(0),
            r#"{"decision": "block", "hookSpecificOutput": {"hookEventName": "Stop", "permissionDecision": "allow"}}"#,
            "",
            Some("Stop"),
        );
        assert_eq!(output.decision, Some(HookDecision::Allow));
        // mismatched event discards the permissionDecision
        let mismatched = parse_hook_output(
            Some(0),
            r#"{"hookSpecificOutput": {"hookEventName": "Other", "permissionDecision": "deny"}}"#,
            "",
            Some("Stop"),
        );
        assert_eq!(mismatched.decision, None);
        assert_eq!(mismatched.hook_event_name.as_deref(), Some("Other"));
    }

    #[test]
    fn exit_zero_with_no_stdout_is_neutral() {
        let output = parse_hook_output(Some(0), "", "", None);
        assert_eq!(output.exit_code, Some(0));
        assert_eq!(output.decision, None);
        assert_eq!(output.continue_, None);
    }

    #[test]
    fn exit_two_empty_stderr_still_blocks_without_reason() {
        let output = parse_hook_output(Some(2), "", "   ", None);
        assert_eq!(output.decision, Some(HookDecision::Block));
        assert_eq!(output.reason, None);
    }

    #[test]
    fn other_nonzero_exit_is_a_non_blocking_error() {
        let output = parse_hook_output(Some(1), "", "some warning", None);
        assert_eq!(output.decision, None);
        assert_eq!(output.exit_code, Some(1));
        assert_eq!(output.stderr, "some warning");
    }

    #[test]
    fn undefined_exit_carries_no_decision() {
        let output = parse_hook_output(None, "", "spawn failed: ENOENT", None);
        assert_eq!(output.exit_code, None);
        assert_eq!(output.decision, None);
        assert_eq!(output.stderr, "spawn failed: ENOENT");
    }

    #[test]
    fn parses_top_level_continue_stop_reason_and_system_message() {
        let output = parse_hook_output(
            Some(0),
            r#"{"continue": false, "stopReason": "budget exceeded", "systemMessage": "heads up"}"#,
            "",
            None,
        );
        assert_eq!(output.continue_, Some(false));
        assert_eq!(output.stop_reason.as_deref(), Some("budget exceeded"));
        assert_eq!(output.system_message.as_deref(), Some("heads up"));
    }

    #[test]
    fn parses_legacy_top_level_decision_approve_block_only() {
        assert_eq!(
            parse_hook_output(
                Some(0),
                r#"{"decision": "block", "reason": "nope"}"#,
                "",
                None
            )
            .decision,
            Some(HookDecision::Block)
        );
        assert_eq!(
            parse_hook_output(Some(0), r#"{"decision": "approve"}"#, "", None).decision,
            Some(HookDecision::Approve)
        );
    }

    #[test]
    fn top_level_allow_deny_ask_are_invalid_and_ignored() {
        for decision in ["deny", "allow", "ask"] {
            let json = format!(r#"{{"decision": "{decision}"}}"#);
            let output = parse_hook_output(Some(0), &json, "", None);
            assert_eq!(output.decision, None, "decision {decision}");
        }
    }

    #[test]
    fn captures_hook_event_name_and_permission_decision() {
        let output = parse_hook_output(
            Some(0),
            r#"{"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny"}}"#,
            "",
            None,
        );
        assert_eq!(output.hook_event_name.as_deref(), Some("PreToolUse"));
        assert_eq!(output.decision, Some(HookDecision::Deny));
    }

    #[test]
    fn parses_allow_and_ask_permission_decisions() {
        assert_eq!(
            parse_hook_output(
                Some(0),
                r#"{"hookSpecificOutput": {"permissionDecision": "allow"}}"#,
                "",
                None,
            )
            .decision,
            Some(HookDecision::Allow)
        );
        assert_eq!(
            parse_hook_output(
                Some(0),
                r#"{"hookSpecificOutput": {"permissionDecision": "ask"}}"#,
                "",
                None,
            )
            .decision,
            Some(HookDecision::Ask)
        );
    }

    #[test]
    fn parses_additional_context_and_updated_input() {
        let output = parse_hook_output(
            Some(0),
            r#"{"hookSpecificOutput": {"additionalContext": "remember X", "updatedInput": {"command": "safe"}}}"#,
            "",
            None,
        );
        assert_eq!(output.additional_context.as_deref(), Some("remember X"));
        assert_eq!(
            output.updated_input,
            Some(serde_json::json!({"command": "safe"}))
        );
    }

    #[test]
    fn unknown_decision_string_is_ignored() {
        let output = parse_hook_output(Some(0), r#"{"decision": "maybe"}"#, "", None);
        assert_eq!(output.decision, None);
    }

    #[test]
    fn discards_a_mismatched_block_but_keeps_its_discriminator() {
        let output = parse_hook_output(
            Some(0),
            r#"{"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": "no", "additionalContext": "x", "updatedInput": {"command": "y"}}}"#,
            "",
            Some("Stop"),
        );
        assert_eq!(output.hook_event_name.as_deref(), Some("PreToolUse"));
        assert_eq!(output.decision, None);
        assert_eq!(output.reason, None);
        assert_eq!(output.additional_context, None);
        assert_eq!(output.updated_input, None);
    }

    #[test]
    fn applies_a_matching_block() {
        let output = parse_hook_output(
            Some(0),
            r#"{"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "additionalContext": "x"}}"#,
            "",
            Some("PreToolUse"),
        );
        assert_eq!(output.decision, Some(HookDecision::Deny));
        assert_eq!(output.additional_context.as_deref(), Some("x"));
    }

    #[test]
    fn applies_the_block_when_expected_event_name_is_omitted() {
        let output = parse_hook_output(
            Some(0),
            r#"{"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny"}}"#,
            "",
            None,
        );
        assert_eq!(output.decision, Some(HookDecision::Deny));
    }

    #[test]
    fn discards_a_discriminator_less_block_when_an_event_is_expected() {
        let output = parse_hook_output(
            Some(0),
            r#"{"hookSpecificOutput": {"permissionDecision": "deny", "additionalContext": "x"}}"#,
            "",
            Some("Stop"),
        );
        assert_eq!(output.hook_event_name, None);
        assert_eq!(output.decision, None);
        assert_eq!(output.additional_context, None);
    }

    #[test]
    fn applies_a_discriminator_less_block_when_opted_out() {
        let output = parse_hook_output(
            Some(0),
            r#"{"hookSpecificOutput": {"permissionDecision": "deny"}}"#,
            "",
            None,
        );
        assert_eq!(output.decision, Some(HookDecision::Deny));
    }

    #[test]
    fn a_mismatched_block_does_not_discard_top_level_fields() {
        let output = parse_hook_output(
            Some(0),
            r#"{"decision": "block", "reason": "top", "continue": false, "stopReason": "halt", "hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "allow"}}"#,
            "",
            Some("Stop"),
        );
        assert_eq!(output.decision, Some(HookDecision::Block));
        assert_eq!(output.reason.as_deref(), Some("top"));
        assert_eq!(output.continue_, Some(false));
        assert_eq!(output.stop_reason.as_deref(), Some("halt"));
    }

    #[test]
    fn non_object_stdout_is_left_verbatim_trimmed() {
        let output = parse_hook_output(Some(0), "just some text output", "", None);
        assert_eq!(output.decision, None);
        assert_eq!(output.continue_, None);
        assert_eq!(output.stdout, "just some text output");
    }

    #[test]
    fn preserves_raw_stdout_trimmed_alongside_parsed_fields() {
        let json = r#"{"decision": "block"}"#;
        let output = parse_hook_output(Some(0), &format!("  {json}  \n"), "", None);
        assert_eq!(output.stdout, json);
        assert_eq!(output.decision, Some(HookDecision::Block));
    }

    #[test]
    fn stdout_is_empty_when_the_hook_emits_none() {
        assert_eq!(parse_hook_output(Some(0), "", "", None).stdout, "");
    }

    #[test]
    fn a_json_array_stdout_yields_no_fields() {
        let output = parse_hook_output(Some(0), "[1,2,3]", "", None);
        assert_eq!(output.decision, None);
    }

    #[test]
    fn structured_stdout_is_ignored_on_a_blocking_exit() {
        let output = parse_hook_output(Some(2), r#"{"decision": "approve"}"#, "blocked", None);
        assert_eq!(output.decision, Some(HookDecision::Block));
        assert_eq!(output.reason.as_deref(), Some("blocked"));
    }
}
