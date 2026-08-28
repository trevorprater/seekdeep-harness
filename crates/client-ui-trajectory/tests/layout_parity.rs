//! Trajectory Turn/group/cell fold and partial-append parity.

use std::rc::Rc;

use seekdeep_client_ui_trajectory::{
    TrajectoryCell, TrajectoryCellKind, TrajectoryLocation, TrajectorySequence, TrajectorySnapshot,
    TrajectoryStepLocation, TrajectoryTurnLocation, append_trajectory_partial_layout,
    derive_trajectory_layout,
};
use serde_json::{Value, json};

fn snapshot(nodes: Vec<Value>) -> TrajectorySnapshot {
    TrajectorySnapshot {
        event_nodes: nodes,
        ..TrajectorySnapshot::default()
    }
}

fn cells(turns: &[seekdeep_client_ui_trajectory::TrajectoryTurnModel]) -> Vec<&TrajectoryCell> {
    turns
        .iter()
        .flat_map(|turn| &turn.groups)
        .flat_map(|group| &group.cells)
        .collect()
}

#[test]
fn assistant_blocks_usage_and_result_pair_fold_into_message_and_tool() {
    let nodes = vec![
        json!({"kind": "user", "seq": 1, "time": 1_000, "content": [{"type": "text", "text": "hello"}], "source": null}),
        json!({
            "kind": "assistant", "seq": 2, "time": 6_000, "turn": 1, "step": 1,
            "blocks": [
                {"kind": "reasoning", "text": "thinking…"},
                {"kind": "text", "text": "I will run bash"},
                {"kind": "tool-call", "callId": "c1", "name": "bash", "argsRaw": "{\"command\":\"ls\"}"},
            ],
            "usage": {"inputTokens": 10, "outputTokens": 20, "reasoningTokens": 5},
        }),
        json!({
            "kind": "tool-result", "seq": 3, "time": 7_500, "callId": "c1",
            "call": {"name": "bash", "argsRaw": "{\"command\":\"ls\"}"}, "callTime": 6_200,
            "content": [{"type": "text", "text": "a.txt"}], "isError": false,
            "callView": null, "resultView": null, "subCalls": [],
        }),
    ];
    let turns = derive_trajectory_layout(&snapshot(nodes));
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].turn, Some(1));
    assert_eq!(
        cells(&turns)
            .iter()
            .map(|cell| cell.kind)
            .collect::<Vec<_>>(),
        [
            TrajectoryCellKind::User,
            TrajectoryCellKind::Message,
            TrajectoryCellKind::Tool,
        ]
    );
    let message = cells(&turns)
        .into_iter()
        .find(|cell| cell.kind == TrajectoryCellKind::Message)
        .unwrap();
    assert_eq!(message.input, Some(10));
    assert_eq!(message.output, Some(20));
    assert_eq!(message.think, Some(5));
    assert_eq!(message.time_seconds, Some(5.0));
    let tool = cells(&turns)
        .into_iter()
        .find(|cell| cell.kind == TrajectoryCellKind::Tool)
        .unwrap();
    assert_eq!(tool.text, "bash");
    assert_eq!(
        tool.preview_markdown.as_deref(),
        Some("{\"command\":\"ls\"}")
    );
    assert_eq!(tool.time_seconds, Some(1.3));
}

#[test]
fn running_calls_and_missing_times_keep_blank_durations() {
    let mut input = TrajectorySnapshot::default();
    input.running_calls.push(json!({
        "callId": "r1", "name": "bash", "argsRaw": "{\"command\":\"pwd\"}",
        "turn": 1, "step": 2, "time": 9_000, "callView": null, "subCalls": [],
    }));
    let turns = derive_trajectory_layout(&input);
    assert_eq!(turns[0].groups[0].title, "Step 2");
    let tool = &turns[0].groups[0].cells[0];
    assert_eq!(tool.text, "bash");
    assert_eq!(tool.time_seconds, None);

    let missing = snapshot(vec![
        json!({"kind": "user", "seq": 1, "content": [{"type": "text", "text": "hi"}], "source": null}),
        json!({"kind": "assistant", "seq": 2, "turn": 1, "step": 1,
            "blocks": [{"kind": "text", "text": "ok"}],
            "usage": {"inputTokens": 1, "outputTokens": 2}}),
    ]);
    let turns = derive_trajectory_layout(&missing);
    let message = cells(&turns)
        .into_iter()
        .find(|cell| cell.kind == TrajectoryCellKind::Message)
        .unwrap();
    assert_eq!(message.time_seconds, None);
    assert_eq!(
        turns[0]
            .groups
            .iter()
            .find(|group| group.title == "Step 1")
            .unwrap()
            .description,
        None
    );
}

