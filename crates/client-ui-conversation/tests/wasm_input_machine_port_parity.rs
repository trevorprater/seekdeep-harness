//! Live WASM coverage for the portable input reducer.

#![cfg(target_arch = "wasm32")]

use std::rc::Rc;

use seekdeep_client_ui_conversation::{
    BusyEnterBehavior, CommandSubmitId, CommandSubmitOutcome, CommandSubmitOutcomeKind,
    InputCommandClaim, InputMachine, InputMachineEffect, InputMachineEvent, InputNoticeLevel,
    InputPhase, InputPickOutcome, InputReferenceInsert, InputTokenSpan, PasteComponent,
};
use wasm_bindgen_test::wasm_bindgen_test;

fn claim() -> InputCommandClaim {
    InputCommandClaim {
        token: "/goal ".to_owned(),
        hint: Some("hint".to_owned()),
        submit_id: CommandSubmitId::new(1),
    }
}

fn reference(name: &str) -> InputReferenceInsert {
    InputReferenceInsert {
        source: "skills".to_owned(),
        reference: name.to_owned(),
        label: format!("/{name}"),
        clipboard_text: format!("/{name}"),
    }
}

fn set_draft(machine: &mut InputMachine, draft: &str) {
    machine.dispatch(InputMachineEvent::DraftChanged {
        draft: draft.to_owned(),
        edit_range: None,
    });
}

fn utf16_find(value: &str, needle: &str) -> u32 {
    let byte = value.find(needle).unwrap();
    u32::try_from(value[..byte].encode_utf16().count()).unwrap()
}

#[wasm_bindgen_test]
fn adjudication_claim_submit_and_commit_execute_in_compiled_wasm() {
    let mut machine = InputMachine::default();
    set_draft(&mut machine, "/goal x");
    let effects = machine.dispatch(InputMachineEvent::Enter(BusyEnterBehavior::Queue));
    let InputMachineEffect::Adjudicate { attempt, .. } = effects[0].clone() else {
        panic!("expected adjudicate")
    };
    let effects = machine.dispatch(InputMachineEvent::Adjudicated {
        attempt: attempt.clone(),
        outcome: InputPickOutcome::Claim(claim()),
    });
    let InputMachineEffect::BeginSubmit { args, .. } = &effects[0] else {
        panic!("expected submit")
    };
    assert_eq!(args, "x");
    assert_eq!(machine.state().phase, InputPhase::Submitting);
    assert_eq!(
        machine.dispatch(InputMachineEvent::SubmitSettled {
            attempt,
            ok: true,
            outcome: Some(CommandSubmitOutcome {
                kind: CommandSubmitOutcomeKind::Success,
                text: Some("done".to_owned()),
            }),
            message: None,
        }),
        [InputMachineEffect::Notice {
            level: InputNoticeLevel::Info,
            text: "done".to_owned(),
        }]
    );
    assert_eq!(machine.state().draft, "");
}

#[wasm_bindgen_test]
fn reference_paste_upgrade_and_two_stage_undo_execute_in_compiled_wasm() {
    let mut machine = InputMachine::default();
    set_draft(&mut machine, "before");
    machine.dispatch(InputMachineEvent::PasteBegin {
        text: "/one /two".to_owned(),
        selection: seekdeep_client_ui_conversation::EditSelection { start: 6, end: 6 },
        components: vec![PasteComponent {
            start: 0,
            end: 4,
            reference: reference("one"),
        }],
        generation: 0,
    });
    let state = machine.state();
    let attempt = state.paste.unwrap();
    let second = utf16_find(&state.draft, "/two");
    machine.dispatch(InputMachineEvent::PasteUpgrade {
        attempt_id: attempt.attempt_id,
        span: InputTokenSpan {
            start: second,
            end: second + 4,
            draft_rev: state.draft_rev,
        },
        reference: reference("two"),
    });
    assert_eq!(machine.state().occurrences.len(), 2);
    machine.dispatch(InputMachineEvent::Undo);
    assert!(machine.state().draft.contains("/two"));
    machine.dispatch(InputMachineEvent::Undo);
    assert_eq!(machine.state().draft, "before");
}

#[wasm_bindgen_test]
fn stale_attempt_release_abort_and_empty_queue_identity_execute_in_compiled_wasm() {
    let mut machine = InputMachine::default();
    let first_queue = machine.state().queue;
    set_draft(&mut machine, "/goal");
    let effects = machine.dispatch(InputMachineEvent::Enter(BusyEnterBehavior::Steer));
    let InputMachineEffect::Adjudicate { attempt, .. } = effects[0].clone() else {
        panic!("expected adjudicate")
    };
    machine.dispatch(InputMachineEvent::Release);
    assert!(attempt.signal.aborted());
    assert!(
        machine
            .dispatch(InputMachineEvent::Adjudicated {
                attempt,
                outcome: InputPickOutcome::Miss,
            })
            .is_empty()
    );
    assert!(Rc::ptr_eq(&first_queue, &machine.state().queue));
}
