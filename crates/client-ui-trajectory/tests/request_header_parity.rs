//! Request-header Definition, prompt inheritance, and placement parity.

use std::rc::Rc;

use indexmap::IndexMap;
use seekdeep_client_runtime::{
    AssemblerEventDefinitions, AssemblerNodeDefinition, AssemblerViewBuilder,
    AssemblerViewDefinition, AssemblerViewDefinitions, ConversationAssemblerError,
    ConversationEventInput, ConversationLocation, ConversationLocationEvent,
    ConversationNodeAssembler, ConversationTimelineSnapshot, ConversationViewNode,
};
use seekdeep_client_ui_trajectory::trajectory_request_header_definition;
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
            create: Rc::new(|| Box::new(CaptureBuilder::default())),
        })]
    }
}

#[derive(Default)]
struct CaptureBuilder {
    nodes: IndexMap<String, Rc<ConversationViewNode>>,
}

impl CaptureBuilder {
    fn snapshot(&self) -> Rc<Value> {
        Rc::new(Value::Array(
            self.nodes
                .values()
                .map(|node| {
                    let placement = node.placement.as_ref().expect("trajectory placement");
                    json!({
                        "key": node.key,
                        "kind": node.kind,
                        "id": node.id,
                        "target": node.target,
                        "anchorSeq": placement.anchor_seq,
                        "location": location_value(&placement.location),
                        "data": node.data.as_ref().clone(),
                    })
                })
                .collect(),
        ))
    }
}

impl AssemblerViewBuilder for CaptureBuilder {
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

fn location_value(location: &ConversationLocation) -> Value {
    match location {
        ConversationLocation::Session => json!({"kind": "session"}),
        ConversationLocation::Turn { turn } => {
            json!({"kind": "turn", "turn": {"turn": turn.turn}})
        }
        ConversationLocation::Step { turn, step } => json!({
            "kind": "step",
            "turn": {"turn": turn.turn},
            "step": {"step": step.step},
        }),
        ConversationLocation::Unresolved => json!({"kind": "unresolved"}),
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

fn header(reason: &str, system: &Value, tools: &Value) -> Value {
    json!({
        "reason": reason,
        "header": {
            "config": {"provider": "test", "model": "model"},
            "system": system,
            "tools": tools,
        },
    })
}

fn assembler() -> ConversationNodeAssembler {
    ConversationNodeAssembler::new(
        Rc::new(Events(vec![
            Rc::new(trajectory_request_header_definition()),
        ])),
        Rc::new(Views),
    )
}

#[test]
fn headers_inherit_previous_prompt_and_publish_exact_step_placement_and_changes() {
    let mut value = assembler();
    value
        .replace_window(
            &[
                at(1, "turn/start", json!({"turn": 1})),
                at(2, "step/start", json!({"turn": 1, "step": 1})),
                at(
                    3,
                    "request/header",
                    header("initial", &json!("system-a"), &json!([{"name": "bash"}])),
                ),
                at(4, "step/end", json!({"turn": 1, "step": 1})),
                at(5, "step/start", json!({"turn": 1, "step": 2})),
                at(
                    6,
                    "request/header",
                    header("retry", &json!("system-a"), &json!([{"name": "bash"}])),
                ),
                at(7, "step/end", json!({"turn": 1, "step": 2})),
                at(8, "step/start", json!({"turn": 1, "step": 3})),
                at(
                    9,
                    "request/header",
                    header("retry", &json!("system-b"), &json!([{"name": "read"}])),
                ),
            ],
            false,
        )
        .unwrap();
    value.flush().unwrap();
    let snapshot = value.snapshot("trajectory").unwrap();
    let rows = snapshot.as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["anchorSeq"], json!(3.0));
    assert_eq!(rows[0]["location"]["kind"], "step");
    assert_eq!(rows[0]["location"]["step"]["step"], 1);
    assert_eq!(rows[0]["data"]["header"]["location"]["step"]["step"], 1);
    assert_eq!(rows[0]["data"]["header"]["change"]["kind"], "initial");
    assert!(rows[1]["data"]["header"].get("change").is_none());
    assert_eq!(rows[1]["data"]["header"]["location"]["step"]["step"], 2);
    assert_eq!(
        rows[2]["data"]["header"]["change"]["kind"],
        "system-and-tools"
    );
    assert_eq!(
        rows[2]["data"]["header"]["change"]["previous"]["system"],
        "system-a"
    );
    assert_eq!(rows[2]["data"]["header"]["prompt"]["system"], "system-b");
}

#[test]
fn truncated_noninitial_header_does_not_invent_change_and_normalizes_nullable_fields() {
    let mut value = assembler();
    value
        .replace_window(
            &[at(
                10,
                "request/header",
                header("retry", &Value::Null, &json!("not-an-array")),
            )],
            true,
        )
        .unwrap();
    value.flush().unwrap();
    let snapshot = value.snapshot("trajectory").unwrap();
    let header = &snapshot[0]["data"]["header"];
    assert_eq!(header["prompt"]["system"], "");
    assert_eq!(header["prompt"]["tools"], json!([]));
    assert!(header.get("change").is_none());
    assert_eq!(header["location"]["kind"], "session");
}
