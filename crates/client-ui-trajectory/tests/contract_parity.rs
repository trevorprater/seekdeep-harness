//! Closed contribution union round-trip parity.

use seekdeep_client_runtime::ConversationPromptSnapshot;
use seekdeep_client_ui_trajectory::{
    TrajectoryContribution, TrajectoryLocation, TrajectoryRequestHeaderState,
};
use serde_json::{Value, json};

fn header() -> TrajectoryRequestHeaderState {
    TrajectoryRequestHeaderState {
        seq: 2,
        time: 3,
        prompt: serde_json::from_value::<ConversationPromptSnapshot>(json!({
            "config": {"provider": "test", "model": "model"},
            "system": "prompt",
            "tools": [],
        }))
        .unwrap(),
        change: None,
        location: TrajectoryLocation::Session,
    }
}

#[test]
fn every_contribution_variant_round_trips_exact_optional_members() {
    let values = [
        json!({"kind": "node", "node": {"kind": "user", "seq": 1}}),
        json!({"kind": "assistant", "partial": null}),
        json!({"kind": "assistant", "node": {"kind": "assistant"}, "partial": {"blocks": []}, "request": {"purpose": "assistant"}}),
        json!({"kind": "tool", "root": {"callId": "call"}}),
        TrajectoryContribution::RequestHeader { header: header() }.to_value(),
        json!({"kind": "compaction", "request": {"purpose": "compaction"}}),
        json!({"kind": "session-end", "seq": 8, "time": 9}),
        json!({"kind": "turn-end", "turn": 2, "time": 10}),
        json!({"kind": "turn-end", "turn": 3, "time": 11, "error": "failed"}),
    ];
    for value in values {
        assert_eq!(
            TrajectoryContribution::from_value(&value)
                .unwrap()
                .to_value(),
            value
        );
    }
}

#[test]
fn malformed_and_unknown_contributions_fail_at_the_contract_boundary() {
    for value in [
        Value::Null,
        json!({}),
        json!({"kind": "future"}),
        json!({"kind": "node"}),
        json!({"kind": "session-end", "seq": "8", "time": 9}),
        json!({"kind": "turn-end", "turn": 1, "time": 2, "error": 3}),
    ] {
        assert!(TrajectoryContribution::from_value(&value).is_err());
    }
}
