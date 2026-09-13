//! Skill row durable-model and locale parity.

use seekdeep_client_ui_skill::{SKILL_LOCALES, SKILL_NS, SkillRowState, skill_row_model};
use seekdeep_client_ui_tool::{ToolCallBlock, ToolCallHead, ToolErrorInfo};
use seekdeep_lossless_json::{JsonString, JsonValue};
use serde_json::json;

fn running(args_raw: &str) -> ToolCallBlock {
    ToolCallBlock::Running {
        call_id: "call-skill".to_owned(),
        args_raw: args_raw.into(),
        call_view: None,
    }
}

fn settled(
    args_raw: Option<&str>,
    content: Vec<serde_json::Value>,
    is_error: bool,
    error: Option<ToolErrorInfo>,
) -> ToolCallBlock {
    ToolCallBlock::Settled {
        call_id: "call-skill".to_owned(),
        call: args_raw.map(|args_raw| ToolCallHead {
            args_raw: args_raw.into(),
        }),
        call_view: None,
        result_view: None,
        content: content.into_iter().map(Into::into).collect(),
        is_error,
        error,
    }
}

#[test]
fn row_model_preserves_names_states_exact_output_and_error_fallbacks() {
    let pending = skill_row_model(&running(r#"{"name":"dsh-manage-issues"}"#));
    assert_eq!(pending.name, "dsh-manage-issues");
    assert_eq!(pending.state, SkillRowState::Running);
    assert_eq!(pending.output, None);

    let ok = skill_row_model(&settled(
        Some(r#"{"name":"dsh-manage-issues"}"#),
        vec![json!({"type":"text","text":"Follow the issue workflow.\nKeep fields in sync."})],
        false,
        None,
    ));
    assert_eq!(ok.state, SkillRowState::Ok);
    assert_eq!(
        ok.output.as_ref().and_then(JsonString::as_str),
        Some("Follow the issue workflow.\nKeep fields in sync.")
    );

    let failed = skill_row_model(&settled(
        Some(r#"{"name":"broken"}"#),
        vec![json!({"type":"text","text":"SkillError: missing resource\nCheck SKILL.md."})],
        true,
        Some(ToolErrorInfo {
            name: "SkillError".to_owned(),
            code: "missing".to_owned(),
        }),
    ));
    assert_eq!(failed.state, SkillRowState::Error);
    assert_eq!(
        failed.error_summary.as_ref().and_then(JsonString::as_str),
        Some("SkillError: missing resource")
    );

    let stopped = skill_row_model(&settled(
        Some(r#"{"name":"stopped"}"#),
        Vec::new(),
        false,
        Some(ToolErrorInfo {
            name: "InterruptedError".to_owned(),
            code: "interrupted".to_owned(),
        }),
    ));
    assert_eq!(stopped.state, SkillRowState::Stopped);
    assert_eq!(
        stopped.output.as_ref().and_then(JsonString::as_str),
        Some("InterruptedError: interrupted")
    );

    let structured = skill_row_model(&settled(
        Some(r#"{"name":"structured"}"#),
        vec![json!({"type":"reasoning","text":"note"})],
        false,
        None,
    ));
    assert!(
        structured
            .output
            .as_ref()
            .unwrap()
            .contains("\"type\": \"reasoning\"")
    );
}

#[test]
fn malformed_empty_and_windowless_names_use_durable_fallbacks() {
    assert_eq!(skill_row_model(&running("{\"name\":\n")).name, "{\"name\":");
    assert_eq!(
        skill_row_model(&running(r#""raw-name""#)).name,
        r#""raw-name""#
    );
    assert_eq!(
        skill_row_model(&running(r#"{"name":""}"#)).name,
        r#"{"name":""}"#
    );
    assert_eq!(
        skill_row_model(&settled(None, Vec::new(), false, None)).name,
        "call-skill"
    );
    assert_eq!(
        skill_row_model(&running(r#"{"name":"first\nsecond"}"#)).name,
        "first"
    );
}

#[test]
fn skill_rows_preserve_raw_utf16_names_outputs_and_error_lines() {
    let pending = skill_row_model(&running(r#"{"name":"\ud800\nsecond"}"#));
    assert_eq!(pending.name.to_utf16(), [0xd800]);
    let block = ToolCallBlock::Settled {
        call_id: "call-skill".to_owned(),
        call: None,
        call_view: None,
        result_view: None,
        content: vec![
            JsonValue::parse(r#"{"type":"text","text":"\udfff\nnext"}"#.to_owned()).unwrap(),
        ],
        is_error: true,
        error: None,
    };
    let model = skill_row_model(&block);
    assert_eq!(model.error_summary.unwrap().to_utf16(), [0xdfff]);
    assert_eq!(model.output.unwrap().as_raw(), r#""\udfff\nnext""#);
}

#[test]
fn locale_namespace_and_parallel_copy_are_exact() {
    assert_eq!(SKILL_NS, "skill");
    assert_eq!(SKILL_LOCALES.len(), 5);
    assert_eq!(
        SKILL_LOCALES[2],
        ("row.stopped", "skill 加载已中止", "Skill load stopped")
    );
    assert_eq!(SKILL_LOCALES[4], ("menu.userOnly", "仅用户", "user-only"));
}
