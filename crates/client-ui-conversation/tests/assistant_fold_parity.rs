//! Folded Assistant chunk runs publish exactly what separate updates publish.

use std::cell::Cell;
use std::rc::Rc;

use seekdeep_client_runtime::{
    AssemblerEventDefinitions, AssemblerNodeDefinition, AssemblerViewDefinition,
    AssemblerViewDefinitions, ConversationAssemblerError, ConversationEventInput,
    ConversationLocationEvent, ConversationMatch, ConversationNodeAssembler,
    ConversationValue as RawValue,
};

/// The production Chat view assembles the nodes this fold publishes.
struct Views;

impl AssemblerViewDefinitions for Views {
    fn entries(&self) -> Vec<Rc<AssemblerViewDefinition>> {
        vec![Rc::new(conversation_chat_view_definition())]
    }
}
use seekdeep_client_ui_conversation::{
    ASSISTANT_STEP_KIND, conversation_assistant_definition, conversation_chat_view_definition,
};
use serde_json::{Value, json};

/// Serves this Definition's runs as one batch only when the test asks it to fold.
struct FoldableEvents {
    folds: bool,
    folded: Rc<Cell<usize>>,
}

impl AssemblerEventDefinitions for FoldableEvents {
    fn entries(&self) -> Vec<Rc<AssemblerNodeDefinition>> {
        vec![Rc::new(conversation_assistant_definition())]
    }

    fn fallback_entry(&self) -> Option<Rc<AssemblerNodeDefinition>> {
        None
    }

    fn batches_updates(&self, definition: &Rc<AssemblerNodeDefinition>) -> bool {
        self.folds && definition.kind == ASSISTANT_STEP_KIND
    }

    fn update_many(
        &self,
        _definition: &Rc<AssemblerNodeDefinition>,
        context: &seekdeep_client_runtime::ConversationNodeContext,
        batch: &[Rc<ConversationMatch>],
    ) -> Result<Option<Rc<RawValue>>, ConversationAssemblerError> {
        self.folded.set(self.folded.get() + 1);
        seekdeep_client_ui_conversation::update_assistant_batch(context, batch)
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

fn chunk(seq: u64, kind: &str, index: u64, payload: &Value) -> ConversationEventInput {
    let mut body = json!({"type": kind, "index": index});
    if let Some(fields) = payload.as_object() {
        for (key, value) in fields {
            body[key] = value.clone();
        }
    }
    at(
        seq,
        "assistant/chunk",
        json!({"turn": 1, "step": 1, "chunk": body}),
    )
}

fn assistant_message(seq: u64, text: &str) -> ConversationEventInput {
    at(
        seq,
        "assistant/message",
        json!({
            "turn": 1,
            "step": 1,
            "message": {
                "id": "message-1",
                "content": [{"type": "text", "text": text}],
            },
            "usage": {"inputTokens": 3, "outputTokens": 4},
        }),
    )
}

/// One step whose blocks every kind of update touches, including wholesale replacements.
fn streaming_window() -> Vec<ConversationEventInput> {
    let mut window = vec![at(1, "step/start", json!({"turn": 1, "step": 1}))];
    let mut seq = 2;
    let push = |window: &mut Vec<ConversationEventInput>, event: ConversationEventInput| {
        window.push(event);
    };
    // A leading Tool call run leaves nothing visible, so the fold still owns every block while
    // the state is hidden.
    for index in 0..4 {
        let delta = format!("{{\"path\":\"file{index}.rs\"}}");
        push(
            &mut window,
            chunk(
                seq,
                "tool-call-delta",
                2,
                &json!({"id": "call-1", "name": "read", "argumentsDelta": delta}),
            ),
        );
        seq += 1;
    }
    push(
        &mut window,
        chunk(seq, "block-start", 2, &json!({"blockType": "tool-call"})),
    );
    seq += 1;
    for index in 0..5 {
        push(
            &mut window,
            chunk(
                seq,
                "reasoning-delta",
                1,
                &json!({"text": format!("thinking step {index}\n")}),
            ),
        );
        seq += 1;
        push(
            &mut window,
            chunk(
                seq,
                "text-delta",
                0,
                &json!({"text": format!("line {index}\n")}),
            ),
        );
        seq += 1;
    }
    // A text block replaces the Tool call block at the same index.
    push(
        &mut window,
        chunk(seq, "text-delta", 2, &json!({"text": "replaced\n"})),
    );
    seq += 1;
    // A completed block replaces the accumulated text wholesale.
    push(
        &mut window,
        chunk(
            seq,
            "block-end",
            0,
            &json!({"block": {"type": "text", "text": "line 0\nline 1\nline 2\nline 3\nline 4\n"}}),
        ),
    );
    seq += 1;
    push(
        &mut window,
        chunk(seq, "text-delta", 0, &json!({"text": " after end\n"})),
    );
    seq += 1;
    window.push(assistant_message(seq, "done"));
    window
}

fn publish(window: &[ConversationEventInput], folds: bool) -> (String, usize) {
    let folded = Rc::new(Cell::new(0));
    let mut assembler = ConversationNodeAssembler::new(
        Rc::new(FoldableEvents {
            folds,
            folded: folded.clone(),
        }),
        Rc::new(Views),
    );
    assembler.replace_window(window, false).unwrap();
    assembler.flush().unwrap();
    let published = assembler
        .snapshot("chat")
        .expect("the chat view published a snapshot")
        .as_raw()
        .to_owned();
    (published, folded.get())
}

#[test]
fn a_folded_run_publishes_exactly_what_separate_chunk_updates_publish() {
    let window = streaming_window();
    let (sequential, single_folds) = publish(&window, false);
    let (batched, batch_folds) = publish(&window, true);

    assert_eq!(single_folds, 0, "an unbatched run never folds");
    assert!(
        batch_folds > 0 && batch_folds < window.len(),
        "the run folded in {batch_folds} calls over {} events",
        window.len()
    );
    assert_eq!(
        batched, sequential,
        "folding a chunk run must publish exactly what separate updates publish"
    );
    assert!(batched.contains("line 4"), "the run published its text");
    assert!(
        batched.contains("thinking step 4"),
        "the run published its reasoning"
    );
    assert!(
        batched.contains("replaced"),
        "the run published the replacement block"
    );
}
