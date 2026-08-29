//! Chat conversation Definition parity through the real incremental assembler.

use std::rc::Rc;

use indexmap::IndexMap;
use seekdeep_client_runtime::{
    AssemblerEventDefinitions, AssemblerNodeDefinition, AssemblerViewBuilder,
    AssemblerViewDefinition, AssemblerViewDefinitions, ConversationAssemblerError,
    ConversationEventInput, ConversationLocationData, ConversationLocationDataScope,
    ConversationLocationEvent, ConversationMatchResult, ConversationMatchRole,
    ConversationNodeAssembler, ConversationTimelineSnapshot, ConversationViewNode,
    ConversationVisibility,
};
use seekdeep_client_ui_conversation::{
    CHAT_FINALIZED_FOLLOWUP_OFFSET, CHAT_INTERRUPTED_ASSISTANT_OFFSET,
    CHAT_INTERRUPTED_FOLLOWUP_OFFSET, CHAT_MAX_TOKENS_NOTICE_OFFSET, conversation_coordinate,
    conversation_inbox_definitions, conversation_message_definition, conversation_retry_definition,
    conversation_turn_error_definition, conversation_turn_max_tokens_definition,
    conversation_unknown_fallback_definition,
};
use serde_json::{Value, json};

struct Events {
    entries: Vec<Rc<AssemblerNodeDefinition>>,
    fallback: Option<Rc<AssemblerNodeDefinition>>,
}

impl AssemblerEventDefinitions for Events {
    fn entries(&self) -> Vec<Rc<AssemblerNodeDefinition>> {
        self.entries.clone()
    }

    fn fallback_entry(&self) -> Option<Rc<AssemblerNodeDefinition>> {
        self.fallback.clone()
    }
}

struct Views;

impl AssemblerViewDefinitions for Views {
    fn entries(&self) -> Vec<Rc<AssemblerViewDefinition>> {
        vec![Rc::new(AssemblerViewDefinition {
            target: "chat".to_owned(),
            create: Rc::new(|| Box::new(ChatBuilder::default())),
        })]
    }
}

#[derive(Default)]
struct ChatBuilder {
    nodes: IndexMap<String, Rc<ConversationViewNode>>,
}

impl ChatBuilder {
    fn snapshot(&self) -> Rc<Value> {
        Rc::new(Value::Array(self.nodes.values().map(node_value).collect()))
    }
}

impl AssemblerViewBuilder for ChatBuilder {
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

fn node_value(node: &Rc<ConversationViewNode>) -> Value {
    let chat = node.chat.as_ref().expect("chat metadata");
    json!({
        "key": node.key,
        "kind": node.kind,
        "id": node.id,
        "anchorSeq": chat.anchor_seq,
        "visibility": match chat.visibility {
            ConversationVisibility::Visible => "visible",
            ConversationVisibility::Hidden => "hidden",
        },
        "data": node.data.as_ref().clone(),
    })
}

fn assembler(
    definitions: Vec<AssemblerNodeDefinition>,
    fallback: Option<AssemblerNodeDefinition>,
    entries: &[ConversationEventInput],
    has_more: bool,
) -> ConversationNodeAssembler {
    let mut value = ConversationNodeAssembler::new(
        Rc::new(Events {
            entries: definitions.into_iter().map(Rc::new).collect(),
            fallback: fallback.map(Rc::new),
        }),
        Rc::new(Views),
    );
    value.replace_window(entries, has_more).unwrap();
    value.flush().unwrap();
    value
}

fn snapshot(value: &ConversationNodeAssembler) -> Rc<Value> {
    value.snapshot("chat").expect("chat snapshot")
}

fn node<'a>(snapshot: &'a Value, kind: &str) -> Option<&'a Value> {
    snapshot
        .as_array()
        .expect("node array")
        .iter()
        .find(|node| node["kind"] == kind)
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

fn surface(seq: u64, event_type: &str, data: Value, surface_op: Value) -> ConversationEventInput {
    let time = 1_700_000_000_000_i64 + i64::try_from(seq).unwrap();
    let mut wire = json!({
        "seq": seq,
        "time": time,
        "type": event_type,
        "data": data,
    });
    wire["surfaceOp"] = surface_op;
    ConversationEventInput {
        event: ConversationLocationEvent::with_wire(seq, time, event_type, data, wire),
        view: None,
    }
}

