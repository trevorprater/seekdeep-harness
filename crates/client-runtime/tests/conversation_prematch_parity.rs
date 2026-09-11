//! A window replacement takes whole-window match answers where the registry supplies them.

#![cfg(not(target_arch = "wasm32"))]

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use seekdeep_client_runtime::ConversationValue as Value;
use seekdeep_client_runtime::{
    AssemblerEventDefinitions, AssemblerNodeDefinition, AssemblerViewDefinition,
    AssemblerViewDefinitions, ConversationAssemblerError, ConversationEventInput,
    ConversationLocationEvent, ConversationMatch, ConversationMatchResult, ConversationMatchRole,
    ConversationNodeAssembler, ConversationNodeContext, PrematchTable,
};

include!("support/conversation_json.rs");

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
            Ok(Some(Rc::new(
                conversation_json!({"seq": accepted.event.seq}),
            )))
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
                at(1, "tool/call", conversation_json!({"callId": "a"})),
                at(2, "note", conversation_json!({})),
                at(3, "tool/call", conversation_json!({"callId": "b"})),
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
        .append(&at(4, "tool/call", conversation_json!({"callId": "c"})))
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

/// A registry that folds `tool` update runs in one call, the way a Definition module does:
/// each Match is folded against the collection prefix and state before it.
struct BatchingRegistry {
    tool: Rc<AssemblerNodeDefinition>,
    note: Rc<AssemblerNodeDefinition>,
    batches: Rc<RefCell<Vec<usize>>>,
}

impl AssemblerEventDefinitions for BatchingRegistry {
    fn entries(&self) -> Vec<Rc<AssemblerNodeDefinition>> {
        vec![self.tool.clone(), self.note.clone()]
    }

    fn fallback_entry(&self) -> Option<Rc<AssemblerNodeDefinition>> {
        None
    }

    fn batches_updates(&self, definition: &Rc<AssemblerNodeDefinition>) -> bool {
        Rc::ptr_eq(definition, &self.tool)
    }

    fn update_many(
        &self,
        definition: &Rc<AssemblerNodeDefinition>,
        context: &ConversationNodeContext,
        batch: &[Rc<ConversationMatch>],
    ) -> Result<Option<Rc<Value>>, ConversationAssemblerError> {
        self.batches.borrow_mut().push(batch.len());
        let mirror = context.matches.clone();
        let prefix = mirror.borrow().len() - batch.len();
        let tail = mirror.borrow_mut().split_off(prefix);
        let mut state = context.state.clone();
        for accepted in tail {
            mirror.borrow_mut().push(accepted.clone());
            let step = ConversationNodeContext {
                key: context.key.clone(),
                kind: context.kind.clone(),
                id: context.id.clone(),
                matches: mirror.clone(),
                start: context.start.clone(),
                state: state.clone(),
                current: context.current.clone(),
            };
            state = (definition.update)(&step, &accepted)?;
        }
        Ok(state)
    }
}

/// `tool` counts its updates and records how many Matches it saw at each step; `note`
/// reads the previous `tool` state when it starts.
fn counting_tool(seen: Rc<RefCell<Vec<usize>>>) -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: "tool".to_owned(),
        target: None,
        match_event: Rc::new(|event| {
            Ok(match event.event_type.as_str() {
                "tool/call" => Some(ConversationMatchResult {
                    id: "t".to_owned(),
                    role: ConversationMatchRole::Start,
                }),
                "tool/chunk" => Some(ConversationMatchResult {
                    id: "t".to_owned(),
                    role: ConversationMatchRole::Update,
                }),
                _ => None,
            })
        }),
        start: Rc::new(|_context, _accepted, _reader| {
            Ok(Some(Rc::new(conversation_json!({"count": 0}))))
        }),
        update: Rc::new(move |context, _accepted| {
            seen.borrow_mut().push(context.matches.borrow().len());
            let count = context.state.as_ref().unwrap()["count"].as_u64().unwrap() + 1;
            Ok(Some(Rc::new(conversation_json!({"count": count}))))
        }),
        publication: None,
        build_location_data: None,
        build_view_node: None,
    }
}

fn reading_note(observed: Rc<RefCell<Vec<Value>>>) -> AssemblerNodeDefinition {
    AssemblerNodeDefinition {
        kind: "note".to_owned(),
        target: None,
        match_event: Rc::new(|event| {
            Ok(
                (event.event_type == "note").then(|| ConversationMatchResult {
                    id: event.seq.to_string(),
                    role: ConversationMatchRole::Start,
                }),
            )
        }),
        start: Rc::new(move |_context, _accepted, reader| {
            let previous = reader
                .peek_previous("tool")
                .map_or(Value::from(serde_json::Value::Null), |context| {
                    context.state.as_ref().clone()
                });
            observed.borrow_mut().push(previous);
            Ok(Some(Rc::new(conversation_json!({}))))
        }),
        update: Rc::new(|_context, _accepted| Ok(None)),
        publication: None,
        build_location_data: None,
        build_view_node: None,
    }
}

