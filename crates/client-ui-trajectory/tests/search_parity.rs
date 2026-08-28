//! Bounded Markdown preview and incremental search-index parity.

use std::rc::Rc;

use seekdeep_client_ui_trajectory::{
    TrajectoryCell, TrajectoryCellKind, TrajectoryGroupModel, TrajectorySearchIndex,
    TrajectorySourceBlock, TrajectoryTurnModel, trajectory_preview_text, trajectory_record_id,
};
use serde_json::json;

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
    let preview = trajectory_preview_text(&output_capped).unwrap();
    assert!(preview.ends_with('…'));
    assert!(preview.encode_utf16().count() <= 513);
    let source_capped = format!("{}TAIL", "a".repeat(2_048));
    assert_eq!(
        trajectory_preview_text(&source_capped).unwrap(),
        format!("{}…", "a".repeat(512))
    );
}

#[test]
fn search_indexes_every_domain_source_with_case_insensitive_all_term_matching() {
    let mut assistant = TrajectoryCell::new(1, TrajectoryCellKind::Message, "Answer");
    assistant.source_seq = Some(10);
    assistant.preview_markdown = Some("**Visible** preview".to_owned());
    assistant.thinking_detail = Some("Hidden Reasoning".to_owned());
    assistant.prompt_detail = Some(json!({"tool": "BASH_SCHEMA"}));
    assistant.source_blocks.push(TrajectorySourceBlock {
        kind: "image".to_owned(),
        content: String::new(),
        image_src: Some("data:image/png;base64,x".to_owned()),
        image_alt: Some("Architecture Diagram".to_owned()),
        call_id: None,
        tool_name: None,
    });
    let mut tool = TrajectoryCell::new(2, TrajectoryCellKind::Tool, "bash · pwd");
    tool.call_id = Some("call-1".to_owned());
    tool.result_preview_markdown = Some("`/workspace`".to_owned());
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
