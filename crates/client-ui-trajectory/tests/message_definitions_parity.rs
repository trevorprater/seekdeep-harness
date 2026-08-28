//! Inbox splice and user/context/steering Definition parity.

use std::rc::Rc;

use indexmap::IndexMap;
use seekdeep_client_runtime::{
    AssemblerEventDefinitions, AssemblerNodeDefinition, AssemblerViewBuilder,
    AssemblerViewDefinition, AssemblerViewDefinitions, ConversationAssemblerError,
    ConversationEventInput, ConversationLocationEvent, ConversationNodeAssembler,
    ConversationTimelineSnapshot, ConversationViewNode,
};
use seekdeep_client_ui_trajectory::{
    TRAJECTORY_COMPACTION_KIND, TRAJECTORY_INBOX_KIND, TRAJECTORY_INPUT_MESSAGE_KIND,
    TRAJECTORY_SESSION_END_KIND, trajectory_compaction_definitions, trajectory_message_definitions,
};
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
        vec![Rc::new(AssemblerViewDefinition {
            target: "trajectory".to_owned(),
            create: Rc::new(|| Box::new(DataBuilder::default())),
        })]
    }
}

#[derive(Default)]
struct DataBuilder {
    nodes: IndexMap<String, Rc<ConversationViewNode>>,
}

impl DataBuilder {
    fn snapshot(&self) -> Rc<Value> {
        Rc::new(Value::Array(
            self.nodes
                .values()
                .map(|node| node.data.as_ref().clone())
                .collect(),
        ))
    }
}

impl AssemblerViewBuilder for DataBuilder {
    fn empty(&self) -> Rc<Value> {
        self.snapshot()
    }

    fn replace(
        &mut self,
        nodes: &[Rc<ConversationViewNode>],
        _timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        self.nodes = nodes
            .iter()
            .map(|node| (node.key.clone(), node.clone()))
            .collect();
        Ok(self.snapshot())
    }

