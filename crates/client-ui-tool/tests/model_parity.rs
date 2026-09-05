//! Generic row and diff/read/Web card model parity.

use seekdeep_client_ui_tool::{
    CHAT_CARD_MAX_LINES, SearchCard, ToolCallBlock, ToolCallHead, ToolErrorInfo, ToolRowState,
    ToolRowVariant, WebCardModel, classify_tool, diff_card_model, plan_summary, read_card_model,
    relativize_to_cwd, result_text, search_card_model, terminal_card_model, terminal_failed,
    tool_row_model, web_card_model,
};
use serde_json::json;

fn running(args: &str, view: Option<serde_json::Value>) -> ToolCallBlock {
    ToolCallBlock::Running {
        call_id: "call-1".to_owned(),
        args_raw: args.to_owned(),
        call_view: view,
    }
}

fn settled(
    args: Option<&str>,
    call_view: Option<serde_json::Value>,
    result_view: Option<serde_json::Value>,
) -> ToolCallBlock {
    ToolCallBlock::Settled {
        call_id: "call-1".to_owned(),
        call: args.map(|args| ToolCallHead {
            args_raw: args.to_owned(),
        }),
        call_view,
        result_view,
        content: vec![json!({"type":"text","text":"done\nsecond"})],
        is_error: false,
        error: None,
    }
}

