//! Target-portable question contract serialization parity.

use seekdeep_user_questions_contract::{
    AskUserQuestionAnswer, AskUserQuestionAnswerItem, AskUserQuestionIntent, AskUserQuestionItem,
    AskUserQuestionOption,
};
use serde_json::{Map, json};

#[test]
fn question_and_answer_wire_names_and_omissions_are_exact() {
    let question = AskUserQuestionItem {
        id: "signals".to_owned(),
        question: "Choose signals".to_owned(),
        detail: None,
        header: Some("Review".to_owned()),
        options: Some(vec![AskUserQuestionOption {
            label: "Correctness".to_owned(),
            description: None,
        }]),
        multi_select: Some(true),
        intent: None,
    };
    assert_eq!(
        serde_json::to_value(&question).unwrap(),
        json!({
            "id": "signals",
            "question": "Choose signals",
            "header": "Review",
            "options": [{"label": "Correctness"}],
            "multiSelect": true,
        })
    );
    let answer = AskUserQuestionAnswer {
        answers: vec![AskUserQuestionAnswerItem {
            id: "signals".to_owned(),
            selected: vec!["Correctness".to_owned()],
            custom: None,
        }],
    };
    assert_eq!(
        serde_json::to_value(answer).unwrap(),
        json!({"answers": [{"id": "signals", "selected": ["Correctness"]}]})
    );
}

#[test]
fn intent_extensions_are_lossless_while_closed_objects_reject_unknown_fields() {
    let value = json!({
        "id": "plan",
        "question": "Review?",
        "intent": {"kind": "future-review", "approve": "Yes", "future": {"x": 1}},
    });
    let parsed: AskUserQuestionItem = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        parsed.intent,
        Some(AskUserQuestionIntent {
            kind: "future-review".to_owned(),
            approve: "Yes".to_owned(),
            extra: Map::from_iter([("future".to_owned(), json!({"x": 1}))]),
        })
    );
    assert_eq!(serde_json::to_value(parsed).unwrap(), value);
    assert!(
        serde_json::from_value::<AskUserQuestionItem>(json!({
            "id": "q",
            "question": "Q?",
            "unexpected": true,
        }))
        .is_err()
    );
}
