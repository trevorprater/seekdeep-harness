//! Bounded Markdown preview and incremental search-index parity.

use std::rc::Rc;

use seekdeep_client_ui_trajectory::{
    TrajectoryCell, TrajectoryCellKind, TrajectoryGroupModel, TrajectorySearchIndex,
    TrajectorySourceBlock, TrajectoryTurnModel, trajectory_preview_text, trajectory_record_id,
};
#[path = "../src/json_value.rs"]
#[allow(dead_code)]
mod json_value;
use json_value::json;
use seekdeep_lossless_json::JsonValue as Value;

fn layout(cells: Vec<TrajectoryCell>) -> Rc<Vec<Vec<TrajectoryTurnModel>>> {
    Rc::new(vec![vec![TrajectoryTurnModel {
        turn: Some(2),
        groups: vec![TrajectoryGroupModel {
            title: "Step 1".to_owned(),
            description: None,
            cells,
        }],
    }]])
}

#[test]
fn preview_removes_markdown_collapses_whitespace_and_caps_both_stages() {
    assert_eq!(
        trajectory_preview_text(
            "# Heading\n\n- **bold** and `code`\n- [link](https://example.com)"
        )
        .unwrap(),
        "Heading bold and code link"
    );
    assert_eq!(
        trajectory_preview_text("alpha\n\t beta   gamma").unwrap(),
        "alpha beta gamma"
    );
    let output_capped = "word ".repeat(200);
    let preview = trajectory_preview_text(output_capped.as_str()).unwrap();
    assert!(preview.ends_with("…"));
    assert!(preview.len_utf16() <= 513);
    let source_capped = format!("{}TAIL", "a".repeat(2_048));
    assert_eq!(
        trajectory_preview_text(source_capped.as_str()).unwrap(),
        format!("{}…", "a".repeat(512))
    );
}

#[test]
fn previews_and_search_preserve_lone_surrogates_and_utf16_slice_edges() {
    use seekdeep_lossless_json::JsonString;

    let source = JsonString::parse(r#""**\ud800** \ue000 &#xE001; `\udfff`""#.to_owned()).unwrap();
    assert_eq!(
        trajectory_preview_text(source).unwrap().utf16_units(),
        &[0xd800, 0x20, 0xe000, 0x20, 0xe001, 0x20, 0xdfff]
    );
    let capped = trajectory_preview_text(format!("{}😀tail", "a".repeat(511))).unwrap();
    assert_eq!(&capped.utf16_units()[511..], &[0xd83d, 0x2026]);
    let mut raw = TrajectoryCell::new(
        1,
        TrajectoryCellKind::Message,
        JsonString::from_utf16(&[0xd800]),
    );
    raw.message_source =
        Some(Value::parse(r#"{"kind":"future","\udfff":"\ud800"}"#.to_owned()).unwrap());
    let replaced = TrajectoryCell::new(2, TrajectoryCellKind::Message, "�");
    let expected = trajectory_record_id(&raw);
    let mut index = TrajectorySearchIndex::new();
    index.update(&layout(vec![raw, replaced]));
    assert_eq!(
        index
            .search_json(&JsonString::from_utf16(&[0xd800]))
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        vec![expected]
    );
}

#[test]
fn search_indexes_every_domain_source_with_case_insensitive_all_term_matching() {
    let mut assistant = TrajectoryCell::new(1, TrajectoryCellKind::Message, "Answer");
    assistant.source_seq = Some(10);
    assistant.preview_markdown = Some("**Visible** preview".into());
    assistant.thinking_detail = Some("Hidden Reasoning".into());
    assistant.prompt_detail = Some(json!({"tool": "BASH_SCHEMA"}));
    assistant.source_blocks.push(TrajectorySourceBlock {
        kind: "image".to_owned(),
        content: seekdeep_lossless_json::JsonString::default(),
        image_src: Some("data:image/png;base64,x".to_owned()),
        image_alt: Some("Architecture Diagram".into()),
        call_id: None,
        tool_name: None,
    });
    let mut tool = TrajectoryCell::new(2, TrajectoryCellKind::Tool, "bash · pwd");
    tool.call_id = Some("call-1".to_owned());
    tool.result_preview_markdown = Some("`/workspace`".into());
    let layouts = layout(vec![assistant.clone(), tool.clone()]);
    let mut index = TrajectorySearchIndex::new();
    assert!(index.update(&layouts));
    assert!(!index.update(&layouts));

    let assistant_id = trajectory_record_id(&assistant);
    let tool_id = trajectory_record_id(&tool);
    assert_eq!(
        index
            .search("visible REASONING")
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        vec![assistant_id.clone()]
    );
    assert_eq!(
        index
            .search("architecture diagram")
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        vec![assistant_id.clone()]
    );
    assert_eq!(
        index
            .search("bash_schema")
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        vec![assistant_id]
    );
    assert_eq!(
        index
            .search("WORKSPACE")
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        vec![tool_id]
    );
    assert_eq!(index.search("missing").unwrap().len(), 0);
    assert_eq!(index.search("   "), None);
}

#[test]
fn update_skips_boundaries_and_removes_records_absent_from_the_new_layout_identity() {
    let mut boundary = TrajectoryCell::new(1, TrajectoryCellKind::System, "secret boundary");
    boundary.source_seq = Some(1);
    boundary.request_only = Some(true);
    let mut visible = TrajectoryCell::new(2, TrajectoryCellKind::User, "keep me");
    visible.source_seq = Some(2);
    let mut index = TrajectorySearchIndex::new();
    assert!(index.update(&layout(vec![boundary, visible.clone()])));
    assert!(index.search("secret").unwrap().is_empty());
    assert_eq!(index.search("keep").unwrap().len(), 1);
    assert!(index.update(&layout(Vec::new())));
    assert!(index.search("keep").unwrap().is_empty());
}
