//! Snapshot scheduling equivalence retains every event and causal constraint.

#[path = "support/smoke_snapshot.rs"]
mod smoke_snapshot;

use serde_json::{Value, json};
use smoke_snapshot::canonical_workflow_starts;

fn fixture() -> Value {
    json!({"events":[{"type":"tool-workflow/agent-start","seq":59}],"notifications":[
        {"method":"session.event","payload":{"sessionId":"parent","event":{"type":"tool-workflow/run-start"}}},
        {"method":"subagent.started","payload":{"parentSessionId":"parent","childSessionId":"child"}},
        {"method":"session.event","payload":{"sessionId":"child","event":{"type":"request/context","seq":0}}},
        {"method":"session.event","payload":{"sessionId":"parent","event":{"type":"tool-workflow/agent-start","seq":59,"data":{"childId":"child"}}}},
        {"method":"session.event","payload":{"sessionId":"child","event":{"type":"assistant/message","seq":1,"data":{"text":"answer"}}}},
        {"method":"subagent.finished","payload":{"parentSessionId":"parent","childSessionId":"child"}},
        {"method":"session.event","payload":{"sessionId":"parent","event":{"type":"tool-workflow/agent-end","seq":60}}}
    ]})
}

#[test]
fn admits_start_before_request_or_after_completion_without_discarding_data() {
    let expected = canonical_workflow_starts(fixture()).unwrap();
    for position in 2..=5 {
        let mut actual = fixture();
        let notifications = actual["notifications"].as_array_mut().unwrap();
        let marker = notifications.remove(3);
        notifications.insert(position, marker);
        assert_eq!(canonical_workflow_starts(actual).unwrap(), expected);
    }
}

#[test]
fn rejects_start_before_publication_or_after_parent_progress() {
    for position in [0, 1, 6] {
        let mut actual = fixture();
        let notifications = actual["notifications"].as_array_mut().unwrap();
        let marker = notifications.remove(3);
        notifications.insert(position, marker);
        assert!(canonical_workflow_starts(actual).is_err());
    }
}

#[test]
fn retains_payload_sequence_count_and_child_order_differences() {
    let expected = canonical_workflow_starts(fixture()).unwrap();
    let mut changed = fixture();
    changed["notifications"][4]["payload"]["event"]["data"]["text"] = json!("wrong");
    assert_ne!(canonical_workflow_starts(changed).unwrap(), expected);
    let mut reversed = fixture();
    reversed["notifications"].as_array_mut().unwrap().swap(2, 4);
    assert_ne!(canonical_workflow_starts(reversed).unwrap(), expected);
    let mut missing = fixture();
    missing["notifications"].as_array_mut().unwrap().remove(2);
    assert_ne!(canonical_workflow_starts(missing).unwrap(), expected);
    let mut duplicate = fixture();
    let marker = duplicate["notifications"][3].clone();
    duplicate["notifications"]
        .as_array_mut()
        .unwrap()
        .insert(4, marker);
    assert!(canonical_workflow_starts(duplicate).is_err());
    let mut sibling = fixture();
    sibling["notifications"][2]["payload"]["sessionId"] = json!("sibling");
    assert!(canonical_workflow_starts(sibling).is_err());
}
