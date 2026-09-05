//! Pure Rust parity for trajectory-ledger projection and inspector decisions.

use std::collections::{BTreeMap, BTreeSet};

use seekdeep_client_ui_trajectory::{
    AssistantMetricDetail, CollapsedSummaryKind, TrajectoryCell, TrajectoryCellKind,
    TrajectoryDetailTab, TrajectoryGroupModel, TrajectoryRecordState, TrajectoryRequestNumber,
    TrajectoryRequestPurpose, TrajectorySourceBlock, TrajectoryTableRecord, TrajectoryTurnModel,
    TrajectoryUsage, collapse_trajectory_assistant_records, collapse_trajectory_turn_records,
    filter_trajectory_table_records, flatten_trajectory_table_records,
    format_trajectory_detail_duration, index_trajectory_request_boundaries,
    index_trajectory_request_boundary_runs, index_trajectory_request_numbers,
    parse_trajectory_json_container, parse_trajectory_tool_schema,
    summarize_trajectory_assistant_tools, summarize_trajectory_turn,
    trajectory_assistant_generation_time, trajectory_assistant_throughput,
    trajectory_assistant_tool_calls, trajectory_assistant_total_time, trajectory_assistant_ttft,
    trajectory_detail_tabs, trajectory_input_total, trajectory_is_tool_call_only,
    trajectory_message_source_label, trajectory_parent_records, trajectory_record_display_text,
    trajectory_record_id, trajectory_record_result_text, trajectory_record_state,
    trajectory_request_key, trajectory_request_step, trajectory_section_label,
    trajectory_status_label, trajectory_tool_call_text_parts,
};
use serde_json::json;

fn cell(index: usize, kind: TrajectoryCellKind, text: &str) -> TrajectoryCell {
    TrajectoryCell::new(index, kind, text)
}

fn group(title: &str, cells: Vec<TrajectoryCell>) -> TrajectoryGroupModel {
    TrajectoryGroupModel {
        title: title.to_owned(),
        description: None,
        cells,
    }
}

fn turn(turn: Option<u64>, groups: Vec<TrajectoryGroupModel>) -> TrajectoryTurnModel {
    TrajectoryTurnModel { turn, groups }
}

fn request(turn: Option<u64>, group: &str, number: u64) -> TrajectoryRequestNumber {
    TrajectoryRequestNumber {
        seq: None,
        group: group.to_owned(),
        number,
        status: None,
        started_at: None,
        completed_at: None,
        error: None,
        retry: None,
        max_retries: None,
        retry_delay_ms: None,
        result_seq: None,
        provider: None,
        model: None,
        request_config: None,
        usage: None,
        cumulative_usage: None,
        purpose: TrajectoryRequestPurpose::Assistant,
        turn,
        step: 1,
    }
}

fn ids(records: &[TrajectoryTableRecord]) -> Vec<usize> {
    records.iter().map(|record| record.cell.index).collect()
}

#[test]
fn flatten_and_filter_recompute_exact_section_boundaries() {
    let mut request_only = cell(1, TrajectoryCellKind::System, "request");
    request_only.request_only = Some(true);
    let turns = vec![
        turn(
            Some(1),
            vec![
                group(
                    "Step 1",
                    vec![
                        request_only,
                        cell(2, TrajectoryCellKind::User, "steer"),
                        cell(3, TrajectoryCellKind::Message, "answer"),
                    ],
                ),
                group("Step 2", vec![cell(4, TrajectoryCellKind::Tool, "bash")]),
            ],
        ),
        turn(
            None,
            vec![group(
                "Context",
                vec![cell(5, TrajectoryCellKind::Compacted, "summary")],
            )],
        ),
    ];

    let flattened = flatten_trajectory_table_records(&turns);
    assert_eq!(ids(&flattened), vec![1, 2, 3, 4, 5]);
    assert!(!flattened[0].turn_start);
    assert!(flattened[1].turn_start);
    assert!(flattened[0].group_start);
    assert!(flattened[3].group_start);
    assert!(flattened[3].turn_end);
    assert!(flattened[4].turn_start);
    assert!(flattened[4].turn_end);

    let filtered = filter_trajectory_table_records(&flattened, &BTreeSet::from([3, 4, 5]));
    assert_eq!(ids(&filtered), vec![3, 4, 5]);
    assert!(filtered[0].group_start);
    assert!(filtered[0].turn_start);
    assert!(!filtered[0].turn_end);
    assert!(filtered[1].group_start);
    assert!(filtered[1].turn_end);
    assert!(filtered[2].group_start);
    assert!(filtered[2].turn_start);
    assert!(filtered[2].turn_end);
}

