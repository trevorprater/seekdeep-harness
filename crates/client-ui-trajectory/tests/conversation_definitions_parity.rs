//! Complete native trajectory Definition registry and snapshot-builder integration parity.

use std::rc::Rc;

use seekdeep_client_runtime::{
    AssemblerEventDefinitions, AssemblerNodeDefinition, AssemblerViewDefinition,
    AssemblerViewDefinitions, ConversationEventInput, ConversationLocationEvent,
    ConversationNodeAssembler,
};
use seekdeep_client_ui_trajectory::{trajectory_event_definitions, trajectory_view_definition};
use serde_json::{Value, json};

struct Events(Vec<Rc<AssemblerNodeDefinition>>);

impl AssemblerEventDefinitions for Events {
    fn entries(&self) -> Vec<Rc<AssemblerNodeDefinition>> {
        self.0.clone()
    }

    fn fallback_entry(&self) -> Option<Rc<AssemblerNodeDefinition>> {
        None
    }
}

struct Views;

impl AssemblerViewDefinitions for Views {
    fn entries(&self) -> Vec<Rc<AssemblerViewDefinition>> {
        vec![Rc::new(trajectory_view_definition())]
    }
}

fn at(seq: u64, event_type: &str, data: Value) -> ConversationEventInput {
    ConversationEventInput {
        event: ConversationLocationEvent::with_time(
            seq,
            1_700_000_000_000_i64 + i64::try_from(seq).unwrap(),
            event_type,
            data,
        ),
        view: None,
    }
}

fn assembler(events: &[ConversationEventInput]) -> ConversationNodeAssembler {
    let mut value = ConversationNodeAssembler::new(
        Rc::new(Events(
            trajectory_event_definitions()
                .into_iter()
                .map(Rc::new)
                .collect(),
        )),
        Rc::new(Views),
    );
    value.replace_window(events, false).unwrap();
    value.flush().unwrap();
    value
}

fn snapshot(value: &ConversationNodeAssembler) -> Rc<Value> {
    value.snapshot("trajectory").expect("trajectory snapshot")
}

fn assistant_message(id: &str, text: &str) -> Value {
    json!({
        "id": id,
        "content": [{"type": "text", "text": text}],
        "source": {"kind": "model", "provider": "test", "model": "test"},
    })
}

#[test]
fn combined_registry_assembles_streaming_retry_usage_and_interruption() {
    let mut value = assembler(&[
        at(1, "turn/start", json!({"turn": 1})),
        at(2, "step/start", json!({"turn": 1, "step": 1})),
        at(
            3,
            "assistant/chunk",
            json!({
                "turn": 1, "step": 1,
                "chunk": {"type": "text-delta", "index": 0, "text": "first attempt"},
            }),
        ),
        at(
            4,
            "assistant/chunk",
            json!({
                "turn": 1, "step": 1,
                "chunk": {"type": "usage", "usage": {"inputTokens": 10, "outputTokens": 3}},
            }),
        ),
    ]);
    let current = snapshot(&value);
    assert_eq!(
        current["partial"]["blocks"],
        json!([{"kind": "text", "text": "first attempt"}])
    );
    assert_eq!(current["requests"][0]["status"], "running");
    assert_eq!(
        current["requests"][0]["usage"],
        json!({"inputTokens": 10, "outputTokens": 3})
    );

    for event in [
        at(
            5,
            "llm/retry",
            json!({
                "turn": 1, "step": 1, "mode": "normal", "retry": 1,
                "maxRetries": 2, "delayMs": 25,
                "failure": {"code": "TRANSPORT", "message": "temporary failure"},
            }),
        ),
        at(
            6,
            "assistant/chunk",
            json!({
                "turn": 1, "step": 1,
                "chunk": {"type": "text-delta", "index": 0, "text": "second attempt"},
            }),
        ),
        at(7, "step/end", json!({"turn": 1, "step": 1})),
    ] {
        value.append(&event).unwrap();
    }
    value.flush().unwrap();
    let settled = snapshot(&value);
    assert_eq!(settled["partial"], Value::Null);
    assert_eq!(settled["eventNodes"][0]["interrupted"], true);
    assert_eq!(
        settled["eventNodes"][0]["blocks"],
        json!([{"kind": "text", "text": "second attempt"}])
    );
    assert_eq!(settled["requests"][0]["status"], "error");
    assert_eq!(settled["requests"][0]["retry"], 1);
    assert_eq!(
        settled["requests"][0]["usage"],
        json!({"inputTokens": 10, "outputTokens": 3})
    );
}