#[test]
fn partial_append_shares_unaffected_turn_and_replaces_running_call_placeholder() {
    let mut base_input = snapshot(vec![json!({
        "kind": "assistant", "seq": 2, "time": 2_000, "turn": 1, "step": 1,
        "blocks": [{"kind": "text", "text": "finalized"}],
    })]);
    base_input.partial = Some(json!({"turn": 2, "step": 1, "blocks": []}));
    base_input.requests.push(json!({
        "purpose": "assistant", "startSeq": 3, "turn": 2, "step": 1,
        "startedAt": 3_000, "completedAt": null, "status": "running",
    }));
    let base = derive_trajectory_layout(&base_input)
        .into_iter()
        .map(Rc::new)
        .collect::<Vec<_>>();
    let streamed = append_trajectory_partial_layout(
        &base,
        Some(&json!({
            "turn": 2, "step": 1,
            "blocks": [{"kind": "reasoning", "text": "streaming"}],
        })),
        1,
    );
    assert!(Rc::ptr_eq(&streamed[0], &base[0]));
    assert_eq!(streamed.len(), 2);
    assert_eq!(streamed[1].groups[0].cells[0].index, 2);
    assert_eq!(
        streamed[1].groups[0].cells[0].preview_markdown.as_deref(),
        Some("streaming")
    );

    let mut placeholder_input = TrajectorySnapshot {
        partial: Some(json!({"turn": 1, "step": 1, "blocks": []})),
        ..TrajectorySnapshot::default()
    };
    placeholder_input.running_calls.push(json!({
        "callId": "c1", "name": "bash", "argsRaw": "{\"command\":\"pwd\"}",
        "turn": 1, "step": 1, "time": 9_000, "callView": null, "subCalls": [],
    }));
    let base = derive_trajectory_layout(&placeholder_input)
        .into_iter()
        .map(Rc::new)
        .collect::<Vec<_>>();
    let streamed = append_trajectory_partial_layout(
        &base,
        Some(&json!({"turn": 1, "step": 1, "blocks": [{
            "kind": "tool-call", "callId": "c1", "name": "bash",
            "argsRaw": "{\"command\":\"pwd\"}",
        }]})),
        1,
    );
    let cells = &streamed[0].groups[0].cells;
    assert_eq!(
        cells.iter().map(|cell| cell.kind).collect::<Vec<_>>(),
        [TrajectoryCellKind::Message, TrajectoryCellKind::Tool]
    );
    assert_eq!(
        cells
            .iter()
            .filter(|cell| cell.call_id.as_deref() == Some("c1"))
            .count(),
        1
    );
}