#[test]
fn request_markers_skip_leading_steering_and_keep_global_numbers() {
    let records = flatten_trajectory_table_records(&[
        turn(
            Some(1),
            vec![group(
                "Step 2",
                vec![
                    cell(1, TrajectoryCellKind::User, "change direction"),
                    cell(2, TrajectoryCellKind::Message, "continued"),
                ],
            )],
        ),
        turn(
            Some(2),
            vec![group(
                "Step 1",
                vec![cell(3, TrajectoryCellKind::Message, "next")],
            )],
        ),
        turn(
            None,
            vec![group(
                "Context",
                vec![cell(4, TrajectoryCellKind::Context, "between")],
            )],
        ),
    ]);
    let boundaries = index_trajectory_request_boundaries(&records);
    assert_eq!(
        boundaries.get(&trajectory_request_key(Some(1), "Step 2")),
        Some(&2)
    );
    assert_eq!(
        boundaries.get(&trajectory_request_key(Some(2), "Step 1")),
        Some(&3)
    );
    assert_eq!(
        boundaries.get(&trajectory_request_key(None, "Context")),
        Some(&4)
    );

    let numbers =
        index_trajectory_request_numbers(&records, &[request(Some(1), "Step 2", 7)], &boundaries);
    assert_eq!(
        numbers,
        BTreeMap::from([
            (trajectory_request_key(Some(1), "Step 2"), 7),
            (trajectory_request_key(Some(2), "Step 1"), 8),
        ])
    );
    assert_eq!(trajectory_request_step("Step 2"), Some(2));
    assert_eq!(trajectory_request_step("Step 1.0"), Some(1));
    assert_eq!(trajectory_request_step("Step 1e2"), Some(100));
    assert_eq!(trajectory_request_step("Step 0"), None);
    assert_eq!(trajectory_request_step("Context"), None);
    assert_eq!(trajectory_section_label(None), "Between turns");
    assert_eq!(trajectory_section_label(Some(3)), "Turn 3");
}

#[test]
fn coincident_request_markers_are_indexed_left_to_right() {
    let request_cell = |index| {
        let mut record = cell(index, TrajectoryCellKind::Message, "");
        record.request_only = Some(true);
        record
    };
    let records = flatten_trajectory_table_records(&[
        turn(Some(1), vec![group("Step 1", vec![request_cell(1)])]),
        turn(Some(2), vec![group("Step 1", vec![request_cell(2)])]),
        turn(
            Some(3),
            vec![group(
                "Step 1",
                vec![cell(3, TrajectoryCellKind::Message, "recovered")],
            )],
        ),
    ]);
    assert_eq!(
        index_trajectory_request_boundary_runs(&records),
        BTreeMap::from([(1, 0), (2, 1), (3, 2)])
    );
}

#[test]
fn turn_and_assistant_folds_preserve_first_rows_and_exact_summaries() {
    let mut system = cell(1, TrajectoryCellKind::System, "prompt");
    system.prompt_detail = Some(json!({"system": "test", "tools": []}));
    let mut assistant = cell(2, TrajectoryCellKind::Message, "Checking files");
    assistant.source_seq = Some(20);
    let records = flatten_trajectory_table_records(&[turn(
        Some(1),
        vec![
            group(
                "Step 1",
                vec![
                    system,
                    assistant.clone(),
                    cell(3, TrajectoryCellKind::Tool, "bash · pwd"),
                    cell(4, TrajectoryCellKind::Subtool, "bash · ls"),
                ],
            ),
            group("Step 2", vec![cell(5, TrajectoryCellKind::Message, "done")]),
        ],
    )]);
    assert_eq!(
        summarize_trajectory_turn(&records[2..]),
        "2 steps · 2 tool calls"
    );

    let folded_turn = collapse_trajectory_turn_records(&records, &BTreeSet::from([1]));
    assert_eq!(ids(&folded_turn), vec![1, 2, 2]);
    assert_eq!(
        folded_turn[2].collapsed_summary.as_deref(),
        Some("2 steps · 2 tool calls")
    );
    assert_eq!(
        folded_turn[2].collapsed_summary_kind,
        Some(CollapsedSummaryKind::Turn)
    );
    assert!(folded_turn[2].turn_end);

    let folded_assistant = collapse_trajectory_assistant_records(
        &records,
        &BTreeSet::from([trajectory_record_id(&assistant)]),
    );
    assert_eq!(ids(&folded_assistant), vec![1, 2, 2, 5]);
    assert_eq!(
        folded_assistant[2].collapsed_summary.as_deref(),
        Some("2 tool calls · bash")
    );
    assert_eq!(
        folded_assistant[2].collapsed_summary_kind,
        Some(CollapsedSummaryKind::Assistant)
    );
    assert_eq!(trajectory_assistant_tool_calls(&records, 2).len(), 2);
    assert_eq!(
        summarize_trajectory_assistant_tools(&records[2..4]),
        "2 tool calls · bash"
    );
}

