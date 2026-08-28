//! Typed trajectory snapshot replace/apply, inheritance, and interruption parity.

use std::rc::Rc;

use seekdeep_client_runtime::{
    ConversationLocation, ConversationViewNode, ConversationViewPlacement,
};
use seekdeep_client_ui_trajectory::{TrajectorySequence, TrajectorySnapshotBuilder};
use serde_json::{Value, json};

fn contribution(key: &str, anchor_seq: f64, data: Value) -> Rc<ConversationViewNode> {
    Rc::new(ConversationViewNode {
        key: key.to_owned(),
        kind: key.to_owned(),
        id: key.to_owned(),
        target: "trajectory".to_owned(),
        data: Rc::new(data),
        placement: Some(ConversationViewPlacement {
            anchor_seq,
            location: ConversationLocation::Session,
        }),
        chat: None,
    })
}

fn assistant_request(start_seq: u64, step: u64) -> Value {
    json!({
        "purpose": "assistant",
        "startSeq": start_seq,
        "turn": 1,
        "step": step,
        "startedAt": start_seq,
        "completedAt": start_seq + 1,
        "status": "complete",
    })
}

fn compaction_request(start_seq: u64) -> Value {
    json!({
        "purpose": "compaction",
        "startSeq": start_seq,
        "turn": null,
        "step": 0,
        "startedAt": start_seq,
        "completedAt": null,
        "status": "running",
    })
}

fn header(
    key: &str,
    anchor_seq: u64,
    system: &str,
    tools: &Value,
    location: &Value,
    change: &Value,
) -> Rc<ConversationViewNode> {
    contribution(
        key,
        f64::from(u32::try_from(anchor_seq).unwrap()),
        json!({
            "kind": "request-header",
            "header": {
                "seq": anchor_seq,
                "time": anchor_seq,
                "prompt": {
                    "config": {"provider": "test", "model": system},
                    "system": system,
                    "tools": tools,
                },
                "change": change,
                "location": location,
            },
        }),
    )
}

#[test]
fn one_header_is_inherited_without_repeating_its_prompt_change() {
    let nodes = vec![
        header(
            "header",
            2,
            "one initial prompt",
            &json!([]),
            &json!({"kind": "session"}),
            &json!({"seq": 2, "time": 2, "kind": "initial"}),
        ),
        contribution(
            "assistant:1",
            3.0,
            json!({"kind": "assistant", "partial": null, "request": assistant_request(3, 1)}),
        ),
        contribution(
            "assistant:2",
            5.0,
            json!({"kind": "assistant", "partial": null, "request": assistant_request(5, 2)}),
        ),
    ];
    let snapshot = TrajectorySnapshotBuilder::new().replace_typed(&nodes);
    assert_eq!(
        snapshot.requests[0]["prompt"]["system"],
        "one initial prompt"
    );
    assert_eq!(
        snapshot.requests[1]["prompt"]["system"],
        "one initial prompt"
    );
    assert_eq!(snapshot.requests[0]["promptChange"]["kind"], "initial");
    assert!(snapshot.requests[1].get("promptChange").is_none());
}

#[test]
fn exact_step_header_and_active_tool_catalog_win_without_backward_scans() {
    let base_tool =
        json!({"name": "read", "description": "Read", "parameters": {"type": "object"}});
    let exact_tool =
        json!({"name": "edit", "description": "Edit", "parameters": {"type": "object"}});
    let nodes = vec![
        header(
            "header:base",
            2,
            "base prompt",
            &json!([base_tool]),
            &json!({"kind": "session"}),
            &json!({"seq": 2, "time": 2, "kind": "initial"}),
        ),
        contribution(
            "assistant:1",
            3.0,
            json!({"kind": "assistant", "partial": null, "request": assistant_request(3, 1)}),
        ),
        contribution(
            "assistant:2",
            5.0,
            json!({"kind": "assistant", "partial": null, "request": assistant_request(5, 2)}),
        ),
        header(
            "header:exact",
            6,
            "exact prompt",
            &json!([exact_tool.clone()]),
            &json!({"kind": "step", "turn": {"turn": 1}, "step": {"step": 2}}),
            &json!({
                "seq": 6, "time": 6, "kind": "system",
                "previous": {
                    "config": {"provider": "test", "model": "base prompt"},
                    "system": "base prompt", "tools": [],
                },
            }),
        ),
        contribution(
            "tool",
            7.0,
            json!({
                "kind": "tool",
                "root": {
                    "callId": "call-edit", "name": "edit", "argsRaw": "{}",
                    "turn": 1, "step": 2, "time": 7, "callView": null,
                    "subCalls": [],
                },
            }),
        ),
    ];
    let snapshot = TrajectorySnapshotBuilder::new().replace_typed(&nodes);
    assert_eq!(snapshot.requests[0]["prompt"]["system"], "base prompt");
    assert_eq!(snapshot.requests[1]["prompt"]["system"], "exact prompt");
    assert_eq!(snapshot.call_schemas.get("call-edit"), Some(&exact_tool));
    assert_eq!(snapshot.running_calls.len(), 1);
}