fn text_message(id: &str, text: &str, source: Value) -> Value {
    let mut message = json!({
        "id": id,
        "role": "user",
        "content": [{"type": "text", "text": text}],
    });
    message["source"] = source;
    message
}

fn retry(seq: u64, retry: u64, message: &str) -> ConversationEventInput {
    at(
        seq,
        "llm/retry",
        json!({
            "retryId": "retry-1",
            "turn": 1,
            "step": 1,
            "provider": "fake",
            "mode": "normal",
            "policyKey": "fake-normal",
            "retry": retry,
            "maxRetries": 2,
            "delayMs": retry * 10,
            "failure": {"code": "TRANSPORT", "message": message},
        }),
    )
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered fixture covers cross-definition replay semantics.
fn inbox_dependencies_classify_messages_and_replay_after_prepend() {
    let mut definitions = conversation_inbox_definitions()
        .into_iter()
        .collect::<Vec<_>>();
    definitions.push(conversation_message_definition());
    let mut value = assembler(
        definitions,
        Some(conversation_unknown_fallback_definition()),
        &[
            at(
                1,
                "agent/inbox/spliced",
                json!({"target": "next-turn", "start": 0, "inserted": [{"id": "turn-only"}]}),
            ),
            at(
                2,
                "agent/inbox/spliced",
                json!({"target": "next-turn", "start": 0, "removedCount": 1, "inserted": []}),
            ),
            at(
                3,
                "agent/inbox/spliced",
                json!({"target": "next-step", "start": 0, "inserted": [{"id": "steer"}]}),
            ),
            at(
                4,
                "agent/inbox/spliced",
                json!({"target": "next-step", "start": 0, "removedCount": 1, "inserted": []}),
            ),
            surface(
                5,
                "user/message",
                text_message("steer", "change direction", json!({"kind": "user"})),
                json!("append"),
            ),
            surface(
                6,
                "user/message",
                text_message("turn-only", "plain", json!({"kind": "user"})),
                json!("append"),
            ),
            surface(
                7,
                "user/message",
                text_message(
                    "skill",
                    "instructions",
                    json!({
                        "kind": "skill-invocation",
                        "name": "demo-skill",
                        "form": "instructions",
                    }),
                ),
                json!("append"),
            ),
            surface(
                8,
                "user/message",
                text_message(
                    "replacement",
                    "model only",
                    json!({"kind": "plugin", "plugin": "foreign"}),
                ),
                json!({"op": "replace", "start": 1, "end": 1}),
            ),
            surface(
                9,
                "tool/result",
                json!({"message": "unclaimed"}),
                json!("append"),
            ),
            at(
                10,
                "agent/inbox/spliced",
                json!({"target": "next-step", "start": 0, "inserted": [{"id": "reinserted"}]}),
            ),
            at(
                11,
                "agent/inbox/spliced",
                json!({"target": "next-step", "start": 0, "removedCount": 1, "inserted": []}),
            ),
            at(
                12,
                "agent/inbox/spliced",
                json!({"target": "next-step", "start": 0, "inserted": [{"id": "reinserted"}]}),
            ),
            surface(
                13,
                "user/message",
                text_message("reinserted", "reinserted", json!({"kind": "user"})),
                json!("append"),
            ),
            at(
                14,
                "agent/inbox/spliced",
                json!({"target": "next-step", "start": 1, "inserted": [{"id": "canceled"}]}),
            ),
            at(
                15,
                "agent/inbox/spliced",
                json!({
                    "target": "next-step", "start": 1, "removedCount": 1,
                    "inserted": [], "outcome": "canceled",
                }),
            ),
            surface(
                16,
                "user/message",
                text_message("canceled", "canceled", json!({"kind": "user"})),
                json!("append"),
            ),
        ],
        false,
    );
    let current = snapshot(&value);
    assert_eq!(
        node(&current, "steering").unwrap()["data"]["messageId"],
        "steer"
    );
    assert_eq!(
        node(&current, "user").unwrap()["data"]["content"][0]["text"],
        "plain"
    );
    assert_eq!(
        node(&current, "context").unwrap()["data"]["provenance"],
        json!({"role": "inject", "label": "demo-skill"})
    );
    assert_eq!(
        node(&current, "context").unwrap()["data"]["form"],
        "instructions"
    );
    assert_eq!(
        node(&current, "unknown").unwrap()["data"]["type"],
        "tool/result"
    );
    let user_texts = current
        .as_array()
        .unwrap()
        .iter()
        .filter(|node| node["kind"] == "user")
        .map(|node| node["data"]["content"][0]["text"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(user_texts, ["plain", "reinserted", "canceled"]);
    assert_eq!(current.as_array().unwrap().len(), 6);

    let mut paged = assembler(
        {
            let mut entries = conversation_inbox_definitions()
                .into_iter()
                .collect::<Vec<_>>();
            entries.push(conversation_message_definition());
            entries
        },
        None,
        &[surface(
            13,
            "user/message",
            text_message("paged", "later", json!({"kind": "user"})),
            json!("append"),
        )],
        true,
    );
    let before = snapshot(&paged);
    let key = node(&before, "user").unwrap()["key"].clone();
    paged
        .prepend(
            &[
                at(
                    11,
                    "agent/inbox/spliced",
                    json!({"target": "next-step", "start": 0, "inserted": [{"id": "paged"}]}),
                ),
                at(
                    12,
                    "agent/inbox/spliced",
                    json!({"target": "next-step", "start": 0, "removedCount": 1, "inserted": []}),
                ),
            ],
            false,
        )
        .unwrap();
    paged.flush().unwrap();
    let after = snapshot(&paged);
    assert!(node(&after, "user").is_none());
    assert_eq!(node(&after, "steering").unwrap()["key"], key);

    value
        .append(&surface(
            17,
            "user/message",
            text_message("plain-2", "tail", json!({"kind": "user"})),
            json!("append"),
        ))
        .unwrap();
    value.flush().unwrap();
    assert_eq!(snapshot(&value).as_array().unwrap().len(), 7);
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered fixture covers visible and paged error lifecycles.
fn retry_chain_cancels_on_boundary_and_suppresses_or_hides_turn_error() {
    let definitions = vec![
        conversation_retry_definition(),
        conversation_turn_error_definition(),
    ];
    let value = assembler(
        definitions,
        None,
        &[
            at(1, "turn/start", json!({"turn": 1})),
            at(2, "step/start", json!({"turn": 1, "step": 1})),
            retry(3, 1, "first"),
            at(
                4,
                "llm/retry-started",
                json!({"retryId": "retry-1", "turn": 1, "step": 1, "retry": 1}),
            ),
            retry(5, 2, "second"),
            at(6, "step/end", json!({"turn": 1, "step": 1})),
            at(
                7,
                "turn/end",
                json!({
                    "turn": 1,
                    "reason": {"kind": "error", "error": {"code": "TRANSPORT", "message": "failed"}},
                }),
            ),
        ],
        false,
    );
    let current = snapshot(&value);
    let retry_node = node(&current, "model-retry").unwrap();
    assert_eq!(
        retry_node["data"]["attempts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|attempt| attempt["retryState"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["started", "cancelled"]
    );
    assert_eq!(retry_node["data"]["current"]["retry"], 2);
    assert!(node(&current, "turn-error").is_none());

    let mut failed = assembler(
        vec![conversation_turn_error_definition()],
        None,
        &[
            at(10, "turn/start", json!({"turn": 2})),
            at(11, "step/start", json!({"turn": 2, "step": 1})),
            at(12, "step/end", json!({"turn": 2, "step": 1})),
            at(
                13,
                "turn/end",
                json!({
                    "turn": 2,
                    "reason": {"kind": "error", "error": {"code": "BAD", "message": "boom"}},
                }),
            ),
        ],
        false,
    );
    let visible = snapshot(&failed);
    assert_eq!(
        node(&visible, "turn-error").unwrap()["visibility"],
        "visible"
    );
    assert_eq!(node(&visible, "turn-error").unwrap()["data"]["step"], 1);
    assert_eq!(
        node(&visible, "turn-error").unwrap()["data"]["message"],
        "boom"
    );
    assert_eq!(node(&visible, "turn-error").unwrap()["data"]["code"], "BAD");
    failed
        .append(&at(
            14,
            "llm/retry",
            json!({
                "retryId": "late",
                "turn": 2,
                "step": 1,
                "retry": 1,
                "failure": {"message": "again"},
            }),
        ))
        .unwrap();
    failed.flush().unwrap();
    assert_eq!(
        node(&snapshot(&failed), "turn-error").unwrap()["visibility"],
        "hidden"
    );

    let paged = assembler(
        vec![conversation_turn_error_definition()],
        None,
        &[
            retry(20, 2, "second"),
            at(
                21,
                "turn/end",
                json!({
                    "turn": 1,
                    "reason": {"kind": "error", "error": {"message": "failed"}},
                }),
            ),
        ],
        true,
    );
    assert!(node(&snapshot(&paged), "turn-error").is_none());

    let legacy = assembler(
        vec![conversation_retry_definition()],
        None,
        &[at(
            30,
            "llm/retry",
            json!({
                "turn": 1,
                "step": 1,
                "retry": 1,
                "failure": {"message": "legacy"},
            }),
        )],
        true,
    );
    assert!(node(&snapshot(&legacy), "model-retry").is_none());
}

#[test]
fn max_tokens_notice_uses_tail_closing_anchor_and_survives_partial_windows() {
    let value = assembler(
        vec![
            synthetic_turn_tail_location_data(),
            conversation_turn_max_tokens_definition(),
            conversation_turn_error_definition(),
        ],
        None,
        &[
            at(1, "turn/start", json!({"turn": 1})),
            at(2, "step/start", json!({"turn": 1, "step": 1})),
            surface(
                3,
                "assistant/message",
                json!({"turn": 1, "step": 1}),
                json!("append"),
            ),
            at(4, "step/end", json!({"turn": 1, "step": 1})),
            at(
                5,
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "max-tokens"}}),
            ),
        ],
        false,
    );
    let current = snapshot(&value);
    let notice = node(&current, "turn-max-tokens").unwrap();
    assert_eq!(notice["data"]["step"], 1);
    assert_eq!(notice["anchorSeq"], 3.05);
    assert!(node(&current, "turn-error").is_none());

    let partial = assembler(
        vec![conversation_turn_max_tokens_definition()],
        None,
        &[at(
            9,
            "turn/end",
            json!({"turn": 3, "reason": {"kind": "max-tokens"}}),
        )],
        true,
    );
    let partial_snapshot = snapshot(&partial);
    let partial_notice = node(&partial_snapshot, "turn-max-tokens").unwrap();
    assert_eq!(partial_notice["data"]["turn"], 3);
    assert_eq!(partial_notice["data"]["step"], 0);
    assert_eq!(partial_notice["anchorSeq"], 9.0);

    assert!((CHAT_INTERRUPTED_ASSISTANT_OFFSET - -0.9).abs() < f64::EPSILON);
    assert!((CHAT_INTERRUPTED_FOLLOWUP_OFFSET - -0.8).abs() < f64::EPSILON);
    assert!((CHAT_MAX_TOKENS_NOTICE_OFFSET - 0.05).abs() < f64::EPSILON);
    assert!((CHAT_FINALIZED_FOLLOWUP_OFFSET - 0.1).abs() < f64::EPSILON);
    assert_eq!(conversation_coordinate(&json!(0)), Some(0));
    assert_eq!(conversation_coordinate(&json!(7.0)), Some(7));
    assert_eq!(
        conversation_coordinate(&json!(9_007_199_254_740_991_u64)),
        Some(9_007_199_254_740_991)
    );
    for invalid in [
        json!(-1),
        json!(1.5),
        json!(9_007_199_254_740_992_u64),
        json!("1"),
        Value::Null,
    ] {
        assert_eq!(conversation_coordinate(&invalid), None);
    }
}

fn synthetic_turn_tail_location_data() -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: "turn-tail".to_owned(),
        target: None,
        match_event: Rc::new(|event| {
            Ok(
                (event.event_type == "assistant/message").then(|| ConversationMatchResult {
                    id: event
                        .data
                        .get("turn")
                        .and_then(Value::as_u64)
                        .unwrap()
                        .to_string(),
                    role: ConversationMatchRole::Start,
                }),
            )
        }),
        start: Rc::new(|_context, accepted, _reader| {
            Ok(Some(Rc::new(json!({
                "turn": accepted.event.data["turn"],
                "closing": {"finalNode": {"seq": accepted.event.seq}},
            }))))
        }),
        update: Rc::new(|context, _accepted| Ok(context.state.clone())),
        publication: None,
        build_location_data: Some(Rc::new(|context, scope| {
            if scope != ConversationLocationDataScope::Turn {
                return Ok(None);
            }
            let Some(state) = context.state.clone() else {
                return Ok(None);
            };
            Ok(Some(Rc::new(ConversationLocationData::Turn {
                turn: state["turn"].as_u64().unwrap(),
                key: "turn-tail".to_owned(),
                value: state,
            })))
        })),
        build_view_node: None,
    }
}