#[test]
fn record_states_and_assistant_timing_match_inspector_labels() {
    let records = flatten_trajectory_table_records(&[turn(
        Some(1),
        vec![group(
            "Step 1",
            vec![
                cell(1, TrajectoryCellKind::Tool, "pending"),
                TrajectoryCell {
                    is_error: Some(true),
                    output_detail: Some("ToolError".to_owned()),
                    ..cell(2, TrajectoryCellKind::Tool, "failed")
                },
                TrajectoryCell {
                    time_seconds: Some(0.1),
                    ..cell(3, TrajectoryCellKind::Compacted, "complete")
                },
            ],
        )],
    )]);
    assert_eq!(
        trajectory_record_state(&records[0]),
        TrajectoryRecordState::Running
    );
    assert_eq!(
        trajectory_record_state(&records[1]),
        TrajectoryRecordState::Error
    );
    assert_eq!(
        trajectory_record_state(&records[2]),
        TrajectoryRecordState::Complete
    );
    assert_eq!(
        trajectory_status_label(TrajectoryRecordState::Running),
        "Pending"
    );
    assert_eq!(
        trajectory_status_label(TrajectoryRecordState::Error),
        "Failed"
    );
    assert_eq!(
        trajectory_status_label(TrajectoryRecordState::Complete),
        "Completed"
    );

    let metrics = AssistantMetricDetail {
        timing_recorded: true,
        step_start_time: Some(1_000.0),
        first_token_time: Some(1_500.0),
        completed_time: Some(2_500.0),
        usage_provided: true,
        output_tokens: Some(20),
    };
    assert_eq!(trajectory_assistant_total_time(&metrics), "1.50 s");
    assert_eq!(trajectory_assistant_ttft(&metrics), "500 ms");
    assert_eq!(trajectory_assistant_generation_time(&metrics), "1.00 s");
    assert_eq!(trajectory_assistant_throughput(&metrics), "20.0 tok/s");
    assert_eq!(format_trajectory_detail_duration(10_000.0), "10.0 s");

    let unrecorded = AssistantMetricDetail {
        timing_recorded: false,
        step_start_time: None,
        first_token_time: None,
        completed_time: None,
        usage_provided: false,
        output_tokens: None,
    };
    assert_eq!(trajectory_assistant_total_time(&unrecorded), "Not recorded");
    assert_eq!(trajectory_assistant_ttft(&unrecorded), "Not recorded");
    assert_eq!(
        trajectory_assistant_generation_time(&unrecorded),
        "First token unavailable"
    );
    assert_eq!(
        trajectory_assistant_throughput(&unrecorded),
        "Usage unavailable"
    );
}

