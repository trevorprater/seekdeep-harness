//! Conversation Turn/Step location rebuild, append, and data ownership parity.

use std::rc::Rc;

use seekdeep_client_runtime::{
    ConversationBoundaryStatus, ConversationEventInput, ConversationLocation,
    ConversationLocationData, ConversationLocationDataChange, ConversationLocationEvent,
    ConversationLocationIndex, ConversationOwnedLocationData,
};
use serde_json::{Value, json};

fn event(seq: u64, event_type: &str, data: Value) -> Rc<ConversationLocationEvent> {
    ConversationLocationEvent::new(seq, event_type, data)
}

fn input(event: &Rc<ConversationLocationEvent>) -> ConversationEventInput {
    ConversationEventInput {
        event: event.clone(),
    }
}

fn location(index: &ConversationLocationIndex, event: &ConversationLocationEvent) -> String {
    match index.location_of(event) {
        ConversationLocation::Session => "session".to_owned(),
        ConversationLocation::Unresolved => "unresolved".to_owned(),
        ConversationLocation::Turn { turn } => format!("turn:{}", turn.turn),
        ConversationLocation::Step { turn, step } => {
            format!("step:{}:{}", turn.turn, step.step)
        }
    }
}

#[test]
fn rebuild_resolves_boundaries_inheritance_session_affinity_and_unknown_coordinates() {
    let events = [
        event(0, "context/message", json!({"turn":null})),
        event(1, "turn/start", json!({"turn":1})),
        event(2, "user/message", json!({})),
        event(3, "step/start", json!({"turn":1,"step":1})),
        event(4, "assistant/chunk", json!({})),
        event(5, "step/end", json!({"turn":1,"step":1})),
        event(6, "tool/result", json!({})),
        event(7, "turn/end", json!({"turn":1})),
        event(8, "context/message", json!({})),
        event(9, "custom", json!({"turn":2,"step":3})),
    ];
    let inputs = events.iter().map(input).collect::<Vec<_>>();
    let mut index = ConversationLocationIndex::default();
    assert_eq!(
        index
            .rebuild(&inputs)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        (0..=9).collect::<Vec<_>>()
    );
    assert_eq!(
        events
            .iter()
            .map(|event| location(&index, event))
            .collect::<Vec<_>>(),
        [
            "session", "turn:1", "turn:1", "step:1:1", "step:1:1", "step:1:1", "turn:1", "turn:1",
            "session", "step:2:3",
        ]
    );
    let timeline = index.snapshot();
    assert_eq!(timeline.turn_order.as_slice(), [1, 2]);
    let turn = &timeline.turns[&1];
    assert_eq!(turn.status, ConversationBoundaryStatus::Closed);
    assert!(Rc::ptr_eq(turn.start.as_ref().unwrap(), &events[1]));
    assert!(Rc::ptr_eq(turn.end.as_ref().unwrap(), &events[7]));
    assert_eq!(turn.steps[0].status, ConversationBoundaryStatus::Closed);
    assert!(Rc::ptr_eq(
        turn.steps[0].start.as_ref().unwrap(),
        &events[3]
    ));
    let unknown = &timeline.turns[&2];
    assert_eq!(unknown.status, ConversationBoundaryStatus::Unknown);
    assert_eq!(unknown.steps[0].status, ConversationBoundaryStatus::Unknown);

    assert!(index.rebuild(&inputs).unwrap().is_empty());
    assert!(Rc::ptr_eq(&timeline, &index.snapshot()));
    assert!(Rc::ptr_eq(turn, &index.snapshot().turns[&1]));
}