#[test]
fn session_boundaries_and_turn_errors_use_linear_request_indexes() {
    let nodes = vec![
        contribution(
            "assistant:1",
            1.0,
            json!({"kind": "assistant", "partial": null, "request": assistant_request(1, 1)}),
        ),
        contribution(
            "assistant:2",
            3.0,
            json!({"kind": "assistant", "partial": null, "request": assistant_request(3, 2)}),
        ),
        contribution(
            "turn-end",
            5.0,
            json!({"kind": "turn-end", "turn": 1, "time": 5, "error": "turn failed"}),
        ),
        contribution(
            "compact:10",
            10.0,
            json!({"kind": "compaction", "request": compaction_request(10)}),
        ),
        contribution(
            "compact:12",
            12.0,
            json!({"kind": "compaction", "request": compaction_request(12)}),
        ),
        contribution(
            "session-end:14",
            14.0,
            json!({"kind": "session-end", "seq": 14, "time": 14}),
        ),
        contribution(
            "session-end:16",
            16.0,
            json!({"kind": "session-end", "seq": 16, "time": 16}),
        ),
    ];
    let snapshot = TrajectorySnapshotBuilder::new().replace_typed(&nodes);
    assert_eq!(snapshot.requests[0]["status"], "complete");
    assert_eq!(snapshot.requests[1]["status"], "error");
    assert_eq!(snapshot.requests[1]["error"], "turn failed");
    assert_eq!(snapshot.requests[2]["completedAt"], 16);
    assert_eq!(snapshot.requests[3]["completedAt"], 14);
    assert_eq!(
        snapshot.requests[2]["error"],
        "Compaction was interrupted before completion."
    );
}

#[test]
fn content_updates_keep_order_while_structural_insert_rebuilds_positions() {
    let first = contribution(
        "assistant:1",
        1.0,
        json!({"kind": "assistant", "partial": null, "request": assistant_request(1, 1)}),
    );
    let last = contribution(
        "assistant:3",
        5.0,
        json!({"kind": "assistant", "partial": null, "request": assistant_request(5, 3)}),
    );
    let mut builder = TrajectorySnapshotBuilder::new();
    assert_eq!(
        builder
            .replace_typed(&[last, first])
            .requests
            .iter()
            .map(|request| request["startSeq"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [1, 5]
    );
    let updated_last = contribution(
        "assistant:3",
        5.0,
        json!({
            "kind": "assistant", "partial": null,
            "request": {"purpose": "assistant", "startSeq": 5, "turn": 1, "step": 3,
                "startedAt": 5, "completedAt": 6, "status": "error", "error": "failed"},
        }),
    );
    assert_eq!(
        builder.apply_typed(&[updated_last]).requests[1]["error"],
        "failed"
    );
    let middle = contribution(
        "assistant:2",
        3.0,
        json!({"kind": "assistant", "partial": null, "request": assistant_request(3, 2)}),
    );
    assert_eq!(
        builder
            .apply_typed(&[middle])
            .requests
            .iter()
            .map(|request| request["startSeq"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [1, 3, 5]
    );
}

#[test]
fn finalized_nodes_locations_partials_and_running_calls_keep_distinct_axes() {
    let nodes = vec![
        contribution(
            "input",
            2.0,
            json!({"kind": "node", "node": {"kind": "user", "seq": 2, "time": 2}}),
        ),
        contribution(
            "assistant",
            3.0,
            json!({
                "kind": "assistant",
                "node": {"kind": "assistant", "seq": 3.5, "time": 4},
                "partial": {"turn": 1, "step": 2, "blocks": []},
            }),
        ),
        contribution(
            "tool",
            4.0,
            json!({"kind": "tool", "root": {
                "callId": "running", "name": "bash", "argsRaw": "{}", "subCalls": [],
            }}),
        ),
    ];
    let snapshot = TrajectorySnapshotBuilder::new().replace_typed(&nodes);
    assert_eq!(
        snapshot
            .event_nodes
            .iter()
            .map(|node| node["seq"].as_f64().unwrap())
            .collect::<Vec<_>>(),
        [2.0, 3.5]
    );
    assert!(
        snapshot
            .event_locations
            .contains_key(&TrajectorySequence::new(2.0))
    );
    assert_eq!(snapshot.event_locations.len(), 1);
    assert_eq!(snapshot.partial.as_ref().unwrap()["step"], 2);
    assert_eq!(snapshot.running_calls[0]["callId"], "running");
}
