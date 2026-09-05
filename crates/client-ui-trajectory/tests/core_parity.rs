//! Record identity, timeline, and virtual-ledger parity.

#![allow(clippy::float_cmp)] // Projections use exact integer-derived IEEE coordinates.

use std::collections::BTreeSet;

use seekdeep_client_ui_trajectory::{
    CollapsedSummaryKind, TrajectoryCell, TrajectoryCellKind, TrajectoryGroupModel,
    TrajectoryTimeRange, TrajectoryTimelineMode, TrajectoryTurnModel,
    VirtualizableTrajectoryRecord, derive_trajectory_timeline, format_duration_millis,
    format_elapsed_seconds, format_timeline_offset, group_trajectory_virtual_rows,
    trajectory_record_id, trajectory_timeline_focus_indexes, trajectory_virtual_record_key,
};

fn cell(index: usize, kind: TrajectoryCellKind, text: &str) -> TrajectoryCell {
    TrajectoryCell::new(index, kind, text)
}

fn timed_cell(
    index: usize,
    kind: TrajectoryCellKind,
    text: &str,
    started_at: f64,
    time_seconds: f64,
) -> TrajectoryCell {
    TrajectoryCell {
        started_at: Some(started_at),
        time_seconds: Some(time_seconds),
        ..cell(index, kind, text)
    }
}

fn turn(turn: Option<u64>, cells: Vec<TrajectoryCell>) -> TrajectoryTurnModel {
    TrajectoryTurnModel {
        turn,
        groups: vec![TrajectoryGroupModel {
            title: "Step 1".to_owned(),
            description: None,
            cells,
        }],
    }
}

#[test]
fn millisecond_formatting_uses_javascript_rounding_and_separators() {
    for (value, expected) in [
        (Some(0.0), "0 ms"),
        (Some(29.0), "29 ms"),
        (Some(500.0), "500 ms"),
        (Some(1_500.0), "1,500 ms"),
        (Some(235_200.0), "235,200 ms"),
        (Some(1_234_567.5), "1,234,568 ms"),
        (Some(-0.5), "0 ms"),
        (None, "—"),
        (Some(f64::NAN), "—"),
    ] {
        assert_eq!(format_duration_millis(value), expected);
    }
    assert_eq!(format_elapsed_seconds(Some(235.2)), "235,200 ms");
    assert_eq!(format_elapsed_seconds(Some(0.029)), "29 ms");
    assert_eq!(format_elapsed_seconds(None), "—");
    assert_eq!(format_timeline_offset(1_500.0), "1,500 ms");
}

#[test]
fn stable_record_identity_uses_explicit_call_seq_then_index_precedence() {
    let mut record = cell(6, TrajectoryCellKind::Tool, "bash");
    assert_eq!(trajectory_record_id(&record), concat!("tool\0index\0", "6"));
    record.source_seq = Some(9);
    assert_eq!(trajectory_record_id(&record), concat!("tool\0seq\0", "9"));
    record.call_id = Some("call-1".to_owned());
    assert_eq!(trajectory_record_id(&record), "tool\0call\0call-1");
    record.record_id = Some("stable".to_owned());
    assert_eq!(trajectory_record_id(&record), "stable");
}

#[test]
fn sequence_timeline_uses_equal_slots_semantic_lanes_and_turn_boundaries() {
    let turns = vec![turn(
        Some(1),
        vec![
            timed_cell(1, TrajectoryCellKind::Message, "assistant", 1_000.0, 1.0),
            timed_cell(2, TrajectoryCellKind::Tool, "bash", 2_000.0, 1.0),
            cell(3, TrajectoryCellKind::User, "unknown"),
        ],
    )];
    let model = derive_trajectory_timeline(&turns, TrajectoryTimelineMode::Sequence).unwrap();
    assert_eq!(model.start, 0.0);
    assert_eq!(model.end, 3.0);
    assert_eq!(
        model
            .spans
            .iter()
            .map(|span| (span.index, span.start, span.end, span.lane))
            .collect::<Vec<_>>(),
        vec![(1, 0.0, 1.0, 1), (2, 1.0, 2.0, 2), (3, 2.0, 3.0, 0)]
    );
    assert_eq!(model.turn_boundaries.len(), 1);
    assert_eq!(model.turn_boundaries[0].turn, 1);
    assert_eq!(model.turn_boundaries[0].time, 0.0);
}

