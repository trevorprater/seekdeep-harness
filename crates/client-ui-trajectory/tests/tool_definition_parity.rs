//! Root Tool lifecycle, nested Code Dispatch, graph safety, and interruption parity.

use std::rc::Rc;

use indexmap::IndexMap;
use seekdeep_client_runtime::{
    AssemblerEventDefinitions, AssemblerNodeDefinition, AssemblerViewBuilder,
    AssemblerViewDefinition, AssemblerViewDefinitions, ConversationAssemblerError,
    ConversationEventInput, ConversationLocationEvent, ConversationNodeAssembler,
    ConversationTimelineSnapshot, ConversationViewNode,
};
use seekdeep_client_ui_trajectory::trajectory_tool_definition;
use serde_json::{Value, json};

struct Events(Rc<AssemblerNodeDefinition>);

impl AssemblerEventDefinitions for Events {
    fn entries(&self) -> Vec<Rc<AssemblerNodeDefinition>> {
        vec![self.0.clone()]
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
    at_view(seq, event_type, data, None)
}

fn at_view(seq: u64, event_type: &str, data: Value, view: Option<Value>) -> ConversationEventInput {
    ConversationEventInput {
        event: ConversationLocationEvent::with_time(
            seq,
            1_700_000_000_000_i64 + i64::try_from(seq).unwrap(),
            event_type,
            data,
        ),
        view: view.map(Rc::new),
    }
}

fn root_call(seq: u64) -> ConversationEventInput {
    at_view(
        seq,
        "tool/call",
        json!({
            "turn": 1,
            "step": 1,
            "callId": "root",
            "name": "code",
            "arguments": "{\"task\":\"read\"}",
        }),
        Some(json!({"for": "call", "view": {"kind": "root-call"}})),
    )
}

fn dispatch_start(seq: u64, parent: &str, child: &str) -> ConversationEventInput {
    at(
        seq,
        "tool/code-dispatch-start",
        json!({
            "rootCallId": "root",
            "parentCallId": parent,
            "subCallId": child,
            "name": "read",
            "arguments": {"path": format!("{child}.md")},
        }),
    )
}

fn dispatch_settle(seq: u64, parent: &str, child: &str) -> ConversationEventInput {
    at(
        seq,
        "tool/code-dispatch",
        json!({
            "rootCallId": "root",
            "parentCallId": parent,
            "subCallId": child,
            "name": "read",
            "arguments": {"path": format!("{child}.md")},
            "content": [{"type": "text", "text": format!("{child} contents")}],
            "isError": false,
        }),
    )
}

fn root_result(seq: u64) -> ConversationEventInput {
    at_view(
        seq,
        "tool/result",
        json!({
            "message": {
                "source": {"callId": "root"},
                "content": [{
                    "content": [{"type": "text", "text": "done"}],
                    "isError": false,
                }],
            },
            "meta": {"cwd": "/workspace"},
        }),
        Some(json!({"for": "result", "view": {"kind": "root-result"}})),
    )
}

fn assembler() -> ConversationNodeAssembler {
    ConversationNodeAssembler::new(
        Rc::new(Events(Rc::new(trajectory_tool_definition()))),
        Rc::new(Views),
    )
}

#[test]
fn settled_root_and_nested_dispatches_preserve_views_times_arguments_and_content() {
    let mut value = assembler();
    value
        .replace_window(
            &[
                at(1, "turn/start", json!({"turn": 1})),
                at(2, "step/start", json!({"turn": 1, "step": 1})),
                root_call(3),
                dispatch_start(4, "root", "child"),
                dispatch_start(5, "child", "grand"),
                dispatch_settle(6, "child", "grand"),
                dispatch_settle(7, "root", "child"),
                root_result(8),
                at(9, "step/end", json!({"turn": 1, "step": 1})),
            ],
            false,
        )
        .unwrap();
    value.flush().unwrap();
    let snapshot = value.snapshot("trajectory").unwrap();
    let root = &snapshot[0]["root"];
    assert_eq!(root["kind"], "tool-result");
    assert_eq!(root["seq"], 8);
    assert_eq!(root["call"]["name"], "code");
    assert_eq!(root["callTime"], 1_700_000_000_003_i64);
    assert_eq!(root["content"][0]["text"], "done");
    assert_eq!(root["callView"]["kind"], "root-call");
    assert_eq!(root["resultView"]["kind"], "root-result");
    assert_eq!(root["meta"]["cwd"], "/workspace");
    let child = &root["subCalls"][0];
    assert_eq!(child["kind"], "tool-result");
    assert_eq!(child["callTime"], 1_700_000_000_004_i64);
    assert_eq!(child["call"]["argsRaw"], "{\"path\":\"child.md\"}");
    assert_eq!(child["content"][0]["text"], "child contents");
    let grand = &child["subCalls"][0];
    assert_eq!(grand["callTime"], 1_700_000_000_005_i64);
    assert_eq!(grand["content"][0]["text"], "grand contents");
}

#[test]
fn closing_step_interrupts_running_root_and_children_at_fractional_boundary() {
    let mut value = assembler();
    value
        .replace_window(
            &[
                at(1, "turn/start", json!({"turn": 1})),
                at(2, "step/start", json!({"turn": 1, "step": 1})),
                root_call(3),
                dispatch_start(4, "root", "child"),
                at(5, "step/end", json!({"turn": 1, "step": 1})),
            ],
            false,
        )
        .unwrap();
    value.flush().unwrap();
    let snapshot = value.snapshot("trajectory").unwrap();
    let root = &snapshot[0]["root"];
    assert_eq!(root["kind"], "tool-result");
    assert_eq!(root["seq"], json!(4.2));
    assert_eq!(
        root["error"],
        json!({"name": "Interrupted", "code": "interrupted"})
    );
    assert_eq!(root["callView"]["kind"], "root-call");
    assert_eq!(root["subCalls"][0]["seq"], json!(4.2));
    assert_eq!(root["subCalls"][0]["callTime"], 1_700_000_000_004_i64);
}

#[test]
fn update_only_window_falls_back_to_result_root_and_retains_nested_settlement() {
    let mut value = assembler();
    value
        .replace_window(
            &[root_result(10), dispatch_settle(11, "root", "child")],
            true,
        )
        .unwrap();
    value.flush().unwrap();
    let snapshot = value.snapshot("trajectory").unwrap();
    let root = &snapshot[0]["root"];
    assert_eq!(root["call"], Value::Null);
    assert_eq!(root["callTime"], Value::Null);
    assert_eq!(root["subCalls"][0]["callId"], "child");
    assert_eq!(root["subCalls"][0]["callTime"], Value::Null);
}

#[test]
fn graph_rejects_cycles_second_parents_self_edges_and_depth_overflow() {
    let mut events = vec![root_call(1)];
    let mut parent = "root".to_owned();
    for depth in 1..=256_u64 {
        let child = format!("call-{depth}");
        events.push(dispatch_start(depth + 1, &parent, &child));
        parent = child;
    }
    events.push(dispatch_start(258, "call-255", "root"));
    events.push(dispatch_start(259, "root", "call-10"));
    events.push(dispatch_start(260, "root", "root"));
    let mut value = assembler();
    value.replace_window(&events, false).unwrap();
    value.flush().unwrap();
    let snapshot = value.snapshot("trajectory").unwrap();
    let mut current = &snapshot[0]["root"];
    let mut depth = 1_usize;
    while let Some(child) = current["subCalls"]
        .as_array()
        .and_then(|children| children.first())
    {
        current = child;
        depth += 1;
    }
    assert_eq!(depth, 256);
    assert_eq!(current["callId"], "call-255");
}