#[test]
fn generic_row_classifies_summarizes_relativizes_and_handles_code_and_errors() {
    assert_eq!(classify_tool("grep"), ToolRowVariant::Search);
    assert_eq!(classify_tool("pwsh"), ToolRowVariant::Bash);
    assert_eq!(classify_tool("web_fetch"), ToolRowVariant::Read);
    assert_eq!(classify_tool("unknown"), ToolRowVariant::Others);
    let bash = tool_row_model(
        "bash",
        &running(r#"{"command":"pwd","description":"Show cwd"}"#, None),
        None,
    );
    assert_eq!(bash.title, "Bash");
    assert_eq!(bash.summary, "Show cwd");
    assert_eq!(bash.state, ToolRowState::Running);
    assert!(
        bash.body
            .as_deref()
            .unwrap()
            .contains("\"command\": \"pwd\"")
    );
    assert_eq!(
        tool_row_model(
            "mystery",
            &running(r#"[42,"first\nsecond","later"]"#, None),
            None
        )
        .summary,
        "mystery · first"
    );

    let code = tool_row_model(
        "run_code",
        &running(r#"{"description":"inspect","code":"return 42"}"#, None),
        None,
    );
    assert_eq!(code.body.as_deref(), Some("return 42"));
    assert_eq!(relativize_to_cwd("/work/a.rs", Some("/work/")), "a.rs");
    assert_eq!(
        relativize_to_cwd("/workspace/a.rs", Some("/work")),
        "/workspace/a.rs"
    );

    let error = ToolCallBlock::Settled {
        call_id: "failed".to_owned(),
        call: None,
        call_view: None,
        result_view: None,
        content: Vec::new(),
        is_error: true,
        error: Some(ToolErrorInfo {
            name: "ToolError".to_owned(),
            code: "BROKEN".to_owned(),
        }),
    };
    assert_eq!(result_text(&error), "ToolError: BROKEN");
    let row = tool_row_model("mystery", &error, None);
    assert_eq!(row.summary, "mystery · failed");
    assert_eq!(row.error_summary.as_deref(), Some("ToolError: BROKEN"));
}

#[test]
fn diff_card_uses_call_while_running_and_result_when_settled_and_rejects_malformed() {
    assert_eq!(CHAT_CARD_MAX_LINES, 8);
    let intended = json!({
        "card":"diff",
        "diffs":[{"path":"a.txt","oldText":null,"newText":"new"}],
    });
    assert_eq!(
        diff_card_model(&running("{}", Some(intended.clone())))
            .unwrap()
            .diffs[0]
            .path,
        "a.txt"
    );
    let applied = json!({
        "card":"diff",
        "diffs":[{"path":"a.txt","oldText":"old","newText":"new"}],
    });
    let model = diff_card_model(&settled(Some("{}"), Some(intended), Some(applied))).unwrap();
    assert_eq!(model.diffs[0].old_text.as_deref(), Some("old"));
    assert!(
        diff_card_model(&settled(
            Some("{}"),
            None,
            Some(json!({"card":"diff","diffs":[{"path":42}]})),
        ))
        .is_none()
    );
}

#[test]
fn read_card_is_result_only_detached_and_uses_replacement_or_relative_label() {
    assert!(read_card_model(&running("{}", None), Some("/work")).is_none());
    let view = json!({
        "card":"read",
        "path":"/work/src/lib.rs",
        "lines":[{"number":1,"text":"fn main() {}"}],
        "totalLines":10,
        "lang":"rust",
    });
    let model = read_card_model(&settled(Some("{}"), None, Some(view)), Some("/work")).unwrap();
    assert_eq!(model.label, "src/lib.rs");
    assert_eq!(model.lines[0].number, 1);
    assert_eq!(model.total_lines, 10);
    assert_eq!(model.lang.as_deref(), Some("rust"));
}

#[test]
fn web_card_is_result_only_copies_sources_and_rejects_unknown_wire_kinds() {
    let search = web_card_model(&settled(
        Some("{}"),
        None,
        Some(json!({
            "card":"web",
            "kind":"search",
            "answer":"answer",
            "sources":[{"url":"https://example.test","title":"Example"}],
            "truncated":true,
        })),
    ))
    .unwrap();
    let WebCardModel::Search {
        answer,
        sources,
        truncated,
    } = search
    else {
        panic!("expected search card");
    };
    assert_eq!(answer.as_deref(), Some("answer"));
    assert_eq!(sources[0].url, "https://example.test");
    assert!(truncated);
    assert!(
        web_card_model(&settled(
            Some("{}"),
            None,
            Some(json!({"card":"web","kind":"future"})),
        ))
        .is_none()
    );
}

#[test]
fn search_card_preserves_both_shapes_titles_and_capped_recovery() {
    let matches = ToolCallBlock::Settled {
        call_id: "grep-1".to_owned(),
        call: None,
        call_view: None,
        result_view: Some(json!({
            "card":"search",
            "shape":"matches",
            "files":[{
                "path":"a.rs",
                "matches":[{"lineNumber":12,"line":"let found = true;"}],
            }],
            "truncated":true,
            "total":42,
            "title":"42 matches",
        })),
        content: vec![
            json!({"type":"image","data":"ignored"}),
            json!({"type":"text","text":"shown rows"}),
            json!({"type":"text","text":"Full result at spill://grep-1"}),
        ],
        is_error: false,
        error: None,
    };
    let model = search_card_model(&matches).unwrap();
    assert_eq!(model.title.as_deref(), Some("42 matches"));
    assert_eq!(
        model.recovery.as_deref(),
        Some("shown rows\nFull result at spill://grep-1")
    );
    let SearchCard::Matches {
        files,
        truncated,
        total,
    } = model.card
    else {
        panic!("expected matches card");
    };
    assert_eq!(files[0].matches[0].line_number.as_u64(), Some(12));
    assert!(truncated);
    assert_eq!(total.as_u64(), Some(42));

    let paths = search_card_model(&settled(
        Some("{}"),
        None,
        Some(json!({
            "card":"search",
            "shape":"paths",
            "paths":["src/a.rs","src/b.rs"],
            "truncated":false,
            "total":2,
        })),
    ))
    .unwrap();
    let SearchCard::Paths {
        paths,
        truncated,
        total,
    } = paths.card
    else {
        panic!("expected paths card");
    };
    assert_eq!(paths, ["src/a.rs", "src/b.rs"]);
    assert!(!truncated);
    assert_eq!(total.as_u64(), Some(2));
}

#[test]
fn search_card_is_result_only_and_rejects_unknown_or_malformed_shapes() {
    assert!(search_card_model(&running("{}", None)).is_none());
    for result_view in [
        json!({"card":"generic"}),
        json!({"card":"search","shape":"future","truncated":false,"total":0}),
        json!({"card":"search","shape":"matches","truncated":false,"total":0}),
        json!({
            "card":"search",
            "shape":"matches",
            "files":[{"path":"a.rs","matches":[{"lineNumber":"x","line":1}]}],
            "truncated":false,
            "total":1,
        }),
        json!({"card":"search","shape":"paths","truncated":false,"total":0}),
        json!({
            "card":"search",
            "shape":"paths",
            "paths":[42],
            "truncated":false,
            "total":1,
        }),
    ] {
        assert!(search_card_model(&settled(Some("{}"), None, Some(result_view))).is_none());
    }
}

fn terminal_call(cwd: Option<&str>) -> serde_json::Value {
    let mut view = json!({
        "card":"terminal",
        "title":"ls -la",
        "description":"List files",
    });
    if let Some(cwd) = cwd {
        view["cwd"] = json!(cwd);
    }
    view
}

fn terminal_result() -> serde_json::Value {
    json!({"card":"terminal","output":"a.rs  b.rs\n","exitCode":0})
}

#[test]
fn terminal_card_combines_running_and_settled_authority_and_failure_state() {
    let pending = terminal_card_model(
        &running("{}", Some(terminal_call(Some("/projects/app")))),
        None,
    )
    .unwrap();
    assert_eq!(pending.description.as_deref(), Some("List files"));
    assert_eq!(pending.card.command, "ls -la");
    assert_eq!(pending.card.cwd.as_deref(), Some("/projects/app"));
    assert!(pending.card.running);
    assert!(!terminal_failed(&pending));

    let mut replacement = terminal_result();
    replacement["title"] = json!("ls -la --color=never");
    replacement["exitCode"] = json!(2);
    replacement["output"] = json!("boom\n");
    let failed = terminal_card_model(
        &settled(
            Some("{}"),
            Some(terminal_call(Some("/projects/app"))),
            Some(replacement),
        ),
        None,
    )
    .unwrap();
    assert_eq!(failed.card.command, "ls -la --color=never");
    assert_eq!(failed.card.output.as_deref(), Some("boom\n"));
    assert!(terminal_failed(&failed));

    let signal = terminal_card_model(
        &settled(
            Some("{}"),
            Some(terminal_call(None)),
            Some(json!({"card":"terminal","output":"","signal":"SIGTERM"})),
        ),
        None,
    )
    .unwrap();
    assert!(terminal_failed(&signal));
}

#[test]
fn terminal_card_resolves_and_normalizes_cross_platform_workdirs() {
    let cwd_of = |view_cwd: Option<&str>, session_cwd: Option<&str>| {
        terminal_card_model(
            &settled(
                Some("{}"),
                Some(terminal_call(view_cwd)),
                Some(terminal_result()),
            ),
            session_cwd,
        )
        .unwrap()
        .card
        .cwd
    };
    assert_eq!(cwd_of(None, Some("/w/app")).as_deref(), Some("/w/app"));
    assert_eq!(cwd_of(None, Some("")).as_deref(), Some(""));
    assert_eq!(
        cwd_of(Some("packages/ui"), Some("/w/app")).as_deref(),
        Some("/w/app/packages/ui")
    );
    assert_eq!(cwd_of(Some(".."), Some("/w/app")).as_deref(), Some("/w"));
    assert_eq!(cwd_of(Some("../../.."), Some("/w")).as_deref(), Some("/"));
    assert_eq!(
        cwd_of(Some("/srv/./app/../other"), Some("/w/app")).as_deref(),
        Some("/srv/other")
    );
    assert_eq!(
        cwd_of(Some(r"C:\ws\app\.."), Some("/w")).as_deref(),
        Some(r"C:\ws")
    );
    assert_eq!(
        cwd_of(Some("../elsewhere"), None).as_deref(),
        Some("../elsewhere")
    );
    assert_eq!(
        cwd_of(Some(".."), Some(r"\\server\share\app")).as_deref(),
        Some(r"\\server\share")
    );
}

#[test]
fn terminal_card_handles_window_truncation_and_generic_fallbacks() {
    let truncated = terminal_card_model(
        &settled(
            None,
            None,
            Some(json!({"card":"terminal","title":"ls -la","output":""})),
        ),
        Some("/w/app"),
    )
    .unwrap();
    assert_eq!(truncated.card.command, "ls -la");
    assert_eq!(truncated.card.cwd, None);
    assert_eq!(truncated.description, None);

    let empty_command = terminal_card_model(
        &settled(None, None, Some(json!({"card":"terminal","output":""}))),
        Some("/w/app"),
    )
    .unwrap();
    assert_eq!(empty_command.card.command, "");
    assert_eq!(empty_command.card.cwd, None);

    assert!(terminal_card_model(&running("{}", None), None).is_none());
    assert!(
        terminal_card_model(
            &settled(
                Some("{}"),
                Some(terminal_call(None)),
                Some(json!({"card":"generic"})),
            ),
            None,
        )
        .is_none()
    );
}

#[test]
fn plan_summary_counts_parallel_work_and_rejects_only_the_first_unusable_name() {
    let summary = plan_summary(&[
        json!({"content":"done","status":"completed"}),
        json!({"content":"first","status":"in_progress"}),
        json!({"content":"second","status":"in_progress"}),
        json!({"content":"later","status":"pending"}),
    ]);
    assert_eq!(summary.done, 1);
    assert_eq!(summary.total, 4);
    assert_eq!(summary.active_content.as_deref(), Some("first"));
    assert_eq!(summary.active_extra, 1);

    let unusable = plan_summary(&[
        json!({"content":"   ","status":"in_progress"}),
        json!({"content":"second","status":"in_progress"}),
    ]);
    assert_eq!(unusable.active_content, None);
    assert_eq!(unusable.active_extra, 0);

    assert_eq!(
        plan_summary(&[]),
        seekdeep_client_ui_tool::PlanSummary {
            done: 0,
            total: 0,
            active_content: None,
            active_extra: 0,
        }
    );
}
