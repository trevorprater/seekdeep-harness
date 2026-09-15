//! Trigger detection, menu reduction, exact lookup, and locale parity.

use std::borrow::Cow;

use seekdeep_client_ui_input_trigger::{
    InputTriggerCandidate, MENU_LOCALES, MENU_NS, MenuEvent, MenuGroup, MenuGroupStatus,
    MenuHighlight, MenuState, MoveDirection, TokenSpan, TriggerChar, TriggerHit, TriggerPosition,
    TriggerTier, detect_trigger, exact_match, menu_reduce, seed_groups,
};

fn hit(query: &str) -> TriggerHit {
    TriggerHit {
        trigger: TriggerChar::Slash,
        query: query.to_owned(),
        position: TriggerPosition::Leading,
        span: TokenSpan {
            start: 0,
            end: 1 + query.encode_utf16().count(),
            draft_rev: 1,
        },
    }
}

fn item(name: &str) -> InputTriggerCandidate {
    InputTriggerCandidate::named(name)
}

fn open(sources: &[&str]) -> MenuState {
    let sources = sources
        .iter()
        .map(|source| (*source).to_owned())
        .collect::<Vec<_>>();
    menu_reduce(
        &seed_groups(&MenuState::default(), &sources),
        &MenuEvent::Hit(Some(hit(""))),
    )
    .into_owned()
}

#[allow(clippy::needless_pass_by_value)]
fn reduce(state: &MenuState, event: MenuEvent) -> MenuState {
    menu_reduce(state, &event).into_owned()
}

#[test]
#[allow(clippy::too_many_lines)]
fn detection_preserves_boundaries_positions_guards_and_utf16_spans() {
    for (draft, trigger, query, position) in [
        ("/go", TriggerChar::Slash, "go", TriggerPosition::Leading),
        ("@wo", TriggerChar::At, "wo", TriggerPosition::Leading),
        ("say /co", TriggerChar::Slash, "co", TriggerPosition::Inline),
        (
            "line1\n/go",
            TriggerChar::Slash,
            "go",
            TriggerPosition::Inline,
        ),
        (
            "see (/go",
            TriggerChar::Slash,
            "go",
            TriggerPosition::Inline,
        ),
        ("ping @wo", TriggerChar::At, "wo", TriggerPosition::Inline),
        (
            "\n\n/goal",
            TriggerChar::Slash,
            "goal",
            TriggerPosition::Leading,
        ),
        (
            "  \n /goal",
            TriggerChar::Slash,
            "goal",
            TriggerPosition::Leading,
        ),
        (
            "第一行\n/goal",
            TriggerChar::Slash,
            "goal",
            TriggerPosition::Inline,
        ),
    ] {
        let caret = draft.encode_utf16().count();
        let found = detect_trigger(draft, caret, TriggerTier::Plain).unwrap();
        assert_eq!(
            (found.trigger, found.query.as_str(), found.position),
            (trigger, query, position)
        );
    }

    for draft in [
        "user@host",
        "a/b",
        "foo_1@bar",
        "https://example",
        "see https://example",
        "https://a.b/c/d",
        "C:/path",
        "/goal x",
        "@worker done",
    ] {
        assert_eq!(
            detect_trigger(draft, draft.encode_utf16().count(), TriggerTier::Plain),
            None,
            "{draft}"
        );
    }

    assert_eq!(
        detect_trigger("note: /go", 9, TriggerTier::Plain)
            .unwrap()
            .query,
        "go"
    );
    assert_eq!(
        detect_trigger(":/go", 4, TriggerTier::Plain).unwrap().query,
        "go"
    );
    assert_eq!(
        detect_trigger("/goal @wor", 10, TriggerTier::Plain)
            .unwrap()
            .trigger,
        TriggerChar::At
    );
    assert_eq!(detect_trigger("/co", 3, TriggerTier::Claimed), None);
    assert_eq!(
        detect_trigger("/goal @wor", 10, TriggerTier::Claimed)
            .unwrap()
            .trigger,
        TriggerChar::At
    );
    assert_eq!(detect_trigger("@wo", 3, TriggerTier::Frozen), None);
    assert_eq!(detect_trigger("/goal", 0, TriggerTier::Plain), None);

    let mid = detect_trigger("say /goal", 9, TriggerTier::Plain).unwrap();
    assert_eq!(
        mid.span,
        TokenSpan {
            start: 4,
            end: 9,
            draft_rev: 0,
        }
    );
    assert_eq!(
        detect_trigger("/goal", 3, TriggerTier::Plain)
            .unwrap()
            .query,
        "go"
    );

    let astral = "🦀 /go";
    let found = detect_trigger(astral, astral.encode_utf16().count(), TriggerTier::Plain).unwrap();
    assert_eq!(found.span.start, 3);
    assert_eq!(found.span.end, 6);
}

