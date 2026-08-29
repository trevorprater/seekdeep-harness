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
    CHAT_INTERRUPTED_FOLLOWUP_OFFSET, CHAT_MAX_TOKENS_NOTICE_OFFSET,
    conversation_assistant_definition, conversation_chat_view_definition,
    conversation_command_definition, conversation_compaction_definition, conversation_coordinate,
    conversation_inbox_definitions, conversation_message_definition, conversation_retry_definition,
    conversation_tool_definition, conversation_turn_error_definition,
    conversation_turn_max_tokens_definition, conversation_turn_tail_definition,
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

struct ProductionChatViews;

impl AssemblerViewDefinitions for ProductionChatViews {
    fn entries(&self) -> Vec<Rc<AssemblerViewDefinition>> {
        vec![Rc::new(conversation_chat_view_definition())]
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

fn production_chat_assembler(
    definitions: Vec<AssemblerNodeDefinition>,
    entries: &[ConversationEventInput],
) -> ConversationNodeAssembler {
    let mut value = ConversationNodeAssembler::new(
        Rc::new(Events {
            entries: definitions.into_iter().map(Rc::new).collect(),
            fallback: None,
        }),
        Rc::new(ProductionChatViews),
    );
    value.replace_window(entries, false).unwrap();
    value.flush().unwrap();
    value
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
#[allow(clippy::too_many_lines)] // One ordered fixture covers correlated and paged transactions.
fn commands_keep_manual_and_automatic_compaction_ownership_separate() {
    let value = assembler(
        vec![
            conversation_command_definition(),
            conversation_compaction_definition(),
        ],
        None,
        &[
            at(
                1,
                "command/run",
                json!({"commandId": "ordinary", "name": "help", "args": "--all"}),
            ),
            at(
                2,
                "command/done",
                json!({
                    "commandId": "ordinary", "kind": "success", "text": "ok",
                    "sourceEventSeq": 1,
                }),
            ),
            at(
                10,
                "command/run",
                json!({"commandId": "manual-command", "name": "compact"}),
            ),
            at(
                11,
                "compaction/start",
                json!({
                    "compactionId": "manual", "sourceCommandId": "manual-command", "turn": null,
                }),
            ),
            at(
                12,
                "compaction/summary",
                json!({
                    "compactionId": "manual", "sourceCommandId": "manual-command",
                    "summary": [
                        {"type": "text", "text": "manual "},
                        {"type": "image", "data": "ignored"},
                        {"type": "text", "text": "summary"},
                    ],
                    "shadowedSeqs": [1, 2], "shadowedTokenCount": 100,
                }),
            ),
            surface(
                13,
                "user/message",
                text_message(
                    "manual-checkpoint",
                    "checkpoint",
                    json!({
                        "kind": "plugin", "plugin": "compact", "compactionId": "manual",
                        "sourceCommandId": "manual-command",
                    }),
                ),
                json!({"op": "replace", "start": 1, "end": 2}),
            ),
            at(
                14,
                "compaction/end",
                json!({
                    "compactionId": "manual", "sourceCommandId": "manual-command", "turn": null,
                }),
            ),
            at(
                15,
                "command/done",
                json!({
                    "commandId": "manual-command", "kind": "success", "sourceEventSeq": 12,
                }),
            ),
            at(
                20,
                "compaction/start",
                json!({"compactionId": "automatic", "turn": null}),
            ),
            at(
                21,
                "compaction/summary",
                json!({
                    "compactionId": "automatic",
                    "summary": [{"type": "text", "text": "automatic summary"}],
                    "shadowedSeqs": [3, 4, 5], "shadowedTokenCount": 200,
                }),
            ),
            surface(
                22,
                "user/message",
                text_message(
                    "automatic-checkpoint",
                    "checkpoint",
                    json!({"kind": "plugin", "plugin": "compact", "compactionId": "automatic"}),
                ),
                json!({"op": "replace", "start": 3, "end": 5}),
            ),
            at(
                23,
                "compaction/end",
                json!({"compactionId": "automatic", "turn": null}),
            ),
        ],
        false,
    );
    let current = snapshot(&value);
    let ordinary = node(&current, "command").unwrap();
    assert_eq!(ordinary["data"]["seq"], 1);
    assert_eq!(ordinary["data"]["name"], "help");
    assert_eq!(ordinary["data"]["args"], "--all");
    assert_eq!(ordinary["data"]["outcome"]["text"], "ok");
    assert_eq!(ordinary["data"]["outcome"]["sourceEventSeq"], 1);

    let manual = node(&current, "manual-compaction").unwrap();
    assert_eq!(manual["anchorSeq"], 13.0);
    assert_eq!(manual["data"]["command"]["seq"], 10);
    assert_eq!(manual["data"]["command"]["outcome"]["sourceEventSeq"], 12);
    assert_eq!(manual["data"]["compaction"]["summary"], "manual summary");
    assert_eq!(manual["data"]["compaction"]["summaryEventSeq"], 12);
    assert_eq!(manual["data"]["compaction"]["shadowedItemCount"], 2);
    assert_eq!(manual["data"]["compaction"]["shadowedTokenCount"], 100);

    let automatic = node(&current, "compaction").unwrap();
    assert_eq!(automatic["data"]["summary"], "automatic summary");
    assert_eq!(automatic["data"]["summaryEventSeq"], 21);
    assert_eq!(automatic["data"]["shadowedItemCount"], 3);
    assert_eq!(automatic["data"]["shadowedTokenCount"], 200);

    let mut paged = assembler(
        vec![conversation_compaction_definition()],
        None,
        &[surface(
            33,
            "user/message",
            text_message(
                "paged-checkpoint",
                "checkpoint",
                json!({"kind": "plugin", "plugin": "compact", "compactionId": "paged"}),
            ),
            json!({"op": "replace", "start": 1, "end": 3}),
        )],
        true,
    );
    let before = snapshot(&paged);
    let before_node = node(&before, "compaction").unwrap();
    let key = before_node["key"].clone();
    assert_eq!(before_node["data"]["summary"], Value::Null);
    paged
        .prepend(
            &[
                at(
                    31,
                    "compaction/start",
                    json!({"compactionId": "paged", "turn": null}),
                ),
                at(
                    32,
                    "compaction/summary",
                    json!({
                        "compactionId": "paged",
                        "summary": [{"type": "text", "text": "older summary"}],
                        "shadowedSeqs": [1, 2, 3], "shadowedTokenCount": 42,
                    }),
                ),
            ],
            false,
        )
        .unwrap();
    paged.flush().unwrap();
    let after = snapshot(&paged);
    let after_node = node(&after, "compaction").unwrap();
    assert_eq!(after_node["key"], key);
    assert_eq!(after_node["data"]["summary"], "older summary");
    assert_eq!(after_node["data"]["shadowedTokenCount"], 42);

    let historical_manual = assembler(
        vec![conversation_command_definition()],
        None,
        &[
            at(
                40,
                "compaction/summary",
                json!({
                    "compactionId": "historical", "sourceCommandId": "missing-run",
                    "summary": [{"type": "text", "text": "historical summary"}],
                    "shadowedSeqs": [1], "shadowedTokenCount": 9,
                }),
            ),
            surface(
                41,
                "user/message",
                text_message(
                    "historical-checkpoint",
                    "checkpoint",
                    json!({
                        "kind": "plugin", "plugin": "compact", "compactionId": "historical",
                        "sourceCommandId": "missing-run",
                    }),
                ),
                json!({"op": "replace", "start": 1, "end": 1}),
            ),
            at(
                42,
                "command/done",
                json!({"commandId": "missing-run", "kind": "success", "sourceEventSeq": 40}),
            ),
        ],
        true,
    );
    let historical = node(&snapshot(&historical_manual), "manual-compaction")
        .unwrap()
        .clone();
    assert_eq!(historical["data"]["command"]["name"], "compact");
    assert_eq!(historical["data"]["command"]["seq"], 42);
    assert_eq!(
        historical["data"]["compaction"]["summary"],
        "historical summary"
    );

    let legacy = assembler(
        vec![conversation_compaction_definition()],
        None,
        &[
            at(50, "compaction/start", json!({"turn": null})),
            at(
                51,
                "compaction/summary",
                json!({"summary": [], "shadowedSeqs": [], "shadowedTokenCount": 0}),
            ),
            surface(
                52,
                "user/message",
                text_message(
                    "legacy-checkpoint",
                    "checkpoint",
                    json!({"kind": "plugin", "plugin": "compact"}),
                ),
                json!({"op": "replace", "start": 1, "end": 1}),
            ),
        ],
        true,
    );
    assert!(node(&snapshot(&legacy), "compaction").is_none());
}

#[test]
#[allow(clippy::too_many_lines)] // One ordered fixture covers the cross-Definition lifecycle.
fn assistant_tool_and_tail_share_location_data_and_interruption_ordering() {
    let value = assembler(
        vec![
            conversation_assistant_definition(),
            conversation_tool_definition(),
            conversation_turn_tail_definition(),
        ],
        None,
        &[
            at(1, "turn/start", json!({"turn": 1})),
            at(2, "step/start", json!({"turn": 1, "step": 1})),
            at(
                3,
                "assistant/chunk",
                json!({
                    "turn": 1, "step": 1,
                    "chunk": {"type": "text-delta", "index": 0, "text": "streaming"},
                }),
            ),
            surface(
                4,
                "assistant/message",
                json!({
                    "turn": 1, "step": 1,
                    "message": {
                        "id": "assistant-1",
                        "content": [{"type": "text", "text": "answer"}],
                        "source": {"kind": "model", "provider": "fake", "model": "fake"},
                    },
                    "usage": {"inputTokens": 20, "outputTokens": 10},
                }),
                json!("append"),
            ),
            at(
                5,
                "tool/call",
                json!({"turn": 1, "step": 1, "callId": "root", "name": "code", "arguments": "{}"}),
            ),
            at(
                6,
                "tool/code-dispatch-start",
                json!({
                    "rootCallId": "root", "parentCallId": "root", "subCallId": "child",
                    "name": "read", "arguments": {"path": "README.md"},
                }),
            ),
            at(
                7,
                "tool/code-dispatch",
                json!({
                    "rootCallId": "root", "parentCallId": "root", "subCallId": "child",
                    "name": "read", "arguments": {"path": "README.md"}, "isError": false,
                    "content": [{"type": "text", "text": "contents"}],
                }),
            ),
            surface(
                8,
                "tool/result",
                json!({
                    "turn": 1, "step": 1,
                    "message": {
                        "source": {"kind": "tool", "callId": "root"},
                        "content": [{
                            "type": "tool-result", "toolCallId": "root", "isError": false,
                            "content": [{"type": "text", "text": "done"}],
                        }],
                    },
                    "meta": {"durationMs": 3},
                }),
                json!("append"),
            ),
            at(9, "step/end", json!({"turn": 1, "step": 1})),
            at(
                10,
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
        ],
        false,
    );
    let current = snapshot(&value);
    let assistant = node(&current, "assistant-step").unwrap();
    assert_eq!(assistant["data"]["status"], "settled");
    assert_eq!(assistant["data"]["blocks"][0]["text"], "answer");
    assert_eq!(
        assistant["data"]["finalNode"]["timing"]["stepStartTime"],
        1_700_000_000_002_i64
    );
    assert_eq!(
        assistant["data"]["finalNode"]["timing"]["firstTokenTime"],
        1_700_000_000_003_i64
    );
    let tool = node(&current, "tool-call").unwrap();
    assert_eq!(tool["data"]["root"]["kind"], "tool-result");
    assert_eq!(tool["data"]["root"]["subCalls"][0]["callId"], "child");
    assert_eq!(
        tool["data"]["root"]["subCalls"][0]["callTime"],
        1_700_000_000_006_i64
    );
    let tail = node(&current, "turn-tail").unwrap();
    assert_eq!(tail["anchorSeq"], 4.1);
    assert_eq!(tail["data"]["closing"]["finalNode"]["seq"], 4);
    assert_eq!(tail["data"]["branchUnavailable"], true);
    assert_eq!(tail["data"]["ttftMs"], 1.0);
    assert_eq!(tail["data"]["tokensPerSecond"], 10_000.0);

    let interrupted = assembler(
        vec![
            conversation_assistant_definition(),
            conversation_turn_tail_definition(),
        ],
        None,
        &[
            at(20, "turn/start", json!({"turn": 2})),
            at(21, "step/start", json!({"turn": 2, "step": 1})),
            at(
                22,
                "assistant/chunk",
                json!({
                    "turn": 2, "step": 1,
                    "chunk": {"type": "text-delta", "index": 0, "text": "partial"},
                }),
            ),
            at(23, "step/end", json!({"turn": 2, "step": 1})),
            at(
                24,
                "turn/end",
                json!({"turn": 2, "reason": {"kind": "completed"}}),
            ),
        ],
        false,
    );
    let interrupted_snapshot = snapshot(&interrupted);
    let assistant = node(&interrupted_snapshot, "assistant-step").unwrap();
    assert_eq!(assistant["data"]["status"], "interrupted");
    assert_eq!(assistant["anchorSeq"], 22.1);
    let tail = node(&interrupted_snapshot, "turn-tail").unwrap();
    assert_eq!(tail["anchorSeq"], 22.2);

    let interrupted_tool = assembler(
        vec![conversation_tool_definition()],
        None,
        &[
            at(30, "turn/start", json!({"turn": 3})),
            at(31, "step/start", json!({"turn": 3, "step": 1})),
            at(
                32,
                "tool/call",
                json!({
                    "turn": 3, "step": 1, "callId": "interrupted", "name": "read",
                    "arguments": "{}",
                }),
            ),
            at(33, "step/end", json!({"turn": 3, "step": 1})),
        ],
        false,
    );
    let tool_snapshot = snapshot(&interrupted_tool);
    let root = &node(&tool_snapshot, "tool-call").unwrap()["data"]["root"];
    assert_eq!(root["kind"], "tool-result");
    assert_eq!(root["seq"], 32.2);
    assert_eq!(
        root["error"],
        json!({"name": "Interrupted", "code": "interrupted"})
    );

    let tool_only = assembler(
        vec![conversation_assistant_definition()],
        None,
        &[
            at(40, "step/start", json!({"turn": 4, "step": 1})),
            surface(
                41,
                "assistant/message",
                json!({
                    "turn": 4, "step": 1,
                    "message": {
                        "id": "tool-only",
                        "content": [{"type": "tool-call", "id": "call", "name": "read", "arguments": "{}"}],
                    },
                }),
                json!("append"),
            ),
        ],
        false,
    );
    assert_eq!(
        node(&snapshot(&tool_only), "assistant-step").unwrap()["visibility"],
        "hidden"
    );
}

#[test]
fn production_chat_builder_encodes_callable_faces_timeline_and_legacy_inputs() {
    let value = production_chat_assembler(
        vec![
            conversation_assistant_definition(),
            conversation_turn_tail_definition(),
        ],
        &[
            at(1, "turn/start", json!({"turn": 1})),
            at(2, "step/start", json!({"turn": 1, "step": 1})),
            at(
                3,
                "assistant/chunk",
                json!({
                    "turn": 1, "step": 1,
                    "chunk": {"type": "text-delta", "index": 0, "text": "answer"},
                }),
            ),
            surface(
                4,
                "assistant/message",
                json!({
                    "turn": 1, "step": 1,
                    "message": {"id": "a", "content": [{"type": "text", "text": "answer"}]},
                    "usage": {"inputTokens": 2, "outputTokens": 1},
                }),
                json!("append"),
            ),
            at(5, "step/end", json!({"turn": 1, "step": 1})),
            at(
                6,
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
        ],
    );
    let snapshot = value.snapshot("chat").unwrap();
    assert_eq!(snapshot["encoding"], "seekdeep-chat-v1");
    assert_eq!(snapshot["order"].as_array().unwrap().len(), 2);
    assert_eq!(snapshot["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(snapshot["locations"]["turns"][0][0], 1);
    assert_eq!(snapshot["locations"]["steps"][0][0], 1);
    assert_eq!(snapshot["timeline"]["turnOrder"], json!([1]));
    assert_eq!(
        snapshot["timeline"]["turns"][0]["data"]["turn-tail"]["closing"]["finalNode"]["seq"],
        4
    );
    assert_eq!(snapshot["legacy"]["nodes"][0]["kind"], "assistant");
    assert_eq!(snapshot["legacy"]["turnTimings"][0][0], 1);
    assert_eq!(snapshot["legacy"]["turnEnds"][0], json!([1, 6]));
}

#[test]
fn partial_windows_reconstruct_assistant_and_nested_tool_without_start_events() {
    let value = assembler(
        vec![
            conversation_assistant_definition(),
            conversation_tool_definition(),
        ],
        None,
        &[
            at(
                1,
                "assistant/chunk",
                json!({
                    "turn": 1, "step": 1,
                    "chunk": {"type": "text-delta", "index": 0, "text": "loaded partial"},
                }),
            ),
            at(2, "step/end", json!({"turn": 1, "step": 1})),
            at(
                10,
                "tool/code-dispatch-start",
                json!({
                    "rootCallId": "history-root", "parentCallId": "history-root",
                    "subCallId": "child", "name": "read", "arguments": {"path": "README.md"},
                }),
            ),
            at(
                11,
                "tool/code-dispatch",
                json!({
                    "rootCallId": "history-root", "parentCallId": "history-root",
                    "subCallId": "child", "name": "read", "arguments": {"path": "README.md"},
                    "content": [{"type": "text", "text": "contents"}], "isError": false,
                }),
            ),
            surface(
                12,
                "tool/result",
                json!({
                    "turn": 2, "step": 1,
                    "message": {
                        "source": {"kind": "tool", "callId": "history-root"},
                        "content": [{
                            "type": "tool-result", "toolCallId": "history-root", "isError": false,
                            "content": [{"type": "text", "text": "root"}],
                        }],
                    },
                }),
                json!("append"),
            ),
        ],
        true,
    );
    let current = snapshot(&value);
    let assistant = node(&current, "assistant-step").unwrap();
    assert_eq!(assistant["data"]["status"], "interrupted");
    assert_eq!(assistant["data"]["blocks"][0]["text"], "loaded partial");
    let root = &node(&current, "tool-call").unwrap()["data"]["root"];
    assert_eq!(root["call"], Value::Null);
    assert_eq!(root["subCalls"][0]["callId"], "child");
    assert_eq!(root["subCalls"][0]["kind"], "tool-result");
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
