//! Portable parity coverage for composer draft decorations.

#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;

use seekdeep_client_ui_conversation::{
    DecorationClaim, DecorationOccurrence, DecorationPhase, InputDecorationState, OccurrenceId,
    ReferenceLexicon, ReferenceTrigger, TextRefRange, derive_decorations, scan_text_refs,
};

fn lexicon() -> ReferenceLexicon {
    BTreeMap::from([
        (
            ReferenceTrigger::Slash,
            vec!["deploy".to_owned(), "goal".to_owned()],
        ),
        (ReferenceTrigger::At, vec!["agent".to_owned()]),
    ])
}

#[test]
fn text_reference_scan_uses_js_boundaries_ascii_names_and_utf16_offsets() {
    let draft = "😀 /deploy x/name @agent /unknown\n/goal /deploy!\u{FEFF}@agent";
    assert_eq!(
        scan_text_refs(draft, &lexicon()),
        vec![
            TextRefRange {
                start: 3,
                end: 10,
                trigger: ReferenceTrigger::Slash,
            },
            TextRefRange {
                start: 18,
                end: 24,
                trigger: ReferenceTrigger::At,
            },
            TextRefRange {
                start: 34,
                end: 39,
                trigger: ReferenceTrigger::Slash,
            },
            TextRefRange {
                start: 40,
                end: 47,
                trigger: ReferenceTrigger::Slash,
            },
            TextRefRange {
                start: 49,
                end: 55,
                trigger: ReferenceTrigger::At,
            },
        ]
    );
    assert!(scan_text_refs("x/deploy /missing", &lexicon()).is_empty());
    assert!(scan_text_refs("/deploy", &ReferenceLexicon::new()).is_empty());
}

#[test]
fn decorations_follow_claim_watch_occurrences_hot_lexicon_and_blank_arguments() {
    let occurrence = DecorationOccurrence {
        occurrence_id: OccurrenceId::new(7),
        offset: 3,
        label: "@agent".to_owned(),
        invalid: true,
    };
    let state = InputDecorationState {
        draft: "/goal \u{FFFC} /deploy".to_owned(),
        phase: DecorationPhase::Claimed,
        claim: Some(DecorationClaim {
            token: "/goal ".to_owned(),
            hint: Some("目标".to_owned()),
        }),
        occurrences: vec![occurrence.clone()],
    };
    let decorated = derive_decorations(&state, &lexicon());
    assert_eq!(decorated.token.unwrap().end, 6);
    assert_eq!(decorated.chips[0].occurrence_id, OccurrenceId::new(7));
    assert!(decorated.chips[0].invalid);
    assert_eq!(decorated.text_refs.len(), 2);
    assert_eq!(decorated.hint, None);

    let blank = derive_decorations(
        &InputDecorationState {
            draft: "/goal \u{FEFF}\n".to_owned(),
            phase: DecorationPhase::Submitting,
            claim: state.claim.clone(),
            occurrences: vec![occurrence],
        },
        &ReferenceLexicon::new(),
    );
    assert_eq!(blank.hint.as_deref(), Some("目标"));
    assert_eq!(blank.token.unwrap().end, 6);

    let released = derive_decorations(
        &InputDecorationState {
            draft: "/goa ".to_owned(),
            phase: DecorationPhase::Claimed,
            claim: state.claim,
            occurrences: Vec::new(),
        },
        &ReferenceLexicon::new(),
    );
    assert_eq!(released.token, None);
    assert_eq!(released.hint, None);
}