#[test]
fn hit_and_settlement_are_generation_gated_and_empty_groups_auto_close() {
    let mut state = open(&["command", "skill"]);
    assert!(state.open);
    assert_eq!(state.generation, 1);
    assert!(
        state
            .groups
            .iter()
            .all(|group| group.status == MenuGroupStatus::Pending)
    );
    state = reduce(
        &state,
        MenuEvent::SourceSettled {
            generation: 1,
            source: "skill".to_owned(),
            items: Some(vec![item("commit")]),
        },
    );
    assert_eq!(
        state.highlight,
        Some(MenuHighlight {
            source: "skill".to_owned(),
            index: 0
        })
    );
    state = reduce(
        &state,
        MenuEvent::SourceSettled {
            generation: 1,
            source: "command".to_owned(),
            items: Some(vec![item("goal")]),
        },
    );
    assert_eq!(state.highlight.as_ref().unwrap().source, "skill");
    state = reduce(&state, MenuEvent::Hit(Some(hit("g"))));
    assert_eq!(state.generation, 2);
    assert!(
        state
            .groups
            .iter()
            .all(|group| group.status == MenuGroupStatus::Pending)
    );
    assert!(state.highlight.is_none());
    assert!(matches!(
        menu_reduce(
            &state,
            &MenuEvent::SourceSettled {
                generation: 1,
                source: "command".to_owned(),
                items: Some(vec![item("stale")]),
            },
        ),
        Cow::Borrowed(_)
    ));

    let mut empty = open(&["command", "skill"]);
    empty = reduce(
        &empty,
        MenuEvent::SourceSettled {
            generation: 1,
            source: "command".to_owned(),
            items: None,
        },
    );
    assert!(empty.open);
    empty = reduce(
        &empty,
        MenuEvent::SourceSettled {
            generation: 1,
            source: "skill".to_owned(),
            items: Some(Vec::new()),
        },
    );
    assert!(!empty.open);
    assert!(empty.groups.is_empty());
    assert_eq!(empty.generation, 1);
    assert!(matches!(
        menu_reduce(&empty, &MenuEvent::Hit(None)),
        Cow::Borrowed(_)
    ));
}

#[test]
fn source_failure_removes_groups_and_repairs_or_closes_highlight() {
    let mut state = open(&["command", "skill"]);
    state = reduce(
        &state,
        MenuEvent::SourceSettled {
            generation: 1,
            source: "command".to_owned(),
            items: Some(vec![item("goal")]),
        },
    );
    state = reduce(
        &state,
        MenuEvent::SourceSettled {
            generation: 1,
            source: "skill".to_owned(),
            items: Some(vec![item("commit")]),
        },
    );
    assert_eq!(state.highlight.as_ref().unwrap().source, "command");
    state = reduce(
        &state,
        MenuEvent::SourceFailed {
            generation: 1,
            source: "command".to_owned(),
        },
    );
    assert_eq!(
        state
            .groups
            .iter()
            .map(|group| group.source.as_str())
            .collect::<Vec<_>>(),
        ["skill"]
    );
    assert_eq!(state.highlight.as_ref().unwrap().source, "skill");

    assert!(matches!(
        menu_reduce(
            &state,
            &MenuEvent::SourceFailed {
                generation: 0,
                source: "skill".to_owned(),
            },
        ),
        Cow::Borrowed(_)
    ));
    state = reduce(
        &state,
        MenuEvent::SourceFailed {
            generation: 1,
            source: "skill".to_owned(),
        },
    );
    assert!(!state.open);

    let mut ready_empty = open(&["command", "skill"]);
    ready_empty = reduce(
        &ready_empty,
        MenuEvent::SourceSettled {
            generation: 1,
            source: "skill".to_owned(),
            items: Some(Vec::new()),
        },
    );
    ready_empty = reduce(
        &ready_empty,
        MenuEvent::SourceFailed {
            generation: 1,
            source: "command".to_owned(),
        },
    );
    assert!(!ready_empty.open);
}