#[test]
fn group_wall_span_histogram_user_turns_and_recorded_step_start_match_source() {
    let nodes = vec![
        json!({"kind": "user", "seq": 1, "time": 1_000, "content": [{"type": "text", "text": "first"}], "source": null}),
        json!({"kind": "assistant", "seq": 2, "time": 1_000, "turn": 1, "step": 1,
        "blocks": [
            {"kind": "tool-call", "callId": "a", "name": "bash", "argsRaw": "{}"},
            {"kind": "tool-call", "callId": "b", "name": "bash", "argsRaw": "{}"}
        ]}),
        json!({"kind": "tool-result", "seq": 3, "time": 2_500, "callId": "a",
            "call": {"name": "bash", "argsRaw": "{}"}, "callTime": 1_100,
            "content": [], "isError": false, "subCalls": []}),
        json!({"kind": "tool-result", "seq": 4, "time": 4_000, "callId": "b",
            "call": {"name": "bash", "argsRaw": "{}"}, "callTime": 2_600,
            "content": [], "isError": false, "subCalls": []}),
        json!({"kind": "user", "seq": 5, "time": 5_000, "content": [{"type": "text", "text": "second"}], "source": null}),
        json!({"kind": "assistant", "seq": 6, "time": 8_000, "turn": 2, "step": 0,
            "blocks": [{"kind": "text", "text": "ok2"}],
            "timing": {"stepStartTime": 7_000, "firstTokenTime": 7_500, "completedTime": 8_000}}),
    ];
    let turns = derive_trajectory_layout(&snapshot(nodes));
    assert_eq!(
        turns.iter().map(|turn| turn.turn).collect::<Vec<_>>(),
        [Some(1), Some(2)]
    );
    assert_eq!(
        turns[0]
            .groups
            .iter()
            .find(|group| group.title == "Step 1")
            .unwrap()
            .description
            .as_deref(),
        Some("3,000 ms bash×2")
    );
    let second_message = cells(&turns)
        .into_iter()
        .find(|cell| cell.preview_markdown.as_deref() == Some("ok2"))
        .unwrap();
    assert_eq!(second_message.started_at, Some(7_000.0));
    assert_eq!(second_message.time_seconds, Some(1.0));
}

#[test]
fn steering_uses_resolved_or_following_step_and_running_boundary_stays_after_input() {
    let nodes = vec![
        json!({"kind": "user", "seq": 1, "time": 1_000, "content": [{"type": "text", "text": "start"}], "source": null}),
        json!({"kind": "assistant", "seq": 2, "time": 2_000, "turn": 1, "step": 1,
            "blocks": [{"kind": "text", "text": "first step"}]}),
        json!({"kind": "steering", "messageId": "steer-1", "seq": 3, "time": 3_000,
            "content": [{"type": "text", "text": "change direction"}], "source": null}),
        json!({"kind": "assistant", "seq": 4, "time": 4_000, "turn": 1, "step": 2,
            "blocks": [{"kind": "text", "text": "second step"}]}),
    ];
    let mut input = snapshot(nodes);
    input.event_locations.insert(
        TrajectorySequence::new(3.0),
        TrajectoryLocation::Step {
            turn: TrajectoryTurnLocation { turn: 1 },
            step: TrajectoryStepLocation { step: 2 },
        },
    );
    let turns = derive_trajectory_layout(&input);
    assert_eq!(
        turns[0]
            .groups
            .iter()
            .map(|group| group.title.as_str())
            .collect::<Vec<_>>(),
        ["Message", "Step 1", "Step 2"]
    );
    assert_eq!(turns[0].groups[2].cells[0].source_seq, Some(3));

    let mut boundary = snapshot(vec![json!({
        "kind": "steering", "messageId": "steer-1", "seq": 3, "time": 3_000,
        "content": [{"type": "text", "text": "change direction"}], "source": null,
    })]);
    boundary.event_locations = input.event_locations;
    boundary.requests.push(json!({
        "purpose": "assistant", "startSeq": 2, "turn": 1, "step": 2,
        "startedAt": 2_000, "completedAt": null, "status": "running",
    }));
    let turns = derive_trajectory_layout(&boundary);
    assert_eq!(turns[0].groups[0].cells[0].source_seq, Some(3));
    assert_eq!(turns[0].groups[0].cells[1].request_only, Some(true));

    let historical = snapshot(vec![
        json!({"kind": "steering", "messageId": "steer", "seq": 3, "time": 3_000,
            "content": [{"type": "text", "text": "change"}], "source": null}),
        json!({"kind": "assistant", "seq": 4, "time": 4_000, "turn": 2, "step": 3,
            "blocks": [{"kind": "text", "text": "continued"}]}),
    ]);
    let turns = derive_trajectory_layout(&historical);
    assert_eq!(turns[0].turn, Some(2));
    assert_eq!(turns[0].groups[0].title, "Step 3");
}