#[test]
fn update_runs_fold_in_one_call_and_flush_before_a_start_that_reads_them() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let observed = Rc::new(RefCell::new(Vec::new()));
    let batches = Rc::new(RefCell::new(Vec::new()));
    let tool = Rc::new(counting_tool(seen.clone()));
    let mut assembler = ConversationNodeAssembler::new(
        Rc::new(BatchingRegistry {
            tool: tool.clone(),
            note: Rc::new(reading_note(observed.clone())),
            batches: batches.clone(),
        }),
        Rc::new(NoViews),
    );
    assembler
        .replace_window(
            &[
                at(1, "tool/call", conversation_json!({})),
                at(2, "tool/chunk", conversation_json!({})),
                at(3, "tool/chunk", conversation_json!({})),
                at(4, "tool/chunk", conversation_json!({})),
                at(5, "note", conversation_json!({})),
                at(6, "tool/chunk", conversation_json!({})),
                at(7, "tool/chunk", conversation_json!({})),
                at(8, "note", conversation_json!({})),
            ],
            false,
        )
        .unwrap();
    assert_eq!(*batches.borrow(), vec![3, 2], "one fold per run of updates");
    assert_eq!(
        *seen.borrow(),
        vec![2, 3, 4, 5, 6],
        "each step saw the collection as it stood before it"
    );
    assert_eq!(
        *observed.borrow(),
        vec![
            conversation_json!({"count": 3}),
            conversation_json!({"count": 5})
        ],
        "each start read the state folded before it"
    );

    assembler
        .append(&at(9, "tool/chunk", conversation_json!({})))
        .unwrap();
    assert_eq!(
        *batches.borrow(),
        vec![3, 2],
        "the live tail folds per Match"
    );
    assert_eq!(seen.borrow().last(), Some(&7));
}

#[test]
fn a_sliced_replacement_publishes_nothing_until_its_last_slice() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let observed = Rc::new(RefCell::new(Vec::new()));
    let batches = Rc::new(RefCell::new(Vec::new()));
    let mut assembler = ConversationNodeAssembler::new(
        Rc::new(BatchingRegistry {
            tool: Rc::new(counting_tool(seen.clone())),
            note: Rc::new(reading_note(observed.clone())),
            batches: batches.clone(),
        }),
        Rc::new(NoViews),
    );
    assembler
        .begin_replace_window(
            &[
                at(1, "tool/call", conversation_json!({})),
                at(2, "tool/chunk", conversation_json!({})),
                at(3, "tool/chunk", conversation_json!({})),
                at(4, "note", conversation_json!({})),
                at(5, "tool/chunk", conversation_json!({})),
            ],
            false,
        )
        .unwrap();
    assert!(!assembler.continue_replace_window(2).unwrap());
    assert!(
        !assembler.flush().unwrap(),
        "a replacement in progress publishes nothing"
    );
    assert!(!assembler.continue_replace_window(2).unwrap());
    assert_eq!(
        *observed.borrow(),
        vec![conversation_json!({"count": 2})],
        "the run before the start folded before it, across the slice boundary"
    );
    assert!(assembler.continue_replace_window(2).unwrap());
    assert!(
        assembler.continue_replace_window(2).unwrap(),
        "complete stays complete"
    );
    assert_eq!(*batches.borrow(), vec![2, 1]);
    assert_eq!(
        *seen.borrow(),
        vec![2, 3, 4],
        "the note at seq 4 is not a tool Match, so the last fold saw four"
    );
    assert!(assembler.flush().unwrap(), "the finished window publishes");

    // A mutation while a replacement is in progress completes it first.
    assembler
        .begin_replace_window(
            &[
                at(1, "tool/call", conversation_json!({})),
                at(2, "tool/chunk", conversation_json!({})),
                at(3, "tool/chunk", conversation_json!({})),
            ],
            false,
        )
        .unwrap();
    assert!(!assembler.continue_replace_window(1).unwrap());
    assembler
        .append(&at(4, "tool/chunk", conversation_json!({})))
        .unwrap();
    assert_eq!(*batches.borrow(), vec![2, 1, 2]);
    assert_eq!(seen.borrow().last(), Some(&4));
}