fn ready_menu() -> MenuState {
    let mut state = open(&["command", "skill"]);
    state = reduce(
        &state,
        MenuEvent::SourceSettled {
            generation: 1,
            source: "command".to_owned(),
            items: Some(vec![item("goal"), item("model")]),
        },
    );
    reduce(
        &state,
        MenuEvent::SourceSettled {
            generation: 1,
            source: "skill".to_owned(),
            items: Some(vec![item("commit")]),
        },
    )
}

#[test]
fn movement_cycles_across_ready_groups_and_preserves_noop_identity() {
    let mut state = ready_menu();
    state = reduce(&state, MenuEvent::Move(MoveDirection::Next));
    assert_eq!(
        state.highlight,
        Some(MenuHighlight {
            source: "command".to_owned(),
            index: 1
        })
    );
    state = reduce(&state, MenuEvent::Move(MoveDirection::Next));
    assert_eq!(
        state.highlight,
        Some(MenuHighlight {
            source: "skill".to_owned(),
            index: 0
        })
    );
    state = reduce(&state, MenuEvent::Move(MoveDirection::Next));
    assert_eq!(
        state.highlight,
        Some(MenuHighlight {
            source: "command".to_owned(),
            index: 0
        })
    );
    state = reduce(&state, MenuEvent::Move(MoveDirection::Previous));
    assert_eq!(state.highlight.as_ref().unwrap().source, "skill");

    let no_highlight = MenuState {
        highlight: None,
        ..ready_menu()
    };
    assert_eq!(
        reduce(&no_highlight, MenuEvent::Move(MoveDirection::Next)).highlight,
        Some(MenuHighlight {
            source: "command".to_owned(),
            index: 0
        })
    );
    assert_eq!(
        reduce(&no_highlight, MenuEvent::Move(MoveDirection::Previous)).highlight,
        Some(MenuHighlight {
            source: "skill".to_owned(),
            index: 0
        })
    );

    let pending = open(&["command"]);
    assert!(matches!(
        menu_reduce(&pending, &MenuEvent::Move(MoveDirection::Next)),
        Cow::Borrowed(_)
    ));
    let single = reduce(
        &pending,
        MenuEvent::SourceSettled {
            generation: 1,
            source: "command".to_owned(),
            items: Some(vec![item("goal")]),
        },
    );
    assert!(matches!(
        menu_reduce(&single, &MenuEvent::Move(MoveDirection::Next)),
        Cow::Borrowed(_)
    ));
    let closed = reduce(&state, MenuEvent::Close);
    assert!(matches!(
        menu_reduce(&closed, &MenuEvent::Move(MoveDirection::Next)),
        Cow::Borrowed(_)
    ));
}

#[test]
fn exact_lookup_requires_a_ready_group_and_locales_are_exact() {
    let groups = vec![
        MenuGroup {
            source: "command".to_owned(),
            status: MenuGroupStatus::Ready,
            items: vec![item("goal"), item("model")],
        },
        MenuGroup {
            source: "skill".to_owned(),
            status: MenuGroupStatus::Pending,
            items: Vec::new(),
        },
    ];
    assert_eq!(
        exact_match(&groups, "command", "model"),
        Some(&item("model"))
    );
    assert_eq!(exact_match(&groups, "command", "goa"), None);
    assert_eq!(exact_match(&groups, "skill", "commit"), None);
    assert_eq!(exact_match(&groups, "ghost", "goal"), None);
    assert_eq!(MENU_NS, "slash.menu");
    assert_eq!(MENU_LOCALES.len(), 5);
    assert_eq!(MENU_LOCALES[2], ("subagent", "子智能体", "Subagents"));
    assert_eq!(
        MENU_LOCALES[4],
        ("suggestions.aria", "触发候选建议", "Trigger suggestions")
    );
}