#[test]
fn standalone_compaction_and_context_cursor_keep_chronology_without_duplicate_marker() {
    let nodes = vec![
        json!({"kind": "user", "seq": 1, "time": 1_000, "content": [{"type": "text", "text": "first"}], "source": null}),
        json!({"kind": "assistant", "seq": 2, "time": 2_000, "turn": 1, "step": 1,
            "blocks": [{"kind": "text", "text": "before"}]}),
        json!({"kind": "context", "seq": 4, "time": 9_000,
            "content": [{"type": "text", "text": "extra"}], "source": null}),
        json!({"kind": "compaction", "seq": 5, "time": 9_500}),
        json!({"kind": "assistant", "seq": 6, "time": 10_000, "turn": 2, "step": 0,
            "blocks": [{"kind": "text", "text": "done"}]}),
    ];
    let mut input = snapshot(nodes);
    input.requests.push(json!({
        "purpose": "compaction", "startSeq": 3, "turn": null, "step": 0,
        "startedAt": 3_000, "completedAt": 4_000, "status": "complete",
        "summary": [{"type": "text", "text": "standalone summary"}],
    }));
    let turns = derive_trajectory_layout(&input);
    assert_eq!(
        turns.iter().map(|turn| turn.turn).collect::<Vec<_>>(),
        [Some(1), None, Some(2)]
    );
    assert_eq!(turns[1].groups[0].title, "Compaction 3");
    assert_eq!(
        turns[1].groups[0].cells[0].preview_markdown.as_deref(),
        Some("standalone summary")
    );
    assert_eq!(
        cells(&turns)
            .iter()
            .filter(|cell| cell.kind == TrajectoryCellKind::Compacted)
            .count(),
        1
    );
    let done = cells(&turns)
        .into_iter()
        .find(|cell| cell.preview_markdown.as_deref() == Some("done"))
        .unwrap();
    assert_eq!(done.time_seconds, Some(0.5));
}

#[test]
fn nested_settled_and_running_subcalls_flatten_immediately_after_parent() {
    let leaf = json!({"kind": "tool-result", "seq": 102, "time": 7_800,
        "callId": "p1:code:1:code:1", "call": {"name": "read", "argsRaw": "{\"x\":1}"},
        "callTime": 7_300, "content": [{"type": "text", "text": "ok"}],
        "isError": false, "subCalls": []});
    let child = json!({"kind": "tool-result", "seq": 101, "time": 8_000,
        "callId": "p1:code:1", "call": {"name": "run_code", "argsRaw": "{\"x\":1}"},
        "callTime": 6_300, "content": [{"type": "text", "text": "ok"}],
        "isError": false, "subCalls": [leaf]});
    let nodes = vec![
        json!({"kind": "assistant", "seq": 2, "time": 6_000, "turn": 1, "step": 1,
            "blocks": [{"kind": "tool-call", "callId": "p1", "name": "run_code", "argsRaw": "{}"}]}),
        json!({"kind": "tool-result", "seq": 3, "time": 9_000, "callId": "p1",
            "call": {"name": "run_code", "argsRaw": "{}"}, "callTime": 6_200,
            "content": [{"type": "text", "text": "done"}], "isError": false,
            "subCalls": [child]}),
    ];
    let turns = derive_trajectory_layout(&snapshot(nodes));
    let cells = cells(&turns);
    assert_eq!(
        cells.iter().map(|cell| cell.kind).collect::<Vec<_>>(),
        [
            TrajectoryCellKind::Message,
            TrajectoryCellKind::Tool,
            TrajectoryCellKind::Subtool,
            TrajectoryCellKind::Subtool,
        ]
    );
    assert_eq!(
        cells.iter().map(|cell| cell.index).collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    assert_eq!(cells[2].call_id.as_deref(), Some("p1:code:1"));
    assert_eq!(cells[3].call_id.as_deref(), Some("p1:code:1:code:1"));
    assert_eq!(cells[3].time_seconds, Some(0.5));
}
