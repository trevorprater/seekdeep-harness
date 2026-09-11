//! Exact assistant and tool payloads through incremental conversation assembly.

use std::rc::Rc;

use indexmap::IndexMap;
use seekdeep_client_runtime::{
    AssemblerEventDefinitions, AssemblerNodeDefinition, AssemblerViewBuilder,
    AssemblerViewDefinition, AssemblerViewDefinitions, ConversationAssemblerError,
    ConversationEventInput, ConversationLocationEvent, ConversationNodeAssembler,
    ConversationTimelineSnapshot, ConversationValue as Value, ConversationViewNode,
    ConversationVisibility,
};
use seekdeep_client_ui_conversation::{
    conversation_assistant_definition, conversation_tool_definition,
};
use serde_json::json;

struct Definitions(Vec<Rc<AssemblerNodeDefinition>>);

impl AssemblerEventDefinitions for Definitions {
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
            target: "chat".to_owned(),
            create: Rc::new(|| Box::<Nodes>::default()),
        })]
    }
}

#[derive(Default)]
struct Nodes(IndexMap<String, Rc<ConversationViewNode>>);

impl Nodes {
    fn snapshot(&self) -> Rc<Value> {
        Rc::new(Value::array(
            &self
                .0
                .values()
                .map(|node| {
                    let chat = node.chat.as_ref().unwrap();
                    Value::object([
                        ("kind", json!(node.kind).into()),
                        ("data", node.data.as_ref().clone()),
                        (
                            "visible",
                            json!(chat.visibility == ConversationVisibility::Visible).into(),
                        ),
                    ])
                })
                .collect::<Vec<_>>(),
        ))
    }
}

impl AssemblerViewBuilder for Nodes {
    fn empty(&self) -> Rc<Value> {
        self.snapshot()
    }

    fn replace(
        &mut self,
        nodes: &[Rc<ConversationViewNode>],
        _timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        self.0 = nodes
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
            self.0.insert(node.key.clone(), node.clone());
        }
        Ok(self.snapshot())
    }
}

fn entry(seq: u64, kind: &str, data: &str) -> ConversationEventInput {
    let data = Value::parse(data.to_owned()).unwrap();
    let time = i64::try_from(seq).unwrap() * 10;
    let mut wire = vec![
        ("seq", json!(seq).into()),
        ("time", json!(time).into()),
        ("type", json!(kind).into()),
        ("data", data.clone()),
    ];
    if matches!(kind, "assistant/message" | "tool/result") {
        wire.push(("surfaceOp", json!("append").into()));
    }
    ConversationEventInput {
        event: ConversationLocationEvent::with_wire(seq, time, kind, data, Value::object(wire)),
        view: None,
    }
}

fn with_view(mut entry: ConversationEventInput, view: &str) -> ConversationEventInput {
    entry.view = Some(Rc::new(Value::parse(view.to_owned()).unwrap()));
    entry
}

fn assembler(
    definition: AssemblerNodeDefinition,
    entries: &[ConversationEventInput],
) -> ConversationNodeAssembler {
    let mut assembler = ConversationNodeAssembler::new(
        Rc::new(Definitions(vec![Rc::new(definition)])),
        Rc::new(Views),
    );
    assembler.replace_window(entries, false).unwrap();
    assembler.flush().unwrap();
    assembler
}

fn snapshot(assembler: &ConversationNodeAssembler) -> Rc<Value> {
    assembler.snapshot("chat").unwrap()
}

fn first_node(snapshot: &Value) -> &Value {
    &snapshot[0]
}

