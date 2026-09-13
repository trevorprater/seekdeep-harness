//! Frozen Cordis call/result view-model parity.

use seekdeep_client_ui_cordis::*;
use serde_json::json;

const ARGS: &str = r#"{"name":"clock","purpose":"顶栏时钟","code":{"client":"return {}","host":"harness.handle('now', () => Date.now())"}}"#;

fn running(args_raw: &str) -> ToolCallBlock {
    ToolCallBlock::Running(RunningToolCall {
        args_raw: args_raw.to_owned(),
    })
}

fn settled() -> SettledToolResult {
    SettledToolResult {
        kind: "tool-result".to_owned(),
        seq: 2,
        call: Some(SettledToolCall {
            name: "cordis_define".to_owned(),
            args_raw: ARGS.to_owned(),
        }),
        content: vec![json!({"type": "text", "text": "defined dyn-1"})],
        is_error: false,
        error: None,
        meta: Some(json!({"pluginId": "dyn-1", "packageId": "pkg-1"})),
    }
}

#[test]
fn define_reads_name_purpose_and_both_code_halves_from_arguments() {
    let card = cordis_define_card(&running(ARGS));
    assert_eq!(card.name.as_deref(), Some("clock"));
    assert_eq!(card.purpose.as_deref(), Some("顶栏时钟"));
    assert_eq!(card.client_code.as_deref(), Some("return {}"));
    assert!(
        card.host_code
            .as_deref()
            .unwrap()
            .contains("harness.handle")
    );
    assert_eq!(card.state, CordisToolState::Running);
    assert_eq!(card.output, None);
    assert_eq!(card.plugin_id, None);
    assert_eq!(card.package_id, None);
}

#[test]
fn define_takes_minted_id_and_output_from_successful_result_meta() {
    let card = cordis_define_card(&ToolCallBlock::Settled(Box::new(settled())));
    assert_eq!(card.plugin_id.unwrap().as_str(), "dyn-1");
    assert_eq!(card.package_id.unwrap().as_str(), "pkg-1");
    assert_eq!(card.output.as_deref(), Some("defined dyn-1"));
    assert_eq!(card.state, CordisToolState::Ok);
}

#[test]
fn define_stays_read_only_when_meta_has_no_usable_identity() {
    for meta in [
        None,
        Some(json!("dyn-1")),
        Some(json!({"pluginId": ""})),
        Some(json!({"pluginId": 7})),
    ] {
        let mut result = settled();
        result.meta = meta;
        assert_eq!(
            cordis_define_card(&ToolCallBlock::Settled(Box::new(result))).plugin_id,
            None
        );
    }
}

#[test]
fn define_classifies_error_interruption_empty_and_non_text_results() {
    let mut failed = settled();
    failed.is_error = true;
    failed.content = vec![json!({
        "type": "text",
        "text": "SyntaxError: unexpected token\n  at line 3"
    })];
    let failed = cordis_define_card(&ToolCallBlock::Settled(Box::new(failed)));
    assert_eq!(failed.state, CordisToolState::Error);
    assert_eq!(
        failed.error_summary.as_deref(),
        Some("SyntaxError: unexpected token")
    );
    assert_eq!(failed.plugin_id, None);

    let mut interrupted = settled();
    interrupted.is_error = true;
    interrupted.error = Some(ToolResultError {
        name: "E".to_owned(),
        code: "interrupted".to_owned(),
    });
    assert_eq!(
        cordis_define_card(&ToolCallBlock::Settled(Box::new(interrupted))).state,
        CordisToolState::Stopped
    );

    let mut empty = settled();
    empty.content.clear();
    assert_eq!(
        cordis_define_card(&ToolCallBlock::Settled(Box::new(empty.clone()))).output,
        None
    );
    empty.error = Some(ToolResultError {
        name: "E".to_owned(),
        code: "boom".to_owned(),
    });
    assert_eq!(
        cordis_define_card(&ToolCallBlock::Settled(Box::new(empty)))
            .output
            .as_deref(),
        Some("E: boom")
    );

    let mut non_text = settled();
    non_text.content = vec![json!({"type": "reasoning", "text": "weighing it"})];
    assert!(
        cordis_define_card(&ToolCallBlock::Settled(Box::new(non_text)))
            .output
            .unwrap()
            .contains("\"type\": \"reasoning\"")
    );
}

#[test]
fn define_degrades_on_truncated_or_non_object_argument_streams() {
    let truncated = cordis_define_card(&running(r#"{"name":"clo"#));
    assert_eq!(truncated.name.as_deref(), Some(r#"{"name":"clo"#));
    assert_eq!(truncated.purpose, None);
    assert_eq!(
        cordis_define_card(&running(r#""just a string""#))
            .name
            .as_deref(),
        Some(r#""just a string""#)
    );
}

#[test]
fn define_uses_raw_first_line_when_arguments_have_no_name() {
    assert_eq!(
        cordis_define_card(&running(r#"{"purpose":"顶栏时钟"}"#))
            .name
            .as_deref(),
        Some(r#"{"purpose":"顶栏时钟"}"#)
    );
    assert_eq!(
        cordis_define_card(&running(r#"{"name":"","purpose":"顶栏时钟"}"#))
            .name
            .as_deref(),
        Some(r#"{"name":"","purpose":"顶栏时钟"}"#)
    );
}

#[test]
fn define_reports_no_name_when_the_event_window_cut_the_call_head() {
    let mut result = settled();
    result.call = None;
    let card = cordis_define_card(&ToolCallBlock::Settled(Box::new(result)));
    assert_eq!(card.name, None);
    assert_eq!(card.purpose, None);
}

#[test]
fn action_keeps_plugin_identity_and_lifecycle_result() {
    let mut result = settled();
    result.call = Some(SettledToolCall {
        name: "cordis_stop".to_owned(),
        args_raw: r#"{"pluginId":"clock-1"}"#.to_owned(),
    });
    result.content = vec![json!({"type": "text", "text": "Stopped clock-1."})];
    result.meta = None;
    let card = cordis_action_card(&ToolCallBlock::Settled(Box::new(result)));
    assert_eq!(card.plugin_id.unwrap().as_str(), "clock-1");
    assert_eq!(card.output.as_deref(), Some("Stopped clock-1."));
    assert_eq!(card.error_summary, None);
    assert_eq!(card.state, CordisToolState::Ok);
}
