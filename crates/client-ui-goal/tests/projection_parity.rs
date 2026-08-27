//! Goal command text, Conversation projection, metadata, and locale parity.

use std::rc::Rc;

use seekdeep_client_runtime::{
    AssemblerEventDefinitions, AssemblerNodeDefinition, AssemblerViewBuilder,
    AssemblerViewDefinition, AssemblerViewDefinitions, ConversationAssemblerError,
    ConversationEventInput, ConversationLocation, ConversationLocationEvent,
    ConversationNodeAssembler, ConversationTimelineSnapshot, ConversationViewNode,
    ConversationVisibility,
};
use seekdeep_client_ui_goal::{
    GOAL_LOCALES, GOAL_NS, GoalCommandInputData, goal_command_input_definition, goal_command_text,
};
use serde_json::{Map, Value, json};

struct Events(Vec<Rc<AssemblerNodeDefinition>>);

impl AssemblerEventDefinitions for Events {
    fn entries(&self) -> Vec<Rc<AssemblerNodeDefinition>> {
        self.0.clone()
    }

    fn fallback_entry(&self) -> Option<Rc<AssemblerNodeDefinition>> {
        None
    }
}

struct Views(Vec<Rc<AssemblerViewDefinition>>);

impl AssemblerViewDefinitions for Views {
    fn entries(&self) -> Vec<Rc<AssemblerViewDefinition>> {
        self.0.clone()
    }
}

struct Builder {
    nodes: Vec<Rc<ConversationViewNode>>,
}

impl Builder {
    fn snapshot(&self) -> Rc<Value> {
        let nodes = self
            .nodes
            .iter()
            .map(|node| {
                let chat = node.chat.as_ref().unwrap();
                (
                    node.key.clone(),
                    json!({
                        "kind":node.kind,
                        "anchorSeq":chat.anchor_seq,
                        "visible":chat.visibility == ConversationVisibility::Visible,
                        "location":match &chat.location {
                            ConversationLocation::Session => "session",
                            ConversationLocation::Turn { .. } => "turn",
                            ConversationLocation::Step { .. } => "step",
                            ConversationLocation::Unresolved => "unresolved",
                        },
                        "data":node.data.as_ref().clone(),
                    }),
                )
            })
            .collect::<Map<_, _>>();
        Rc::new(json!({"nodes":nodes}))
    }
}

impl AssemblerViewBuilder for Builder {
    fn empty(&self) -> Rc<Value> {
        self.snapshot()
    }

    fn replace(
        &mut self,
        nodes: &[Rc<ConversationViewNode>],
        _timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        self.nodes = nodes.to_vec();
        Ok(self.snapshot())
    }

    fn apply(
        &mut self,
        upserts: &[Rc<ConversationViewNode>],
        _timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<Value>, ConversationAssemblerError> {
        for upsert in upserts {
            if let Some(current) = self.nodes.iter_mut().find(|node| node.key == upsert.key) {
                *current = upsert.clone();
            } else {
                self.nodes.push(upsert.clone());
            }
        }
        Ok(self.snapshot())
    }
}

fn at(seq: u64, event_type: &str, data: Value) -> ConversationEventInput {
    ConversationEventInput {
        event: ConversationLocationEvent::with_time(
            seq,
            1_700_000_000_000 + i64::try_from(seq).unwrap(),
            event_type,
            data,
        ),
        view: None,
    }
}

fn snapshot(entries: &[ConversationEventInput], has_more: bool) -> Rc<Value> {
    let mut assembler = ConversationNodeAssembler::new(
        Rc::new(Events(vec![Rc::new(goal_command_input_definition())])),
        Rc::new(Views(vec![Rc::new(AssemblerViewDefinition {
            target: "chat".to_owned(),
            create: Rc::new(|| Box::new(Builder { nodes: Vec::new() })),
        })])),
    );
    assembler.replace_window(entries, has_more).unwrap();
    assembler.flush().unwrap();
    assembler.snapshot("chat").unwrap()
}

#[test]
fn projection_builds_separate_goal_input_with_exact_anchor_location_and_data() {
    let run = at(
        1,
        "command/run",
        json!({"commandId":"command-goal","name":"goal","args":" ","source":{"kind":"user"}}),
    );
    let value = snapshot(&[run], false);
    let node = value["nodes"].as_object().unwrap().values().next().unwrap();
    assert_eq!(node["kind"], "command-input");
    assert_eq!(node["anchorSeq"], 0.9);
    assert_eq!(node["location"], "session");
    assert_eq!(node["visible"], true);
    let data: GoalCommandInputData = serde_json::from_value(node["data"].clone()).unwrap();
    assert_eq!(data.command_id.as_str(), "command-goal");
    assert_eq!(data.text, "/goal");
    assert_eq!(data.time, 1_700_000_000_001);

    let plan = at(
        2,
        "command/run",
        json!({"commandId":"command-plan","name":"plan","args":""}),
    );
    assert!(
        snapshot(&[plan], false)["nodes"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    let done = at(
        3,
        "command/done",
        json!({"commandId":"command-goal","kind":"success"}),
    );
    assert!(
        snapshot(&[done], true)["nodes"]
            .as_object()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn command_text_and_locales_preserve_internal_lines_and_trailing_trim() {
    assert_eq!(goal_command_text("goal", None), "/goal");
    assert_eq!(
        goal_command_text("goal", Some("\nfirst line\nsecond line \n\u{feff}")),
        "/goal\nfirst line\nsecond line"
    );
    assert_eq!(GOAL_NS, "goal");
    assert_eq!(GOAL_LOCALES.len(), 11);
    assert_eq!(
        GOAL_LOCALES[0],
        ("phase.active", "进行中的目标", "Ongoing Goal")
    );
    assert_eq!(GOAL_LOCALES[10], ("action.clear", "清除目标", "Clear goal"));
}
