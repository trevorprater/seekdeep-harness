//! Claude Code config and substitution parity.

use seekdeep_hooks_claude_code::{
    CLAUDE_EVENTS, SubstitutionVars, parse_claude_code_config, substitute_command,
};
use serde_json::json;

#[test]
fn substitutes_all_configured_tokens_and_leaves_unset_tokens_verbatim() {
    let vars = SubstitutionVars {
        plugin_root: Some("/plugin".to_owned()),
        project_dir: Some("/project".to_owned()),
    };
    assert_eq!(
        substitute_command(
            "${CLAUDE_PLUGIN_ROOT}/a ${CLAUDE_PROJECT_DIR} ${CLAUDE_PLUGIN_ROOT}",
            &vars,
        ),
        "/plugin/a /project /plugin"
    );
    assert_eq!(
        substitute_command("${CLAUDE_PLUGIN_ROOT}/a", &SubstitutionVars::default()),
        "${CLAUDE_PLUGIN_ROOT}/a"
    );
}

#[test]
fn parses_wrapper_defaults_timeout_substitution_and_supported_events() {
    let groups = json!({
        "PreToolUse":[{"matcher":"Edit|Write","hooks":[
            {"command":"${CLAUDE_PROJECT_DIR}/hook","timeout":1.5},
            {"type":"command","command":"plain"}
        ]}],
        "SubagentStart":[{"hooks":[{"command":"start"}]}],
        "Notification":[{"matcher":"[","hooks":[{"command":"ignored"}]}]
    });
    let variables = SubstitutionVars {
        plugin_root: None,
        project_dir: Some("/work".to_owned()),
    };
    let bare = parse_claude_code_config(&groups, &variables).unwrap();
    let wrapped = parse_claude_code_config(&json!({"hooks":groups}), &variables).unwrap();
    assert_eq!(bare, wrapped);
    assert_eq!(CLAUDE_EVENTS.len(), 7);
    assert_eq!(bare.config.len(), 2);
    let pre = &bare.config["PreToolUse"][0];
    assert_eq!(pre.matcher.as_deref(), Some("Edit|Write"));
    assert_eq!(pre.hooks[0].command, "/work/hook");
    assert_eq!(pre.hooks[0].timeout_sec, Some(1.5));
    assert_eq!(pre.hooks[1].timeout_sec, None);
}

#[test]
fn records_unsupported_hooks_and_ignores_malformed_shapes() {
    let parsed = parse_claude_code_config(
        &json!({
            "PostToolUse":[{"hooks":[
                null,
                7,
                {"type":"http","url":"x"},
                {"command":"valid"},
                {"command":9}
            ]}],
            "Stop":"not-an-array"
        }),
        &SubstitutionVars::default(),
    )
    .unwrap();
    assert_eq!(parsed.config["PostToolUse"][0].hooks[0].command, "valid");
    assert_eq!(parsed.skipped.len(), 1);
    assert_eq!(parsed.skipped[0].hook_type, "http");
    for value in [json!(null), json!([]), json!(7)] {
        assert!(
            parse_claude_code_config(&value, &SubstitutionVars::default())
                .unwrap()
                .config
                .is_empty()
        );
    }
}

#[test]
fn validates_only_matcher_subjects_on_supported_events() {
    let error = parse_claude_code_config(
        &json!({"PreToolUse":[{"matcher":"[","hooks":[{"command":"bad"}]}]}),
        &SubstitutionVars::default(),
    )
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid claude-code regex matcher \"[\" on event \"PreToolUse\""
    );
    let parsed = parse_claude_code_config(
        &json!({
            "UserPromptSubmit":[{"matcher":"[","hooks":[{"command":"prompt"}]}],
            "Stop":[{"matcher":"(","hooks":[{"command":"stop"}]}],
            "Unsupported":[{"matcher":"[","hooks":[{"command":"ignored"}]}]
        }),
        &SubstitutionVars::default(),
    )
    .unwrap();
    assert_eq!(parsed.config["UserPromptSubmit"][0].matcher, None);
    assert_eq!(parsed.config["Stop"][0].matcher, None);
}
