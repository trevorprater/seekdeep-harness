//! Portable reducer parity for the conversation input machine.

#![cfg(not(target_arch = "wasm32"))]

use std::{cell::Cell, rc::Rc};

use seekdeep_client_ui_conversation::{
    BusyEnterBehavior, CommandSubmitId, CommandSubmitOutcome, CommandSubmitOutcomeKind,
    ConsumeTokenGuard, DecorationClaim, DecorationOccurrence, DecorationPhase, DraftRevision,
    EditRange, EditSelection, InputCommandClaim, InputDecorationState, InputMachine,
    InputMachineEffect, InputMachineEvent, InputMachineOptions, InputNoticeLevel, InputPhase,
    InputPickOutcome, InputReferenceInsert, InputTokenSpan, PasteComponent, ReferenceLexicon,
    SubmitAttempt, derive_decorations, project_input_clipboard,
};

fn claim(name: &str) -> InputCommandClaim {
    InputCommandClaim {
        token: format!("/{name} "),
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

fn span(machine: &InputMachine, start: u32, end: u32) -> InputTokenSpan {
    InputTokenSpan {
        start,
        end,
        draft_rev: machine.state().draft_rev,
    }
}

fn utf16_find(value: &str, needle: &str) -> u32 {
    let byte = value.find(needle).unwrap();
    u32::try_from(value[..byte].encode_utf16().count()).unwrap()
}

fn adjudicate(effect: &InputMachineEffect) -> SubmitAttempt {
    let InputMachineEffect::Adjudicate { attempt, .. } = effect else {
        panic!("expected adjudicate effect")
    };
    attempt.clone()
}

fn begin_submit(effect: &InputMachineEffect) -> (SubmitAttempt, String) {
    let InputMachineEffect::BeginSubmit { attempt, args, .. } = effect else {
        panic!("expected begin-submit effect")
    };
    (attempt.clone(), args.clone())
}

#[test]
fn enter_arbitration_preserves_mode_snapshot_staleness_and_release_abort() {
    let mut machine = InputMachine::default();
    assert!(
        machine
            .dispatch(InputMachineEvent::Enter(BusyEnterBehavior::Queue))
            .is_empty()
    );
    set_draft(&mut machine, "hello");
    assert_eq!(
        machine.dispatch(InputMachineEvent::Enter(BusyEnterBehavior::Steer)),
        [InputMachineEffect::DefaultSink {
            draft: "hello".to_owned(),
            mode: BusyEnterBehavior::Steer,
        }]
    );
    set_draft(&mut machine, "\n /goal x");
    let effects = machine.dispatch(InputMachineEvent::Enter(BusyEnterBehavior::Queue));
    let attempt = adjudicate(&effects[0]);
    assert_eq!(attempt.draft_snapshot, "\n /goal x");
    assert_eq!(machine.state().phase, InputPhase::Adjudicating);
    assert!(
        machine
            .dispatch(InputMachineEvent::Enter(BusyEnterBehavior::Queue))
            .is_empty()
    );

    let stale = SubmitAttempt {
        id: seekdeep_client_ui_conversation::SubmitAttemptId::new(999),
        ..attempt.clone()
    };
    assert!(
        machine
            .dispatch(InputMachineEvent::Adjudicated {
                attempt: stale,
                outcome: InputPickOutcome::Miss,
            })
            .is_empty()
    );
    machine.dispatch(InputMachineEvent::Release);
    assert!(attempt.signal.aborted());
    assert_eq!(machine.state().phase, InputPhase::Plain);
    assert!(
        machine
            .dispatch(InputMachineEvent::Adjudicated {
                attempt,
                outcome: InputPickOutcome::Miss,
            })
            .is_empty()
    );
}

#[test]
fn adjudication_claim_miss_handled_and_failure_follow_distinct_effect_paths() {
    let mut machine = InputMachine::default();
    set_draft(&mut machine, "/goal x\ny");
    let attempt =
        adjudicate(&machine.dispatch(InputMachineEvent::Enter(BusyEnterBehavior::Queue))[0]);
    let effects = machine.dispatch(InputMachineEvent::Adjudicated {
        attempt: attempt.clone(),
        outcome: InputPickOutcome::Claim(claim("goal")),
    });
    let (_, args) = begin_submit(&effects[0]);
    assert_eq!(args, "x\ny");
    assert_eq!(machine.state().phase, InputPhase::Submitting);

    let mut missed = InputMachine::default();
    set_draft(&mut missed, "/unknown");
    let attempt =
        adjudicate(&missed.dispatch(InputMachineEvent::Enter(BusyEnterBehavior::Steer))[0]);
    assert_eq!(
        missed.dispatch(InputMachineEvent::Adjudicated {
            attempt: attempt.clone(),
            outcome: InputPickOutcome::Miss,
        }),
        [InputMachineEffect::DefaultSink {
            draft: "/unknown".to_owned(),
            mode: BusyEnterBehavior::Steer,
        }]
    );

    let mut handled = InputMachine::default();
    set_draft(&mut handled, "/popup");
    let attempt =
        adjudicate(&handled.dispatch(InputMachineEvent::Enter(BusyEnterBehavior::Queue))[0]);
    assert!(
        handled
            .dispatch(InputMachineEvent::Adjudicated {
                attempt,
                outcome: InputPickOutcome::Handled,
            })
            .is_empty()
    );
    assert_eq!(handled.state().phase, InputPhase::Plain);

    let mut failed = InputMachine::default();
    set_draft(&mut failed, "/warm");
    let attempt =
        adjudicate(&failed.dispatch(InputMachineEvent::Enter(BusyEnterBehavior::Queue))[0]);
    assert_eq!(
        failed.dispatch(InputMachineEvent::AdjudicationFailed {
            attempt,
            message: "warm failed".to_owned(),
        }),
        [InputMachineEffect::Notice {
            level: InputNoticeLevel::Error,
            text: "warm failed".to_owned(),
        }]
    );
    assert_eq!(failed.state().draft, "/warm");
}

#[test]
fn command_claim_span_cas_submit_commit_and_rollback_match_source() {
    let mut machine = InputMachine::default();
    set_draft(&mut machine, "  /go args");
    let stale = InputTokenSpan {
        start: 2,
        end: 5,
        draft_rev: DraftRevision::default(),
    };
    let revision = machine.state().draft_rev;
    machine.dispatch(InputMachineEvent::BeginCommand {
        claim: claim("goal"),
        span: stale,
    });
    assert_eq!(machine.state().draft_rev, revision);
    machine.dispatch(InputMachineEvent::BeginCommand {
        claim: claim("goal"),
        span: span(&machine, 2, 5),
    });
    assert_eq!(machine.state().draft, "/goal  args");
    assert_eq!(machine.state().phase, InputPhase::Claimed);
    let effects = machine.dispatch(InputMachineEvent::Enter(BusyEnterBehavior::Queue));
    let (attempt, args) = begin_submit(&effects[0]);
    assert_eq!(args, " args");
    machine.dispatch(InputMachineEvent::SubmitSettled {
        attempt: attempt.clone(),
        ok: false,
        outcome: None,
        message: Some("retry".to_owned()),
    });
    assert_eq!(machine.state().phase, InputPhase::Claimed);
    let effects = machine.dispatch(InputMachineEvent::Enter(BusyEnterBehavior::Queue));
    let (next, _) = begin_submit(&effects[0]);
    assert_ne!(next.id, attempt.id);
    let notice = machine.dispatch(InputMachineEvent::SubmitSettled {
        attempt: next,
        ok: true,
        outcome: Some(CommandSubmitOutcome {
            kind: CommandSubmitOutcomeKind::Success,
            text: Some("done".to_owned()),
        }),
        message: None,
    });
    assert_eq!(machine.state().draft, "");
    assert_eq!(machine.state().phase, InputPhase::Plain);
    assert_eq!(
        notice,
        [InputMachineEffect::Notice {
            level: InputNoticeLevel::Info,
            text: "done".to_owned(),
        }]
    );
    machine.dispatch(InputMachineEvent::Undo);
    assert_eq!(machine.state().draft, "");
}

#[test]
fn references_reconcile_offsets_guards_clipboard_invalidity_and_table_identity() {
    let mut machine = InputMachine::default();
    set_draft(&mut machine, "/one /one");
    machine.dispatch(InputMachineEvent::InsertReference {
        reference: reference("one"),
        span: span(&machine, 0, 4),
    });
    let second_start = 2;
    machine.dispatch(InputMachineEvent::InsertReference {
        reference: reference("one"),
        span: span(&machine, second_start, 6),
    });
    let state = machine.state();
    assert_eq!(state.occurrences.len(), 2);
    assert_ne!(
        state.occurrences[0].occurrence_id,
        state.occurrences[1].occurrence_id
    );
    assert_eq!(
        project_input_clipboard(&state.draft, &state.occurrences),
        "/one /one "
    );
    let before = state.occurrences.clone();
    machine.dispatch(InputMachineEvent::SetInvalid(Vec::new()));
    assert!(Rc::ptr_eq(&before, &machine.state().occurrences));
    let id = state.occurrences[1].occurrence_id;
    machine.dispatch(InputMachineEvent::SetInvalid(vec![id]));
    assert!(!machine.state().occurrences[0].invalid);
    assert!(machine.state().occurrences[1].invalid);

    let draft = machine.state().draft.clone();
    machine.dispatch(InputMachineEvent::DraftChanged {
        draft: format!("x{draft}"),
        edit_range: Some(EditRange {
            start: 0,
            end: 0,
            inserted_length: 1,
        }),
    });
    assert_eq!(machine.state().occurrences[0].offset, 1);
    let first = machine.state().occurrences[0].offset;
    let mut units = machine.state().draft.encode_utf16().collect::<Vec<_>>();
    units.remove(usize::try_from(first).unwrap());
    machine.dispatch(InputMachineEvent::DraftChanged {
        draft: String::from_utf16(&units).unwrap(),
        edit_range: None,
    });
    assert_eq!(machine.state().occurrences.len(), 1);
}

#[test]
fn consume_guards_and_claim_watch_are_revision_observable() {
    let mut machine = InputMachine::default();
    set_draft(&mut machine, "/goal ");
    machine.dispatch(InputMachineEvent::BeginCommand {
        claim: claim("goal"),
        span: span(&machine, 0, 5),
    });
    let stale = span(&machine, 0, 6);
    set_draft(&mut machine, "/goal x");
    let revision = machine.state().draft_rev;
    machine.dispatch(InputMachineEvent::ConsumeToken(ConsumeTokenGuard::Span(
        stale,
    )));
    assert_eq!(machine.state().draft_rev, revision);
    machine.dispatch(InputMachineEvent::ConsumeToken(
        ConsumeTokenGuard::BareToken("/goal".to_owned()),
    ));
    assert_eq!(machine.state().draft, "/goal x");
    set_draft(&mut machine, " /goal ");
    machine.dispatch(InputMachineEvent::ConsumeToken(
        ConsumeTokenGuard::BareToken("/goal".to_owned()),
    ));
    assert_eq!(machine.state().draft, "");
    assert_eq!(machine.state().phase, InputPhase::Plain);
}

#[test]
fn typing_merge_window_undo_redo_and_redo_cut_match_transaction_rules() {
    let now = Rc::new(Cell::new(0.0));
    let clock = now.clone();
    let mut machine = InputMachine::new(InputMachineOptions {
        merge_window_ms: 1_000.0,
        now: Rc::new(move || clock.get()),
    });
    set_draft(&mut machine, "a");
    now.set(500.0);
    set_draft(&mut machine, "ab");
    machine.dispatch(InputMachineEvent::Undo);
    assert_eq!(machine.state().draft, "");
    machine.dispatch(InputMachineEvent::Redo);
    assert_eq!(machine.state().draft, "ab");
    machine.dispatch(InputMachineEvent::Undo);
    set_draft(&mut machine, "x");
    machine.dispatch(InputMachineEvent::Redo);
    assert_eq!(machine.state().draft, "x");

    let mut split = InputMachine::new(InputMachineOptions {
        merge_window_ms: 10.0,
        now: Rc::new(|| 100.0),
    });
    set_draft(&mut split, "a");
    set_draft(&mut split, "ab");
    split.dispatch(InputMachineEvent::Undo);
    assert_eq!(split.state().draft, "");
}

#[test]
fn paste_sync_and_async_components_form_independent_undo_transactions() {
    let mut machine = InputMachine::default();
    set_draft(&mut machine, "before after");
    machine.dispatch(InputMachineEvent::PasteBegin {
        text: format!(
            "/one {0}/two",
            seekdeep_client_ui_conversation::INPUT_PLACEHOLDER
        ),
        selection: EditSelection { start: 7, end: 7 },
        components: vec![PasteComponent {
            start: 0,
            end: 4,
            reference: reference("one"),
        }],
        generation: 3,
    });
    let pasted = machine.state();
    assert!(!pasted.draft.contains("\u{FFFC}\u{FFFC}"));
    assert_eq!(pasted.occurrences.len(), 1);
    assert_eq!(pasted.paste.unwrap().generation, 3);
    let token_start = utf16_find(&pasted.draft, "/two");
    let attempt = pasted.paste.unwrap();
    machine.dispatch(InputMachineEvent::PasteUpgrade {
        attempt_id: attempt.attempt_id,
        span: InputTokenSpan {
            start: token_start,
            end: token_start + 4,
            draft_rev: pasted.draft_rev,
        },
        reference: reference("two"),
    });
    assert_eq!(machine.state().occurrences.len(), 2);
    machine.dispatch(InputMachineEvent::Undo);
    assert!(machine.state().draft.contains("/two"));
    machine.dispatch(InputMachineEvent::Undo);
    assert_eq!(machine.state().draft, "before after");
}

#[test]
fn draft_change_during_submit_wins_rollback_and_release_aborts_only_own_machine() {
    let mut one = InputMachine::default();
    let mut two = InputMachine::default();
    for machine in [&mut one, &mut two] {
        set_draft(machine, "/goal ");
        machine.dispatch(InputMachineEvent::BeginCommand {
            claim: claim("goal"),
            span: span(machine, 0, 5),
        });
    }
    let one_attempt =
        begin_submit(&one.dispatch(InputMachineEvent::Enter(BusyEnterBehavior::Queue))[0]).0;
    let two_attempt =
        begin_submit(&two.dispatch(InputMachineEvent::Enter(BusyEnterBehavior::Queue))[0]).0;
    set_draft(&mut one, "/goal newer");
    one.dispatch(InputMachineEvent::SubmitSettled {
        attempt: one_attempt.clone(),
        ok: false,
        outcome: None,
        message: Some("failed".to_owned()),
    });
    assert_eq!(one.state().draft, "/goal newer");
    assert_eq!(one.state().phase, InputPhase::Plain);
    assert_eq!(two.state().phase, InputPhase::Submitting);
    two.dispatch(InputMachineEvent::Release);
    assert!(two_attempt.signal.aborted());
    assert!(!one_attempt.signal.aborted());
}

#[test]
fn machine_state_projects_into_existing_decoration_core() {
    let mut machine = InputMachine::default();
    set_draft(&mut machine, "/goal ");
    machine.dispatch(InputMachineEvent::BeginCommand {
        claim: claim("goal"),
        span: span(&machine, 0, 5),
    });
    let state = machine.state();
    let decorations = derive_decorations(
        &InputDecorationState {
            draft: state.draft,
            phase: DecorationPhase::Claimed,
            claim: state.claim.map(|claim| DecorationClaim {
                token: claim.token,
                hint: claim.hint,
            }),
            occurrences: state
                .occurrences
                .iter()
                .map(|occurrence| DecorationOccurrence {
                    occurrence_id: occurrence.occurrence_id,
                    offset: occurrence.offset,
                    label: occurrence.label.clone(),
                    invalid: occurrence.invalid,
                })
                .collect(),
        },
        &ReferenceLexicon::new(),
    );
    assert_eq!(decorations.token.unwrap().end, 6);
    assert_eq!(decorations.hint.as_deref(), Some("hint"));
}

#[test]
fn typing_beyond_window_splits_and_transaction_ring_caps_at_one_hundred() {
    let now = Rc::new(Cell::new(0.0));
    let clock = now.clone();
    let mut split = InputMachine::new(InputMachineOptions {
        merge_window_ms: 1_000.0,
        now: Rc::new(move || clock.get()),
    });
    set_draft(&mut split, "a");
    now.set(2_000.0);
    set_draft(&mut split, "ab");
    split.dispatch(InputMachineEvent::Undo);
    assert_eq!(split.state().draft, "a");

    let mut bounded = InputMachine::default();
    for index in 0..101 {
        set_draft(&mut bounded, &format!("value-{index}"));
    }
    for _ in 0..100 {
        bounded.dispatch(InputMachineEvent::Undo);
    }
    assert_eq!(bounded.state().draft, "value-0");
    bounded.dispatch(InputMachineEvent::Undo);
    assert_eq!(bounded.state().draft, "value-0");
}

#[test]
fn nonclaim_adjudication_and_paste_attempt_killers_drop_late_work() {
    for outcome in [
        InputPickOutcome::Insert(reference("one")),
        InputPickOutcome::Text("/one ".to_owned()),
    ] {
        let mut machine = InputMachine::default();
        set_draft(&mut machine, "/one");
        let attempt =
            adjudicate(&machine.dispatch(InputMachineEvent::Enter(BusyEnterBehavior::Queue))[0]);
        assert!(
            machine
                .dispatch(InputMachineEvent::Adjudicated { attempt, outcome })
                .is_empty()
        );
        assert_eq!(machine.state().phase, InputPhase::Plain);
        assert_eq!(machine.state().draft, "/one");
    }

    let mut machine = InputMachine::default();
    machine.dispatch(InputMachineEvent::PasteBegin {
        text: "/one".to_owned(),
        selection: EditSelection { start: 0, end: 0 },
        components: Vec::new(),
        generation: 0,
    });
    let attempt = machine.state().paste.unwrap();
    let revision = machine.state().draft_rev;
    machine.dispatch(InputMachineEvent::InvalidatePaste);
    machine.dispatch(InputMachineEvent::PasteUpgrade {
        attempt_id: attempt.attempt_id,
        span: InputTokenSpan {
            start: 0,
            end: 4,
            draft_rev: revision,
        },
        reference: reference("one"),
    });
    assert_eq!(machine.state().draft, "/one");
    assert!(machine.state().occurrences.is_empty());
}

#[test]
fn submit_failure_text_priority_and_send_commit_clear_without_undo_resurrection() {
    let mut machine = InputMachine::default();
    set_draft(&mut machine, "/goal ");
    machine.dispatch(InputMachineEvent::BeginCommand {
        claim: claim("goal"),
        span: span(&machine, 0, 5),
    });
    let attempt =
        begin_submit(&machine.dispatch(InputMachineEvent::Enter(BusyEnterBehavior::Queue))[0]).0;
    assert_eq!(
        machine.dispatch(InputMachineEvent::SubmitSettled {
            attempt,
            ok: false,
            outcome: Some(CommandSubmitOutcome {
                kind: CommandSubmitOutcomeKind::Error,
                text: Some("outcome failure".to_owned()),
            }),
            message: None,
        }),
        [InputMachineEffect::Notice {
            level: InputNoticeLevel::Error,
            text: "outcome failure".to_owned(),
        }]
    );
    assert_eq!(machine.state().phase, InputPhase::Claimed);
    machine.dispatch(InputMachineEvent::SendCommitted);
    assert_eq!(machine.state().draft, "");
    machine.dispatch(InputMachineEvent::Undo);
    assert_eq!(machine.state().draft, "");
}
