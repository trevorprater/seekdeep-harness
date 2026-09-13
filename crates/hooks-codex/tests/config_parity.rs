//! Codex tolerant configuration parser parity.

use seekdeep_hooks_codex::{CODEX_EVENTS, parse_codex_config};
use serde_json::json;

#[test]
fn accepts_only_five_events_wrapper_aliases_and_default_command_type() {
    let parsed = parse_codex_config(&json!({
        "hooks": {
            "PreToolUse": [{"matcher":"^Bash$","hooks":[
                {"command":"${NOT_SUBSTITUTED}/a.sh","timeout":10},
                {"type":"command","command":"b.sh","timeoutSec":20}
            ]}],
            "Stop": [{"hooks":[{"command":"stop.sh"}]}],
            "SubagentStop": [{"hooks":[{"command":"ignored.sh"}]}],
            "Notification": [{"hooks":[{"command":"ignored-too.sh"}]}]
        }
    }))
    .unwrap();
    assert_eq!(CODEX_EVENTS.len(), 5);
    assert!(!CODEX_EVENTS.contains(&"SubagentStop"));
    assert_eq!(parsed.config.len(), 2);
    let pre = &parsed.config["PreToolUse"][0];
    assert_eq!(pre.matcher.as_deref(), Some("^Bash$"));
    assert_eq!(pre.hooks[0].command, "${NOT_SUBSTITUTED}/a.sh");
    assert_eq!(pre.hooks[0].timeout_sec, Some(10.0));
    assert_eq!(pre.hooks[1].timeout_sec, Some(20.0));
    assert_eq!(parsed.config["Stop"][0].matcher, None);
}

#[test]
fn records_unsupported_and_async_hooks_while_retaining_valid_siblings() {
    let parsed = parse_codex_config(&json!({
        "PreToolUse": [{"hooks":[
            null,
            7,
            {"type":"prompt"},
            {"type":"command","command":"background.sh","async":true},
            {"type":"command","command":"sync.sh"}
        ]}]
    }))
    .unwrap();
    assert_eq!(parsed.config["PreToolUse"][0].hooks[0].command, "sync.sh");
    assert_eq!(parsed.skipped.len(), 2);
    assert_eq!(parsed.skipped[0].reason, "unsupported \"prompt\" hook");
    assert_eq!(parsed.skipped[1].reason, "async hook");
}

#[test]
fn ignores_malformed_shapes_and_non_object_roots() {
    for value in [
        json!(null),
        json!({"PreToolUse":"no"}),
        json!({"Stop":[7,{"hooks":"x"},{"hooks":[{"command":9}]}]}),
    ] {
        assert!(parse_codex_config(&value).unwrap().config.is_empty());
    }
    let bare = json!({"Stop":[{"hooks":[{"command":"s.sh"}]}]});
    assert_eq!(
        parse_codex_config(&bare).unwrap().config,
        parse_codex_config(&json!({"hooks":bare})).unwrap().config
    );
}

#[test]
fn rejects_runnable_invalid_regex_but_discards_subjectless_matchers_first() {
    let error = parse_codex_config(&json!({
        "PreToolUse":[{"matcher":"[","hooks":[{"command":"bad.sh"}]}]
    }))
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid codex regex matcher \"[\" on event \"PreToolUse\""
    );
    let parsed = parse_codex_config(&json!({
        "UserPromptSubmit":[{"matcher":"[","hooks":[{"command":"prompt.sh"}]}],
        "Stop":[{"matcher":"(","hooks":[{"command":"stop.sh"}]}]
    }))
    .unwrap();
    assert_eq!(parsed.config["UserPromptSubmit"][0].matcher, None);
    assert_eq!(parsed.config["Stop"][0].matcher, None);
}