#[test]
fn duration_compresses_idle_while_actual_retains_wall_time() {
    let turns = vec![
        turn(
            Some(1),
            vec![
                timed_cell(1, TrajectoryCellKind::Message, "first", 1_000.0, 1.0),
                timed_cell(2, TrajectoryCellKind::Tool, "gap", 4_000.0, 1.0),
            ],
        ),
        turn(
            Some(2),
            vec![timed_cell(
                3,
                TrajectoryCellKind::Message,
                "after idle",
                40_000.0,
                1.0,
            )],
        ),
    ];
    let duration = derive_trajectory_timeline(&turns, TrajectoryTimelineMode::Duration).unwrap();
    assert_eq!(duration.start, 1_000.0);
    assert_eq!(duration.end, 4_000.0);
    assert_eq!(
        duration
            .spans
            .iter()
            .map(|span| (span.index, span.start, span.end))
            .collect::<Vec<_>>(),
        vec![
            (1, 1_000.0, 2_000.0),
            (2, 2_000.0, 3_000.0),
            (3, 3_000.0, 4_000.0),
        ]
    );
    assert_eq!(duration.turn_boundaries[1].time, 3_000.0);

    let actual = derive_trajectory_timeline(&turns, TrajectoryTimelineMode::Actual).unwrap();
    assert_eq!(actual.start, 1_000.0);
    assert_eq!(actual.end, 41_000.0);
    assert_eq!(actual.spans[1].start, 4_000.0);
    assert_eq!(actual.spans[2].start, 40_000.0);

    let time = derive_trajectory_timeline(&turns, TrajectoryTimelineMode::Time).unwrap();
    assert_eq!(time.spans[0].start, time.spans[0].end);
    assert_eq!(time.end, 40_000.0);
}

#[test]
fn request_boundaries_missing_times_errors_and_inclusive_focus_follow_source_rules() {
    let mut request = cell(1, TrajectoryCellKind::System, "request");
    request.request_only = Some(true);
    let missing = cell(2, TrajectoryCellKind::Context, "untimed");
    let mut failed = timed_cell(3, TrajectoryCellKind::Subtool, "failed", 2_000.0, -1.0);
    failed.is_error = Some(true);
    let turns = vec![turn(None, vec![request, missing, failed])];

    let sequence = derive_trajectory_timeline(&turns, TrajectoryTimelineMode::Sequence).unwrap();
    assert_eq!(
        sequence
            .spans
            .iter()
            .map(|span| span.index)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert!(sequence.turn_boundaries.is_empty());
    assert!(sequence.spans[1].is_error);
    assert_eq!(sequence.spans[1].lane, 2);

    let timed = derive_trajectory_timeline(&turns, TrajectoryTimelineMode::Actual).unwrap();
    assert_eq!(timed.spans.len(), 1);
    assert_eq!(timed.spans[0].start, 2_000.0);
    assert_eq!(timed.spans[0].end, 2_000.0);
    assert_eq!(
        trajectory_timeline_focus_indexes(
            &turns,
            TrajectoryTimeRange {
                start: 0.0,
                end: 1.0,
            },
            TrajectoryTimelineMode::Sequence,
        ),
        BTreeSet::from([2, 3])
    );
    assert!(derive_trajectory_timeline(&[], TrajectoryTimelineMode::Sequence).is_none());
}

fn virtual_record(
    index: usize,
    request_only: bool,
    source_seq: u64,
    summary: Option<CollapsedSummaryKind>,
) -> VirtualizableTrajectoryRecord {
    let mut cell = cell(
        index,
        TrajectoryCellKind::Message,
        format!("record {index}").as_str(),
    );
    cell.request_only = request_only.then_some(true);
    cell.source_seq = Some(source_seq);
    VirtualizableTrajectoryRecord {
        cell,
        collapsed_summary_kind: summary,
    }
}

#[test]
fn virtual_rows_group_boundaries_preserve_keys_and_use_exact_heights() {
    let first = virtual_record(1, true, 10, None);
    let second = virtual_record(2, true, 11, None);
    let content = virtual_record(3, false, 12, None);
    let rows = group_trajectory_virtual_rows(&[first.clone(), second, content.clone()]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].entries.len(), 3);
    assert_eq!(rows[0].height, 30);
    assert_eq!(rows[0].key, trajectory_virtual_record_key(&content));

    let terminal =
        group_trajectory_virtual_rows(&[content.clone(), virtual_record(4, true, 13, None)]);
    assert_eq!(terminal.len(), 2);
    assert_eq!(terminal[1].height, 9);

    let summary = virtual_record(3, false, 12, Some(CollapsedSummaryKind::Turn));
    assert_eq!(
        group_trajectory_virtual_rows(std::slice::from_ref(&summary))[0].height,
        20
    );
    assert_ne!(
        trajectory_virtual_record_key(&summary),
        trajectory_virtual_record_key(&content)
    );
}

#[test]
fn virtual_keys_are_dom_safe_and_stable_across_prepend_and_join() {
    let mut punctuated = virtual_record(1, false, 1, None);
    punctuated.cell.source_seq = None;
    punctuated.cell.call_id = Some("call with spaces/and?punctuation".to_owned());
    assert_eq!(
        trajectory_virtual_record_key(&punctuated),
        "message%00call%00call%20with%20spaces%2Fand%3Fpunctuation"
    );

    let existing = virtual_record(2, false, 100, None);
    let before = group_trajectory_virtual_rows(std::slice::from_ref(&existing))[0]
        .key
        .clone();
    let after =
        group_trajectory_virtual_rows(&[virtual_record(1, false, 10, None), existing.clone()])[1]
            .key
            .clone();
    assert_eq!(before, after);
    assert_eq!(
        group_trajectory_virtual_rows(&[virtual_record(1, true, 99, None), existing.clone(),])[0]
            .key,
        group_trajectory_virtual_rows(&[existing])[0].key
    );
}