#[test]
fn tabs_display_text_and_tool_splitting_preserve_source_rules() {
    let mut tool_call_only = cell(1, TrajectoryCellKind::Message, "Tool call only");
    tool_call_only.source_blocks.push(TrajectorySourceBlock {
        kind: "tool-call".to_owned(),
        content: "{}".to_owned(),
        image_src: None,
        image_alt: None,
        call_id: Some("call-1".to_owned()),
        tool_name: Some("read".to_owned()),
    });
    assert!(trajectory_is_tool_call_only(&tool_call_only));
    assert_eq!(trajectory_record_display_text(&tool_call_only).unwrap(), "");

    let context = TrajectoryCell {
        input_detail: Some(
            "<background-job-complete>\nExit code: 0\n</background-job-complete>".to_owned(),
        ),
        ..cell(2, TrajectoryCellKind::Context, "")
    };
    assert!(
        trajectory_record_display_text(&context)
            .unwrap()
            .contains("<background-job-complete>")
    );

    let mut tool = cell(3, TrajectoryCellKind::Tool, "bash · {\"command\":\"pwd\"}");
    tool.input_detail = Some("{\"command\":\"pwd\"}".to_owned());
    tool.output_detail = Some("ok".to_owned());
    let records = flatten_trajectory_table_records(&[turn(
        Some(1),
        vec![group("Step 1", vec![tool_call_only, context, tool.clone()])],
    )]);
    let tool_tabs = trajectory_detail_tabs(&records[2]);
    assert_eq!(
        tool_tabs
            .iter()
            .map(|tab| (tab.id, tab.label))
            .collect::<Vec<_>>(),
        vec![
            (TrajectoryDetailTab::Overview, "Summary"),
            (TrajectoryDetailTab::Input, "Payload"),
            (TrajectoryDetailTab::Output, "Result"),
            (TrajectoryDetailTab::Schema, "Schema"),
            (TrajectoryDetailTab::Timing, "Timing"),
        ]
    );
    let parts = trajectory_tool_call_text_parts(tool.kind, &tool.text).unwrap();
    assert_eq!(parts.name, "bash");
    assert_eq!(parts.arguments.as_deref(), Some("{\"command\":\"pwd\"}"));
    assert!(trajectory_tool_call_text_parts(TrajectoryCellKind::User, "bash").is_none());

    let compacted = flatten_trajectory_table_records(&[turn(
        Some(2),
        vec![group(
            "Compaction",
            vec![cell(4, TrajectoryCellKind::Compacted, "summary")],
        )],
    )]);
    assert_eq!(trajectory_detail_tabs(&compacted[0])[1].label, "Raw Output");

    tool.result = Some("fallback".to_owned());
    tool.result_preview_markdown = Some("**preview**".to_owned());
    assert_eq!(
        trajectory_record_result_text(&tool).unwrap().as_deref(),
        Some("preview")
    );
}

#[test]
fn hierarchy_sources_usage_and_json_shapes_are_exact() {
    let mut assistant = cell(1, TrajectoryCellKind::Message, "assistant");
    assistant.source_blocks.push(TrajectorySourceBlock {
        kind: "tool-call".to_owned(),
        content: "{}".to_owned(),
        image_src: None,
        image_alt: None,
        call_id: Some("call-1".to_owned()),
        tool_name: Some("bash".to_owned()),
    });
    let mut tool = cell(2, TrajectoryCellKind::Tool, "bash");
    tool.call_id = Some("call-1".to_owned());
    let mut subtool = cell(3, TrajectoryCellKind::Subtool, "read");
    subtool.call_id = Some("child".to_owned());
    let records = flatten_trajectory_table_records(&[turn(
        Some(1),
        vec![group("Step 1", vec![assistant, tool, subtool])],
    )]);
    assert_eq!(
        trajectory_parent_records(&records, &records[2]),
        seekdeep_client_ui_trajectory::TrajectoryParentRecords {
            message: Some(1),
            tool: Some(2),
        }
    );

    assert_eq!(
        trajectory_message_source_label(&json!({"kind": "user"})),
        "User"
    );
    assert_eq!(
        trajectory_message_source_label(&json!({"kind": "plugin", "plugin": "memory"})),
        "Plugin · memory"
    );
    assert_eq!(
        trajectory_message_source_label(&json!({"kind": "goal", "round": 2})),
        "Goal · Round 2"
    );
    assert_eq!(
        trajectory_message_source_label(&json!({"kind": "worker"})),
        "Worker"
    );
    assert_eq!(trajectory_message_source_label(&json!([])), "Unknown");

    assert_eq!(
        trajectory_input_total(TrajectoryUsage {
            input: Some(10),
            cache_read: Some(20),
            cache_write: Some(30),
            output: Some(40),
            reasoning: Some(5),
        }),
        Some(60)
    );
    assert_eq!(trajectory_input_total(TrajectoryUsage::default()), None);
    assert!(parse_trajectory_json_container("[1,2]").is_some());
    assert!(parse_trajectory_json_container("1").is_none());
    assert!(parse_trajectory_json_container("invalid").is_none());
    let schema = parse_trajectory_tool_schema(
        r#"{"name":"read","description":"Read","parameters":{"type":"object"}}"#,
    )
    .unwrap();
    assert_eq!(schema.name, "read");
    assert_eq!(schema.description, "Read");
    assert_eq!(schema.parameters, json!({"type": "object"}));
    assert!(
        parse_trajectory_tool_schema(r#"{"name":"read","description":"Read","parameters":[]}"#)
            .is_none()
    );
}

#[test]
fn request_wire_defaults_absent_purpose_to_assistant() {
    let request: TrajectoryRequestNumber = serde_json::from_value(json!({
        "group": "Step 1",
        "number": 3,
        "turn": 2,
        "step": 1
    }))
    .unwrap();
    assert_eq!(request.purpose, TrajectoryRequestPurpose::Assistant);
    assert_eq!(request.number, 3);
}