#[test]
fn append_paths_update_only_the_tail_and_preserve_recorded_coordinates() {
    let mut index = ConversationLocationIndex::default();
    let turn_start = event(1, "turn/start", json!({"turn":1}));
    assert_eq!(
        index
            .append_boundary(&turn_start)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        [1]
    );
    let before_step = event(2, "user/message", json!({}));
    index.append_non_boundary(&before_step);
    assert_eq!(location(&index, &before_step), "turn:1");
    let step_start = event(3, "step/start", json!({"turn":1,"step":1}));
    assert_eq!(
        index
            .append_boundary(&step_start)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    assert_eq!(location(&index, &before_step), "turn:1");
    let in_step = event(4, "assistant/chunk", json!({}));
    index.append_non_boundary(&in_step);
    assert_eq!(location(&index, &in_step), "step:1:1");
    let step_end = event(5, "step/end", json!({"turn":1,"step":1}));
    assert_eq!(
        index
            .append_boundary(&step_end)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
    let after_step = event(6, "tool/result", json!({}));
    index.append_non_boundary(&after_step);
    assert_eq!(location(&index, &after_step), "turn:1");
    let turn_end = event(7, "turn/end", json!({"turn":1}));
    assert_eq!(
        index
            .append_boundary(&turn_end)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>(),
        [1, 2, 3, 4, 5, 6, 7]
    );
    let after_turn = event(8, "context/message", json!({}));
    index.append_non_boundary(&after_turn);
    assert_eq!(location(&index, &after_turn), "session");
    assert_eq!(
        index
            .append_boundary(&event(9, "assistant/message", json!({})))
            .unwrap_err()
            .to_string(),
        "conversation Location boundary expected, received assistant/message"
    );
}

#[test]
fn malformed_and_explicit_session_coordinates_degrade_without_ambient_state() {
    let mut index = ConversationLocationIndex::default();
    index
        .append_boundary(&event(1, "turn/start", json!({"turn":1})))
        .unwrap();
    let malformed = event(
        2,
        "custom",
        json!({"turn":9_007_199_254_740_992_u64,"step":-1}),
    );
    index.append_non_boundary(&malformed);
    assert_eq!(location(&index, &malformed), "turn:1");
    let session = event(3, "custom", json!({"turn":null,"step":1}));
    index.append_non_boundary(&session);
    assert_eq!(location(&index, &session), "session");
}

#[test]
#[allow(clippy::too_many_lines)] // One transaction covers replacement, transfer, conflict, and clear.
fn location_data_keeps_reader_identity_enforces_ownership_and_supports_atomic_transfer() {
    let mut index = ConversationLocationIndex::default();
    let start = event(1, "turn/start", json!({"turn":1}));
    let step = event(2, "step/start", json!({"turn":1,"step":1}));
    index.rebuild(&[input(&start), input(&step)]).unwrap();
    let timeline = index.snapshot();
    let turn_store = timeline.turns[&1].data.clone();
    let step_store = timeline.turns[&1].steps[0].data.clone();
    let turn_value = Rc::new(json!({"label":"one"}));
    let step_value = Rc::new(json!([1, 2]));
    assert!(
        index
            .replace_data(&[
                ConversationOwnedLocationData {
                    owner: "a".to_owned(),
                    data: ConversationLocationData::Turn {
                        turn: 1,
                        key: "summary".to_owned(),
                        value: turn_value.clone(),
                    },
                },
                ConversationOwnedLocationData {
                    owner: "b".to_owned(),
                    data: ConversationLocationData::Step {
                        turn: 1,
                        step: Some(1),
                        key: "usage".to_owned(),
                        value: step_value.clone(),
                    },
                },
            ])
            .unwrap()
    );
    assert!(Rc::ptr_eq(&turn_store.get("summary").unwrap(), &turn_value));
    assert!(Rc::ptr_eq(&step_store.get("usage").unwrap(), &step_value));
    assert!(Rc::ptr_eq(&timeline, &index.snapshot()));
    assert!(
        !index
            .replace_data(&[
                ConversationOwnedLocationData {
                    owner: "a".to_owned(),
                    data: ConversationLocationData::Turn {
                        turn: 1,
                        key: "summary".to_owned(),
                        value: turn_value,
                    },
                },
                ConversationOwnedLocationData {
                    owner: "b".to_owned(),
                    data: ConversationLocationData::Step {
                        turn: 1,
                        step: Some(1),
                        key: "usage".to_owned(),
                        value: step_value,
                    },
                },
            ])
            .unwrap()
    );

    let transferred = Rc::new(json!({"label":"two"}));
    assert!(
        index
            .apply_data(&[
                ConversationLocationDataChange {
                    owner: "a".to_owned(),
                    previous: Some(ConversationLocationData::Turn {
                        turn: 1,
                        key: "summary".to_owned(),
                        value: Rc::new(Value::Null),
                    }),
                    next: None,
                },
                ConversationLocationDataChange {
                    owner: "c".to_owned(),
                    previous: None,
                    next: Some(ConversationLocationData::Turn {
                        turn: 1,
                        key: "summary".to_owned(),
                        value: transferred.clone(),
                    }),
                },
            ])
            .unwrap()
    );
    assert!(Rc::ptr_eq(
        &turn_store.get("summary").unwrap(),
        &transferred
    ));
    assert_eq!(
        index
            .apply_data(&[ConversationLocationDataChange {
                owner: "other".to_owned(),
                previous: None,
                next: Some(ConversationLocationData::Turn {
                    turn: 1,
                    key: "summary".to_owned(),
                    value: Rc::new(json!(3)),
                }),
            }])
            .unwrap_err()
            .to_string(),
        "conversation Location data \"summary\" is already owned by c"
    );
    assert!(
        index
            .replace_data(&[])
            .expect("complete replacement clears omitted values")
    );
    assert!(turn_store.get("summary").is_none());
    assert!(step_store.get("usage").is_none());
}

#[test]
fn step_scoped_data_without_a_step_fails_at_the_publication_edge() {
    let index = ConversationLocationIndex::default();
    let error = index
        .replace_data(&[ConversationOwnedLocationData {
            owner: "a".to_owned(),
            data: ConversationLocationData::Step {
                turn: 1,
                step: None,
                key: "usage".to_owned(),
                value: Rc::new(json!(1)),
            },
        }])
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "conversation Step data \"usage\" requires a step"
    );
}