    fn apply(
        &mut self,
        upserts: &[Rc<ConversationViewNode>],
        _timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        for node in upserts {
            self.nodes.insert(node.key.clone(), node.clone());
        }
        Ok(self.snapshot())
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

fn splice(
    start: u64,
    removed_count: Option<u64>,
    inserted: &[&str],
    outcome: Option<&str>,
) -> Value {
    let mut value = serde_json::Map::from_iter([
        ("target".to_owned(), json!("next-step")),
        ("start".to_owned(), json!(start)),
        (
            "inserted".to_owned(),
            Value::Array(inserted.iter().map(|id| json!({"id": id})).collect()),
        ),
    ]);
    if let Some(removed_count) = removed_count {
        value.insert("removedCount".to_owned(), json!(removed_count));
    }
    if let Some(outcome) = outcome {
        value.insert("outcome".to_owned(), json!(outcome));
    }
    Value::Object(value)
}

fn user_message(id: &str, text: &str, source: &Value) -> Value {
    json!({
        "id": id,
        "content": [{"type": "text", "text": text}],
        "source": source,
    })
}

#[test]
fn claimed_removed_messages_are_steering_while_reinserted_and_canceled_are_user() {
    let definitions = trajectory_message_definitions();
    assert_eq!(definitions[0].kind, TRAJECTORY_INBOX_KIND);
    assert_eq!(definitions[0].target, None);
    assert_eq!(definitions[1].kind, TRAJECTORY_INPUT_MESSAGE_KIND);
    assert_eq!(definitions[1].target.as_deref(), Some("trajectory"));
    let mut value = ConversationNodeAssembler::new(
        Rc::new(Events(definitions.into_iter().map(Rc::new).collect())),
        Rc::new(Views),
    );
    value
        .replace_window(
            &[
                at(1, "agent/inbox/spliced", splice(0, None, &["m1"], None)),
                at(2, "agent/inbox/spliced", splice(0, Some(1), &[], None)),
                at(
                    3,
                    "user/message",
                    user_message("m1", "steer", &json!({"kind": "user"})),
                ),
                at(
                    4,
                    "user/message",
                    user_message("m2", "plain", &json!({"kind": "user"})),
                ),
                at(
                    5,
                    "user/message",
                    user_message(
                        "ctx",
                        "injected",
                        &json!({"kind": "plugin", "plugin": "memory", "form": "notice"}),
                    ),
                ),
                at(6, "agent/inbox/spliced", splice(0, None, &["m1"], None)),
                at(
                    7,
                    "user/message",
                    user_message("m1", "reinserted", &json!({"kind": "user"})),
                ),
                at(
                    8,
                    "agent/inbox/spliced",
                    json!({"target": "now", "start": 0, "inserted": [{"id": "ignored"}]}),
                ),
                at(9, "agent/inbox/spliced", splice(1, None, &["m3"], None)),
                at(
                    10,
                    "agent/inbox/spliced",
                    splice(1, Some(1), &[], Some("canceled")),
                ),
                at(
                    11,
                    "user/message",
                    user_message("m3", "canceled", &json!({"kind": "user"})),
                ),
                at(
                    12,
                    "user/message",
                    user_message(
                        "recall",
                        "remembered",
                        &json!({
                            "kind": "session-reference",
                            "form": "recall",
                            "references": [{"label": "Alpha"}, {"label": "Beta"}],
                        }),
                    ),
                ),
            ],
            false,
        )
        .unwrap();
    value.flush().unwrap();
    let snapshot = value.snapshot("trajectory").unwrap();
    let nodes = snapshot.as_array().unwrap();
    assert_eq!(nodes.len(), 6);
    assert_eq!(nodes[0]["node"]["kind"], "steering");
    assert_eq!(nodes[0]["node"]["messageId"], "m1");
    assert_eq!(nodes[1]["node"]["kind"], "user");
    assert_eq!(nodes[2]["node"]["kind"], "context");
    assert_eq!(
        nodes[2]["node"]["provenance"],
        json!({"role": "inject", "label": "memory"})
    );
    assert_eq!(nodes[2]["node"]["form"], "notice");
    assert_eq!(nodes[3]["node"]["kind"], "user");
    assert_eq!(nodes[3]["node"]["content"][0]["text"], "reinserted");
    assert_eq!(nodes[4]["node"]["kind"], "user");
    assert_eq!(nodes[4]["node"]["content"][0]["text"], "canceled");
    assert_eq!(nodes[5]["node"]["kind"], "context");
    assert_eq!(
        nodes[5]["node"]["provenance"],
        json!({"role": "recall", "label": "Alpha, Beta"})
    );
    assert_eq!(nodes[5]["node"]["form"], "recall");
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered lifecycle fixture mirrors the source protocol.
fn compaction_lifecycle_preserves_summary_checkpoint_error_running_and_session_end() {
    let definitions = trajectory_compaction_definitions();
    assert_eq!(definitions[0].kind, TRAJECTORY_COMPACTION_KIND);
    assert_eq!(definitions[1].kind, TRAJECTORY_SESSION_END_KIND);
    let mut value = ConversationNodeAssembler::new(
        Rc::new(Events(definitions.into_iter().map(Rc::new).collect())),
        Rc::new(Views),
    );
    value
        .replace_window(
            &[
                at(
                    1,
                    "compaction/start",
                    json!({"compactionId": "complete", "turn": null}),
                ),
                at(
                    2,
                    "compaction/summary",
                    json!({
                        "compactionId": "complete",
                        "turn": null,
                        "summary": "summary",
                        "rawOutput": [{"type": "text", "text": "raw"}],
                        "provider": "test",
                        "model": "model",
                        "maxTokens": 100,
                        "usage": {"inputTokens": 20, "outputTokens": 5},
                    }),
                ),
                at(
                    3,
                    "user/message",
                    json!({
                        "id": "checkpoint",
                        "content": [],
                        "source": {
                            "kind": "plugin",
                            "plugin": "compact",
                            "compactionId": "complete",
                        },
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
                    json!({"compactionId": "failed", "turn": 2}),
                ),
                at(
                    6,
                    "compaction/end",
                    json!({"compactionId": "failed", "turn": 2, "error": "boom"}),
                ),
                at(
                    7,
                    "compaction/start",
                    json!({"compactionId": "orphan", "turn": null}),
                ),
                at(
                    8,
                    "compaction/start",
                    json!({"compactionId": "", "turn": null}),
                ),
                at(9, "session/end-seed", json!({})),
            ],
            false,
        )
        .unwrap();
    value.flush().unwrap();
    let snapshot = value.snapshot("trajectory").unwrap();
    let nodes = snapshot.as_array().unwrap();
    assert_eq!(nodes.len(), 4);
    let complete = &nodes[0]["request"];
    assert_eq!(nodes[0]["kind"], "compaction");
    assert_eq!(complete["purpose"], "compaction");
    assert_eq!(complete["turn"], Value::Null);
    assert_eq!(complete["step"], 0);
    assert_eq!(complete["status"], "complete");
    assert_eq!(complete["completedAt"], 1_700_000_000_004_i64);
    assert_eq!(complete["resultSeq"], 2);
    assert_eq!(complete["summary"], "summary");
    assert_eq!(complete["rawOutput"][0]["text"], "raw");
    assert_eq!(
        complete["provenance"],
        json!({"provider": "test", "model": "model"})
    );
    assert_eq!(complete["requestConfig"]["purpose"], "compaction");
    assert_eq!(complete["requestConfig"]["maxTokens"], 100);
    assert_eq!(complete["usage"]["outputTokens"], 5);
    assert_eq!(complete["replacementSeq"], 3);

    assert_eq!(nodes[1]["request"]["status"], "error");
    assert_eq!(nodes[1]["request"]["error"], "boom");
    assert_eq!(nodes[2]["request"]["status"], "running");
    assert_eq!(nodes[2]["request"]["completedAt"], Value::Null);
    assert_eq!(
        nodes[3],
        json!({"kind": "session-end", "seq": 9, "time": 1_700_000_000_009_i64})
    );
}