#[test]
fn combined_registry_keeps_parallel_roots_and_nested_dispatch_results() {
    let current = snapshot(&assembler(&[
        at(1, "turn/start", json!({"turn": 1})),
        at(2, "step/start", json!({"turn": 1, "step": 1})),
        at(
            3,
            "tool/call",
            json!({
                "turn": 1, "step": 1, "callId": "root-a", "name": "code", "arguments": "{}",
            }),
        ),
        at(
            4,
            "tool/call",
            json!({
                "turn": 1, "step": 1, "callId": "root-b", "name": "parallel", "arguments": "{}",
            }),
        ),
        at(
            5,
            "tool/code-dispatch-start",
            json!({
                "rootCallId": "root-a", "parentCallId": "root-a", "subCallId": "child",
                "name": "read", "arguments": {"path": "README.md"},
            }),
        ),
        at(
            6,
            "tool/code-dispatch",
            json!({
                "rootCallId": "root-a", "parentCallId": "root-a", "subCallId": "child",
                "name": "read", "arguments": {"path": "README.md"},
                "content": [{"type": "text", "text": "contents"}],
            }),
        ),
        at(7, "step/end", json!({"turn": 1, "step": 1})),
    ]));
    let mut tools = current["eventNodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|node| node["kind"] == "tool-result")
        .collect::<Vec<_>>();
    tools.sort_by_key(|node| node["callId"].as_str().unwrap());
    assert_eq!(
        tools
            .iter()
            .map(|node| node["callId"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["root-a", "root-b"]
    );
    assert_eq!(tools[0]["subCalls"][0]["callId"], "child");
    assert_eq!(tools[0]["subCalls"][0]["call"]["name"], "read");
}

#[test]
fn combined_registry_assembles_compaction_checkpoint_and_orphan_interruption() {
    let current = snapshot(&assembler(&[
        at(
            1,
            "compaction/start",
            json!({"compactionId": "complete", "turn": null}),
        ),
        at(
            2,
            "compaction/summary",
            json!({
                "compactionId": "complete", "turn": null, "summary": "summary",
                "provider": "test", "model": "test", "maxTokens": 100,
                "usage": {"inputTokens": 20, "outputTokens": 5},
            }),
        ),
        at(
            3,
            "user/message",
            json!({
                "id": "checkpoint", "content": [{"type": "text", "text": "summary checkpoint"}],
                "source": {"kind": "plugin", "plugin": "compact", "compactionId": "complete"},
            }),
        ),
        at(
            4,
            "compaction/end",
            json!({"compactionId": "complete", "turn": null}),
        ),
        at(
            5,
            "compaction/start",
            json!({"compactionId": "orphan", "turn": null}),
        ),
        at(6, "session/end-seed", json!({})),
    ]));
    assert_eq!(current["requests"][0]["status"], "complete");
    assert_eq!(current["requests"][0]["resultSeq"], 2);
    assert_eq!(current["requests"][0]["replacementSeq"], 3);
    assert_eq!(current["requests"][0]["summary"], "summary");
    assert_eq!(current["requests"][1]["status"], "error");
    assert_eq!(current["requests"][1]["completedAt"], 1_700_000_000_006_i64);
}

#[test]
fn combined_registry_classifies_claimed_steering_and_consumes_header_change_once() {
    let mut value = assembler(&[
        at(1, "turn/start", json!({"turn": 1})),
        at(
            2,
            "request/header",
            json!({
                "reason": "initial",
                "header": {"config": {"provider": "test", "model": "test"}, "system": "system prompt", "tools": []},
            }),
        ),
        at(3, "step/start", json!({"turn": 1, "step": 1})),
        at(
            4,
            "assistant/message",
            json!({
                "turn": 1, "step": 1, "message": assistant_message("assistant-1", "first"),
            }),
        ),
        at(5, "step/end", json!({"turn": 1, "step": 1})),
        at(
            6,
            "agent/inbox/spliced",
            json!({
                "target": "next-step", "start": 0, "removedCount": 0, "inserted": [{"id": "m1"}],
            }),
        ),
        at(
            7,
            "agent/inbox/spliced",
            json!({
                "target": "next-step", "start": 0, "removedCount": 1, "inserted": [],
            }),
        ),
        at(8, "step/start", json!({"turn": 1, "step": 2})),
    ]);
    value
        .append(&at(
            9,
            "user/message",
            json!({
                "id": "m1", "content": [{"type": "text", "text": "steer here"}],
                "source": {"kind": "user"},
            }),
        ))
        .unwrap();
    value.flush().unwrap();
    let steering = snapshot(&value);
    let steering_node = steering["eventNodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["seq"] == 9)
        .unwrap();
    assert_eq!(steering_node["kind"], "steering");
    let location = steering["eventLocations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry[0].as_f64() == Some(9.0))
        .unwrap();
    assert_eq!(location[1]["kind"], "step");
    assert_eq!(location[1]["step"]["step"], 2);

    value
        .append(&at(
            10,
            "assistant/message",
            json!({
                "turn": 1, "step": 2, "message": assistant_message("assistant-2", "second"),
            }),
        ))
        .unwrap();
    value.flush().unwrap();
    let current = snapshot(&value);
    assert_eq!(current["requests"][0]["prompt"]["system"], "system prompt");
    assert_eq!(current["requests"][1]["prompt"]["system"], "system prompt");
    assert_eq!(current["requests"][0]["promptChange"]["kind"], "initial");
    assert!(current["requests"][1].get("promptChange").is_none());
}
