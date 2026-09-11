//! A window replacement takes whole-window match answers where the registry supplies them.

#![cfg(not(target_arch = "wasm32"))]

use std::{cell::Cell, rc::Rc};

use seekdeep_client_runtime::{
    AssemblerEventDefinitions, AssemblerNodeDefinition, AssemblerViewDefinition,
    AssemblerViewDefinitions, ConversationAssemblerError, ConversationEventInput,
    ConversationLocationEvent, ConversationMatchResult, ConversationMatchRole,
    ConversationNodeAssembler, PrematchTable,
};
use serde_json::{Value, json};

struct NoViews;

impl AssemblerViewDefinitions for NoViews {
    fn entries(&self) -> Vec<Rc<AssemblerViewDefinition>> {
        Vec::new()
    }
}

/// Answers for the `tool` Definition come from the table; the `note` Definition is left to
/// its own matcher, as a Definition without a batch face would be.
struct Registry {
    tool: Rc<AssemblerNodeDefinition>,
    note: Rc<AssemblerNodeDefinition>,
    prematched_events: Rc<Cell<u64>>,
}

impl AssemblerEventDefinitions for Registry {
    fn entries(&self) -> Vec<Rc<AssemblerNodeDefinition>> {
        vec![self.tool.clone(), self.note.clone()]
    }

    fn fallback_entry(&self) -> Option<Rc<AssemblerNodeDefinition>> {
        None
    }

    fn prematch(
        &self,
        events: &[Rc<ConversationLocationEvent>],
    ) -> Result<Option<PrematchTable>, ConversationAssemblerError> {
        let mut table = PrematchTable::default();
        for event in events {
            table.insert(&self.tool, event.seq, tool_answer(event));
        }
        self.prematched_events
            .set(self.prematched_events.get() + u64::try_from(events.len()).unwrap());
        Ok(Some(table))
    }
}

fn tool_answer(event: &ConversationLocationEvent) -> Option<ConversationMatchResult> {
    (event.event_type == "tool/call").then(|| ConversationMatchResult {
        id: event.data["callId"].as_str().unwrap().to_owned(),
        role: ConversationMatchRole::Start,
    })
}

fn definition(kind: &str, asked: Rc<Cell<u64>>, starts: Rc<Cell<u64>>) -> AssemblerNodeDefinition {
    let matches_kind = kind.to_owned();
    AssemblerNodeDefinition {
        kind: kind.to_owned(),
        target: None,
        match_event: Rc::new(move |event| {
            asked.set(asked.get() + 1);
            Ok(match matches_kind.as_str() {
                "tool" => tool_answer(event),
                _ => (event.event_type == "note").then(|| ConversationMatchResult {
                    id: event.seq.to_string(),
                    role: ConversationMatchRole::Start,
                }),
            })
        }),
        start: Rc::new(move |_context, accepted, _reader| {
            starts.set(starts.get() + 1);
            Ok(Some(Rc::new(json!({"seq": accepted.event.seq}))))
        }),
        update: Rc::new(|_context, _accepted| Ok(None)),
        publication: None,
        build_location_data: None,
        build_view_node: None,
    }
}

fn at(seq: u64, event_type: &str, data: Value) -> ConversationEventInput {
    ConversationEventInput {
        event: ConversationLocationEvent::new(seq, event_type, data),
        view: None,
    }
}

#[test]
fn replace_window_uses_the_table_and_the_live_tail_asks_per_event_again() {
    let tool_asked = Rc::new(Cell::new(0));
    let tool_starts = Rc::new(Cell::new(0));
    let note_asked = Rc::new(Cell::new(0));
    let note_starts = Rc::new(Cell::new(0));
    let prematched_events = Rc::new(Cell::new(0));
    let mut assembler = ConversationNodeAssembler::new(
        Rc::new(Registry {
            tool: Rc::new(definition("tool", tool_asked.clone(), tool_starts.clone())),
            note: Rc::new(definition("note", note_asked.clone(), note_starts.clone())),
            prematched_events: prematched_events.clone(),
        }),
        Rc::new(NoViews),
    );
    assembler
        .replace_window(
            &[
                at(1, "tool/call", json!({"callId": "a"})),
                at(2, "note", json!({})),
                at(3, "tool/call", json!({"callId": "b"})),
            ],
            false,
        )
        .unwrap();
    assert_eq!(prematched_events.get(), 3, "the window was matched once");
    assert_eq!(
        tool_asked.get(),
        0,
        "a tabled Definition is not asked per event"
    );
    assert_eq!(
        note_asked.get(),
        3,
        "an untabled Definition keeps its matcher"
    );
    assert_eq!((tool_starts.get(), note_starts.get()), (2, 1));

    assembler
        .append(&at(4, "tool/call", json!({"callId": "c"})))
        .unwrap();
    assert_eq!(
        prematched_events.get(),
        3,
        "a live tail event is not prematched"
    );
    assert_eq!(
        tool_asked.get(),
        1,
        "the table is gone once the replacement settled"
    );
    assert_eq!(note_asked.get(), 4);
    assert_eq!(tool_starts.get(), 3);
}
