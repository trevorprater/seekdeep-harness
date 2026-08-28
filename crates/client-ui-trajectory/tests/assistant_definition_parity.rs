//! Assistant streaming, retry, completion, fallback, publication, and Turn-end parity.

use std::rc::Rc;

use indexmap::IndexMap;
use seekdeep_client_runtime::{
    AssemblerEventDefinitions, AssemblerNodeDefinition, AssemblerViewBuilder,
    AssemblerViewDefinition, AssemblerViewDefinitions, ConversationAssemblerError,
    ConversationEventInput, ConversationLocation, ConversationLocationEvent, ConversationMatch,
    ConversationMatchRole, ConversationNodeAssembler, ConversationPublication,
    ConversationTimelineSnapshot, ConversationViewNode,
};
use seekdeep_client_ui_trajectory::{
    TRAJECTORY_ASSISTANT_KIND, TRAJECTORY_TURN_END_KIND, trajectory_assistant_definitions,
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

fn assistant_message(seq: u64, text: &str, usage: &Value) -> ConversationEventInput {
    at(
        seq,
        "assistant/message",
        json!({
            "turn": 1,
            "step": 1,
            "message": {
                "id": "message-1",
                "content": [{"type": "text", "text": text}],
                "source": {"provider": "test", "model": "model"},
            },
            "usage": usage,
        }),
    )
}

fn assembler() -> ConversationNodeAssembler {
    ConversationNodeAssembler::new(
        Rc::new(Events(
            trajectory_assistant_definitions()
                .into_iter()
                .map(Rc::new)
                .collect(),
        )),
        Rc::new(Views),
    )
}

#[test]
fn finalized_message_separates_stream_request_usage_from_final_node_usage_and_timing() {
    let mut value = assembler();
    value
        .replace_window(
            &[
                at(1, "turn/start", json!({"turn": 1})),
                at(2, "step/start", json!({"turn": 1, "step": 1})),
                at(
                    3,
                    "assistant/chunk",
                    json!({
                        "turn": 1, "step": 1,
                        "chunk": {"type": "text-delta", "index": 0, "text": "hello"},
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
                at(
                    5,
                    "assistant/chunk",
                    json!({
                        "turn": 1, "step": 1,
                        "chunk": {"type": "usage", "usage": {
                            "inputTokens": 2, "outputTokens": 1, "cacheReadTokens": 5,
                        }},
                    }),
                ),
                assistant_message(6, "final", &json!({"inputTokens": 99, "outputTokens": 50})),
                at(7, "step/end", json!({"turn": 1, "step": 1})),
            ],
            false,
        )
        .unwrap();
    value.flush().unwrap();
    let snapshot = value.snapshot("trajectory").unwrap();
    let contribution = &snapshot[0];
    assert_eq!(contribution["kind"], "assistant");
    assert_eq!(contribution["partial"], Value::Null);
    let node = &contribution["node"];
    assert_eq!(node["messageId"], "message-1");
    assert_eq!(node["blocks"], json!([{"kind": "text", "text": "final"}]));
    assert_eq!(
        node["usage"],
        json!({"inputTokens": 99, "outputTokens": 50})
    );
    assert_eq!(
        node["provenance"],
        json!({"provider": "test", "model": "model"})
    );
    assert_eq!(node["timing"]["stepStartTime"], 1_700_000_000_002_i64);
    assert_eq!(node["timing"]["firstTokenTime"], 1_700_000_000_003_i64);
    assert_eq!(node["timing"]["completedTime"], 1_700_000_000_006_i64);
    let request = &contribution["request"];
    assert_eq!(request["status"], "complete");
    assert_eq!(request["resultSeq"], 6);
    assert_eq!(
        request["usage"],
        json!({"inputTokens": 12, "outputTokens": 4, "cacheReadTokens": 5})
    );
}

#[test]
fn retry_resets_blocks_retains_usage_and_first_token_then_interruption_closes_request() {
    let mut value = assembler();
    value
        .replace_window(
            &[
                at(1, "turn/start", json!({"turn": 1})),
                at(2, "step/start", json!({"turn": 1, "step": 1})),
                at(
                    3,
                    "assistant/chunk",
                    json!({
                        "turn": 1, "step": 1,
                        "chunk": {"type": "reasoning-delta", "index": 0, "text": "first"},
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
                        "chunk": {"type": "text-delta", "index": 0, "text": "second"},
                    }),
                ),
                at(
                    7,
                    "assistant/chunk",
                    json!({
                        "turn": 1, "step": 1,
                        "chunk": {"type": "usage", "usage": {"inputTokens": 2, "outputTokens": 1}},
                    }),
                ),
                at(8, "step/end", json!({"turn": 1, "step": 1})),
            ],
            false,
        )
        .unwrap();
    value.flush().unwrap();
    let snapshot = value.snapshot("trajectory").unwrap();
    let contribution = &snapshot[0];
    assert_eq!(contribution["node"]["seq"], json!(7.1));
    assert_eq!(contribution["node"]["interrupted"], true);
    assert_eq!(
        contribution["node"]["blocks"],
        json!([{"kind": "text", "text": "second"}])
    );
    let request = &contribution["request"];
    assert_eq!(request["status"], "error");
    assert_eq!(request["error"], "temporary failure");
    assert_eq!(request["retry"], 1);
    assert_eq!(request["maxRetries"], 2);
    assert_eq!(request["retryDelayMs"], 25);
    assert_eq!(request["completedAt"], 1_700_000_000_008_i64);
    assert_eq!(
        request["usage"],
        json!({"inputTokens": 12, "outputTokens": 4})
    );
}

#[test]
fn update_only_final_falls_back_without_inventing_request_or_step_start() {
    let mut value = assembler();
    value
        .replace_window(
            &[
                at(
                    10,
                    "assistant/chunk",
                    json!({
                        "turn": 1, "step": 1,
                        "chunk": {"type": "text-delta", "index": 0, "text": "prefix"},
                    }),
                ),
                assistant_message(
                    11,
                    "complete",
                    &json!({"inputTokens": 1, "outputTokens": 2}),
                ),
            ],
            true,
        )
        .unwrap();
    value.flush().unwrap();
    let snapshot = value.snapshot("trajectory").unwrap();
    let contribution = &snapshot[0];
    assert!(contribution.get("request").is_none());
    assert_eq!(contribution["node"]["timing"]["stepStartTime"], Value::Null);
    assert_eq!(
        contribution["node"]["timing"]["firstTokenTime"],
        1_700_000_000_010_i64
    );
}

#[test]
fn tool_only_stream_stays_partial_and_publication_cadence_matches_chunk_kind() {
    let definitions = trajectory_assistant_definitions();
    assert_eq!(definitions[0].kind, TRAJECTORY_ASSISTANT_KIND);
    assert_eq!(definitions[1].kind, TRAJECTORY_TURN_END_KIND);
    let publication = definitions[0].publication.as_ref().unwrap();
    for (event_type, chunk, expected) in [
        ("step/start", None, ConversationPublication::None),
        (
            "assistant/chunk",
            Some(json!({"type": "usage", "usage": {"inputTokens": 0, "outputTokens": 0}})),
            ConversationPublication::None,
        ),
        (
            "assistant/chunk",
            Some(json!({"type": "finish"})),
            ConversationPublication::None,
        ),
        (
            "assistant/chunk",
            Some(json!({"type": "text-delta", "index": 0, "text": "x"})),
            ConversationPublication::AnimationFrame,
        ),
        (
            "assistant/message",
            None,
            ConversationPublication::Immediate,
        ),
    ] {
        let data = chunk.map_or_else(
            || json!({"turn": 1, "step": 1}),
            |chunk| json!({"turn": 1, "step": 1, "chunk": chunk}),
        );
        let input = at(1, event_type, data);
        let accepted = ConversationMatch {
            event: input.event,
            view: None,
            role: if event_type == "step/start" {
                ConversationMatchRole::Start
            } else {
                ConversationMatchRole::Update
            },
            location: ConversationLocation::Session,
        };
        assert_eq!(publication(&accepted).unwrap(), expected);
    }

    let mut value = assembler();
    value
        .replace_window(
            &[
                at(1, "step/start", json!({"turn": 1, "step": 1})),
                at(2, "assistant/chunk", json!({
                    "turn": 1, "step": 1,
                    "chunk": {"type": "tool-call-delta", "index": 0, "id": "call", "name": "bash", "argumentsDelta": "{"},
                })),
            ],
            false,
        )
        .unwrap();
    value.flush().unwrap();
    let snapshot = value.snapshot("trajectory").unwrap();
    assert_eq!(snapshot[0]["partial"]["blocks"][0]["kind"], "tool-call");
    assert_eq!(snapshot[0]["request"]["status"], "running");
    assert!(snapshot[0].get("node").is_none());
}

#[test]
fn turn_end_projects_only_error_reason_with_display_safe_message() {
    let mut value = assembler();
    value
        .replace_window(
            &[
                at(
                    1,
                    "turn/end",
                    json!({
                        "turn": 1,
                        "reason": {"kind": "complete"},
                    }),
                ),
                at(
                    2,
                    "turn/end",
                    json!({
                        "turn": 2,
                        "reason": {"kind": "error", "error": {"code": "AUTH", "message": "secret"}},
                    }),
                ),
            ],
            false,
        )
        .unwrap();
    value.flush().unwrap();
    let snapshot = value.snapshot("trajectory").unwrap();
    assert_eq!(
        snapshot[0],
        json!({"kind": "turn-end", "turn": 1, "time": 1_700_000_000_001_i64})
    );
    assert_eq!(snapshot[1]["error"], "API key is invalid");
}