#[test]
fn assistant_stream_keeps_code_units_visibility_null_usage_and_old_snapshots() {
    let mut entries = vec![
        entry(1, "turn/start", r#"{"turn":1}"#),
        entry(2, "step/start", r#"{"turn":1,"step":1}"#),
        entry(
            3,
            "assistant/chunk",
            r#"{"turn":1,"step":1,"chunk":{"type":"text-delta","index":0,"text":"\ud800"}}"#,
        ),
    ];
    let mut live = assembler(conversation_assistant_definition(), &entries);
    let first = snapshot(&live);
    assert_eq!(first_node(&first)["visible"], true);
    assert_eq!(
        first_node(&first)["data"]["blocks"][0]["text"].to_utf16(),
        Some(vec![0xd800])
    );

    for next in [
        entry(
            4,
            "assistant/chunk",
            r#"{"turn":1,"step":1,"chunk":{"type":"usage","usage":null}}"#,
        ),
        entry(
            5,
            "assistant/chunk",
            r#"{"turn":1,"step":1,"chunk":{"type":"text-delta","index":0,"text":"\udfffB"}}"#,
        ),
        entry(
            6,
            "assistant/chunk",
            r#"{"turn":1,"step":1,"chunk":{"type":"reasoning-delta","index":1,"text":"\udfff"}}"#,
        ),
        entry(
            7,
            "assistant/chunk",
            r#"{"turn":1,"step":1,"chunk":{"type":"tool-call-delta","index":2,"id":"\ud800","name":"\udfff","argumentsDelta":"\ud800"}}"#,
        ),
        entry(
            8,
            "assistant/chunk",
            r#"{"turn":1,"step":1,"chunk":{"type":"tool-call-delta","index":2,"id":"ignored","name":null,"argumentsDelta":"\udfff"}}"#,
        ),
    ] {
        live.append(&next).unwrap();
        entries.push(next);
    }
    live.flush().unwrap();
    let current = snapshot(&live);
    let data = &first_node(&current)["data"];
    assert!(data.get_value("usage").is_some_and(Value::is_null));
    assert_eq!(
        data["blocks"][0]["text"].to_utf16(),
        Some(vec![0xd800, 0xdfff, 0x42])
    );
    assert_eq!(data["blocks"][1]["text"].to_utf16(), Some(vec![0xdfff]));
    assert_eq!(data["blocks"][2]["callId"].to_utf16(), Some(vec![0xd800]));
    assert_eq!(data["blocks"][2]["name"].to_utf16(), Some(vec![0xdfff]));
    assert_eq!(
        data["blocks"][2]["argsRaw"].to_utf16(),
        Some(vec![0xd800, 0xdfff])
    );
    assert_eq!(
        first_node(&first)["data"]["blocks"][0]["text"].to_utf16(),
        Some(vec![0xd800])
    );

    let end = entry(9, "step/end", r#"{"turn":1,"step":1}"#);
    live.append(&end).unwrap();
    entries.push(end);
    live.flush().unwrap();
    let interrupted = snapshot(&live);
    let data = &first_node(&interrupted)["data"];
    assert_eq!(data["status"], "interrupted");
    assert_eq!(
        data["finalNode"]["blocks"][2]["argsRaw"].to_utf16(),
        Some(vec![0xd800, 0xdfff])
    );
    assert_eq!(
        snapshot(&assembler(conversation_assistant_definition(), &entries)),
        interrupted
    );
}

#[test]
fn assistant_final_blocks_and_usage_retain_opaque_json_through_replay() {
    let settled = entry(
        4,
        "assistant/message",
        r#"{"turn":1,"step":1,"message":{"id":"final","content":[{"type":"text","text":"\ud800"},{"type":"reasoning","text":"\udfff"},{"type":"image","attachment":{"\ud800":"\udfff","huge":9007199254740993,"tiny":1e-400}},{"type":"future","payload":{"\udfff":"\ud800","order":[3,2,1]}}]},"usage":{"provider":{"\ud800":"\udfff"},"huge":9007199254740993,"tiny":1e-400}}"#,
    );
    let entries = vec![
        entry(1, "turn/start", r#"{"turn":1}"#),
        entry(2, "step/start", r#"{"turn":1,"step":1}"#),
        entry(
            3,
            "assistant/chunk",
            r#"{"turn":1,"step":1,"chunk":{"type":"block-end","index":0,"block":{"type":"future","payload":{"\udfff":"\ud800"}}}}"#,
        ),
        settled.clone(),
    ];
    let live = assembler(conversation_assistant_definition(), &entries);
    let current = snapshot(&live);
    let data = &first_node(&current)["data"];
    assert_eq!(data["status"], "settled");
    assert_eq!(data["blocks"][0]["text"].to_utf16(), Some(vec![0xd800]));
    assert_eq!(data["blocks"][1]["text"].to_utf16(), Some(vec![0xdfff]));
    for (actual, expected) in [
        (
            &data["blocks"][2]["attachment"],
            &settled.event.data["message"]["content"][2]["attachment"],
        ),
        (
            &data["blocks"][3]["block"],
            &settled.event.data["message"]["content"][3],
        ),
        (&data["usage"], &settled.event.data["usage"]),
        (&data["finalNode"]["usage"], &settled.event.data["usage"]),
    ] {
        assert_eq!(actual.as_raw(), expected.as_raw());
    }
    assert_eq!(
        snapshot(&assembler(conversation_assistant_definition(), &entries)),
        current
    );
}

#[test]
fn tool_dispatch_stringifies_arguments_and_retains_content_views_and_metadata() {
    let call = with_view(
        entry(
            3,
            "tool/call",
            r#"{"turn":1,"step":1,"callId":"root","name":"run\ud800","arguments":"A\ud800"}"#,
        ),
        r#"{"for":"call","view":{"card":"code","payload":{"\ud800":"\udfff","huge":9007199254740993}}}"#,
    );
    let dispatch = entry(
        4,
        "tool/code-dispatch-start",
        r#"{"rootCallId":"root","parentCallId":"root","subCallId":"child","name":"read\udfff","arguments":{"z":1,"10":"ten","2":"two","\ud800":"\udfff","number":9007199254740993,"tiny":1e-400}}"#,
    );
    let mut entries = vec![
        entry(1, "turn/start", r#"{"turn":1}"#),
        entry(2, "step/start", r#"{"turn":1,"step":1}"#),
        call.clone(),
        dispatch,
    ];
    let mut live = assembler(conversation_tool_definition(), &entries);
    let running = snapshot(&live);
    let root = &first_node(&running)["data"]["root"];
    assert_eq!(root["argsRaw"].to_utf16(), Some(vec![0x41, 0xd800]));
    let expected_arguments =
        r#"{"2":"two","10":"ten","z":1,"\ud800":"\udfff","number":9007199254740992,"tiny":0}"#;
    assert_eq!(root["subCalls"][0]["argsRaw"], expected_arguments);

    let child = entry(
        5,
        "tool/code-dispatch",
        r#"{"rootCallId":"root","parentCallId":"root","subCallId":"child","name":"read\udfff","arguments":{"z":1,"10":"ten","2":"two","\ud800":"\udfff","number":9007199254740993,"tiny":1e-400},"content":[{"type":"text","text":"\ud800"},{"type":"future","payload":{"\udfff":"\ud800","tiny":1e-400}}],"isError":false}"#,
    );
    let result = with_view(
        entry(
            6,
            "tool/result",
            r#"{"turn":1,"step":1,"message":{"source":{"kind":"tool","callId":"root"},"content":[{"type":"tool-result","isError":false,"content":[{"type":"text","text":"\udfff"}]}]},"error":{"message":"\ud800","detail":{"\udfff":"\ud800"}},"meta":{"\ud800":"\udfff","huge":9007199254740993,"tiny":1e-400}}"#,
        ),
        r#"{"for":"result","view":{"card":"done","payload":{"\udfff":"\ud800"}}}"#,
    );
    for next in [&child, &result] {
        live.append(next).unwrap();
        entries.push(next.clone());
    }
    live.flush().unwrap();
    let settled = snapshot(&live);
    let root = &first_node(&settled)["data"]["root"];
    assert_eq!(root["content"][0]["text"].to_utf16(), Some(vec![0xdfff]));
    assert_eq!(root["subCalls"][0]["call"]["argsRaw"], expected_arguments);
    assert_eq!(
        root["subCalls"][0]["content"].as_raw(),
        child.event.data["content"].as_raw()
    );
    assert_eq!(root["error"].as_raw(), result.event.data["error"].as_raw());
    assert_eq!(root["meta"].as_raw(), result.event.data["meta"].as_raw());
    assert_eq!(
        root["callView"].as_raw(),
        call.view.as_ref().unwrap()["view"].as_raw()
    );
    assert_eq!(
        root["resultView"].as_raw(),
        result.view.as_ref().unwrap()["view"].as_raw()
    );
    assert!(
        first_node(&running)["data"]["root"]
            .get_value("kind")
            .is_none()
    );
    assert_eq!(
        snapshot(&assembler(conversation_tool_definition(), &entries)),
        settled
    );
}
