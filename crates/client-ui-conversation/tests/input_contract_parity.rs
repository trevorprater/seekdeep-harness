//! Portable compile-time coverage for the frozen input contract.

#![cfg(not(target_arch = "wasm32"))]

use std::{cell::RefCell, rc::Rc};

use seekdeep_client_ui_conversation::{
    ArbitrateKey, ArbitrateOutcome, BusyEnterBehavior, ComposerKeyboard, DraftAttachmentId,
    EditRange, EditSelection, InputActions, InputCommandClaim, InputMachine, InputMachineEvent,
    InputMachineState, InputNoticeLevel, InputReferenceInsert, InputStateSource, InputTarget,
    InputTokenSpan, PasteComponent, SessionInput,
};

struct StateSource {
    machine: Rc<RefCell<InputMachine>>,
}

impl InputStateSource for StateSource {
    fn snapshot(&self) -> InputMachineState {
        self.machine.borrow().state()
    }

    fn subscribe(&self, _listener: Rc<dyn Fn()>) -> Box<dyn FnOnce()> {
        Box::new(|| {})
    }
}

struct StructuralInput {
    machine: Rc<RefCell<InputMachine>>,
    source: StateSource,
}

impl StructuralInput {
    fn new() -> Self {
        let machine = Rc::new(RefCell::new(InputMachine::default()));
        Self {
            machine: machine.clone(),
            source: StateSource { machine },
        }
    }
}

impl InputTarget for StructuralInput {
    fn begin_command(&self, claim: InputCommandClaim, span: InputTokenSpan) -> bool {
        let before = self.machine.borrow().state().draft_rev;
        self.machine
            .borrow_mut()
            .dispatch(InputMachineEvent::BeginCommand { claim, span });
        self.machine.borrow().state().draft_rev != before
    }

    fn insert_reference(&self, reference: InputReferenceInsert, span: InputTokenSpan) -> bool {
        let before = self.machine.borrow().state().draft_rev;
        self.machine
            .borrow_mut()
            .dispatch(InputMachineEvent::InsertReference { reference, span });
        self.machine.borrow().state().draft_rev != before
    }
}

impl InputActions for StructuralInput {
    fn set_draft(&self, text: String) {
        self.machine
            .borrow_mut()
            .dispatch(InputMachineEvent::DraftChanged {
                draft: text,
                edit_range: None,
            });
    }

    fn add_images(&self, _ids: Vec<DraftAttachmentId>) -> bool {
        true
    }

    fn remove_image(&self, _id: &DraftAttachmentId) {}

    fn prune_images(&self, _ids: &[DraftAttachmentId]) {}

    fn submit(&self) {
        self.submit_mode(BusyEnterBehavior::Queue);
    }
}

impl SessionInput for StructuralInput {
    fn submit_mode(&self, mode: BusyEnterBehavior) {
        self.machine
            .borrow_mut()
            .dispatch(InputMachineEvent::Enter(mode));
    }

    fn notify(&self, _level: InputNoticeLevel, _text: String) {}

    fn state(&self) -> &dyn InputStateSource {
        &self.source
    }
}

impl ComposerKeyboard for StructuralInput {
    fn snapshot(&self) -> InputMachineState {
        self.source.snapshot()
    }

    fn set_draft(&self, text: String, edit_range: Option<EditRange>) {
        self.machine
            .borrow_mut()
            .dispatch(InputMachineEvent::DraftChanged {
                draft: text,
                edit_range,
            });
    }

    fn submit(&self, mode: BusyEnterBehavior) {
        self.submit_mode(mode);
    }

    fn steer_queue(&self) {}
    fn undo(&self) {
        self.machine.borrow_mut().dispatch(InputMachineEvent::Undo);
    }
    fn redo(&self) {
        self.machine.borrow_mut().dispatch(InputMachineEvent::Redo);
    }
    fn paste_begin(
        &self,
        text: String,
        selection: EditSelection,
        components: Vec<PasteComponent>,
        generation: u64,
    ) {
        self.machine
            .borrow_mut()
            .dispatch(InputMachineEvent::PasteBegin {
                text,
                selection,
                components,
                generation,
            });
    }
    fn invalidate_paste(&self) {
        self.machine
            .borrow_mut()
            .dispatch(InputMachineEvent::InvalidatePaste);
    }
    fn track(&self, _draft: &str, _caret: u32) {}
    fn arbitrate(&self, _key: ArbitrateKey, _composing: bool) -> ArbitrateOutcome {
        ArbitrateOutcome::Pass
    }
    fn space(&self) -> bool {
        false
    }
    fn dismiss_popup(&self) {}
}

#[test]
fn nominal_ids_structural_faces_and_state_source_match_the_frozen_contract() {
    let id = DraftAttachmentId::new("draft-1");
    assert_eq!(id.as_str(), "draft-1");
    assert_eq!(id.clone().into_string(), "draft-1");

    let input = StructuralInput::new();
    InputActions::set_draft(&input, "/goal".to_owned());
    assert_eq!(SessionInput::state(&input).snapshot().draft, "/goal");
    assert!(InputActions::add_images(&input, vec![id]));
    ComposerKeyboard::set_draft(
        &input,
        "/goal x".to_owned(),
        Some(EditRange {
            start: 5,
            end: 5,
            inserted_length: 2,
        }),
    );
    assert_eq!(ComposerKeyboard::snapshot(&input).draft, "/goal x");
    let dispose = SessionInput::state(&input).subscribe(Rc::new(|| {}));
    dispose();

    for key in [
        ArbitrateKey::Up,
        ArbitrateKey::Down,
        ArbitrateKey::Enter,
        ArbitrateKey::Escape,
    ] {
        assert_eq!(input.arbitrate(key, false), ArbitrateOutcome::Pass);
    }
    let verdicts = [
        ArbitrateOutcome::Consumed,
        ArbitrateOutcome::PickHighlighted,
        ArbitrateOutcome::Pass,
    ];
    assert_eq!(verdicts.len(), 3);
}
