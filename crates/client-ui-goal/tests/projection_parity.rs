//! Goal command text, Conversation projection, metadata, and locale parity.

use std::rc::Rc;

use seekdeep_client_runtime::ConversationValue as SnapshotValue;
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
use serde_json::{Value, json};

include!("../../client-runtime/tests/support/conversation_json.rs");

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
    fn snapshot(&self) -> Rc<SnapshotValue> {
        let nodes = self
            .nodes
            .iter()
            .map(|node| {
                let chat = node.chat.as_ref().unwrap();
                (
                    node.key.clone(),
                    conversation_json!({
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
            .collect::<Vec<_>>();
        Rc::new(conversation_json!({"nodes":SnapshotValue::object(nodes)}))
    }
}

impl AssemblerViewBuilder for Builder {
    fn empty(&self) -> Rc<SnapshotValue> {
        self.snapshot()
    }

    fn replace(
        &mut self,
        nodes: &[Rc<ConversationViewNode>],
        _timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<SnapshotValue>, ConversationAssemblerError> {
        self.nodes = nodes.to_vec();
        Ok(self.snapshot())
    }

    fn apply(
        &mut self,
        upserts: &[Rc<ConversationViewNode>],
        _timeline: Rc<ConversationTimelineSnapshot>,
    ) -> Result<Rc<SnapshotValue>, ConversationAssemblerError> {
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

fn snapshot(entries: &[ConversationEventInput], has_more: bool) -> Rc<SnapshotValue> {
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
    let node = value["nodes"].object_entries().unwrap()[0].1.to_owned();
    assert_eq!(node["kind"], "command-input");
    assert_eq!(node["anchorSeq"], 0.9);
    assert_eq!(node["location"], "session");
    assert_eq!(node["visible"], true);
    let data: GoalCommandInputData = node["data"].deserialize().unwrap();
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
            .object_entries()
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
            .object_entries()
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

#[test]
fn command_projection_preserves_raw_argument_code_units() {
    let run = ConversationEventInput {
        event: ConversationLocationEvent::with_time(
            1,
            1,
            "command/run",
            SnapshotValue::parse(
                r#"{"commandId":"raw","name":"goal","args":" \ud800x\udfff \n"}"#.to_owned(),
            )
            .unwrap(),
        ),
        view: None,
    };
    let value = snapshot(&[run], false);
    let node = value["nodes"].object_entries().unwrap()[0].1.to_owned();
    assert_eq!(
        node["data"]["text"].to_utf16(),
        Some(vec![47, 103, 111, 97, 108, 32, 0xd800, 120, 0xdfff])
    );
}
