//! Question projection, draft-flow, and pending-carrier parity.

#![cfg(not(target_arch = "wasm32"))]

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use futures::FutureExt as _;
use seekdeep_client_runtime::{PendingClientResponse, PendingKind, PendingResponder, PendingWait};
use seekdeep_client_ui_user_questions::{
    INJECT, LOCALE_NAMESPACE, PendingQuestion, PlanReview, QUESTION_EN, QUESTION_ZH, QuestionBusy,
    QuestionFeedback, QuestionFlow, QuestionFlowEffect, RecommendedLabel, parse_recommended_label,
    plan_review_of,
};
use seekdeep_identity::{RpcId, SessionId};
use seekdeep_user_questions_contract::{
    AskUserQuestionAnswer, AskUserQuestionAnswerItem, AskUserQuestionIntent, AskUserQuestionItem,
    AskUserQuestionOption,
};
use serde_json::{Map, Value, json};

fn option(label: &str, description: Option<&str>) -> AskUserQuestionOption {
    AskUserQuestionOption {
        label: label.to_owned(),
        description: description.map(ToOwned::to_owned),
    }
}

fn questions() -> Vec<AskUserQuestionItem> {
    vec![
        AskUserQuestionItem {
            id: "profile".to_owned(),
            header: Some("偏好".to_owned()),
            question: "选择候选人类型".to_owned(),
            detail: Some("按当前空缺岗位的优先级选择。".to_owned()),
            options: Some(vec![
                option("工程落地型 (Recommended)", Some("优先工程交付。")),
                option("研究潜力型", Some("优先研究能力。")),
            ]),
            multi_select: None,
            intent: None,
        },
        AskUserQuestionItem {
            id: "detail".to_owned(),
            question: "补充你的要求".to_owned(),
            detail: None,
            header: None,
            options: None,
            multi_select: None,
            intent: None,
        },
        AskUserQuestionItem {
            id: "signals".to_owned(),
            question: "选择重要信号（可多选）".to_owned(),
            detail: None,
            header: None,
            options: Some(vec![
                option("系统设计", None),
                option("代码质量", None),
                option("产品判断", None),
            ]),
            multi_select: Some(true),
            intent: None,
        },
    ]
}

fn plan_questions() -> Vec<AskUserQuestionItem> {
    vec![AskUserQuestionItem {
        id: "plan-review".to_owned(),
        header: Some("Plan review".to_owned()),
        question: "Approve this plan and leave plan mode?".to_owned(),
        detail: Some("# Ship the picker\n\n- read the store\n- render the rows\n".to_owned()),
        options: Some(vec![
            option(
                "Approve",
                Some("Leave plan mode; the plan is carried out from the next step."),
            ),
            option(
                "Keep planning",
                Some("Stay in plan mode; feedback goes back to the model."),
            ),
        ]),
        multi_select: None,
        intent: Some(AskUserQuestionIntent {
            kind: "plan-review".to_owned(),
            approve: "Approve".to_owned(),
            extra: Map::new(),
        }),
    }]
}

#[test]
fn locale_namespace_dependency_order_and_copy_are_exact() {
    assert_eq!(INJECT, &["slots", "locale"]);
    assert_eq!(LOCALE_NAMESPACE, "question");
    assert_eq!(QUESTION_ZH.len(), 13);
    assert_eq!(QUESTION_EN.len(), 13);
    assert_eq!(
        QUESTION_ZH.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
        QUESTION_EN.iter().map(|(key, _)| *key).collect::<Vec<_>>()
    );
    assert_eq!(QUESTION_ZH[0], ("error.incomplete", "请先完成这道问题。"));
    assert_eq!(QUESTION_ZH[12], ("plan.discuss", "去聊天里说"));
    assert_eq!(QUESTION_EN[4], ("nav.cancel", "Dismiss all questions"));
    assert_eq!(QUESTION_EN[12], ("plan.discuss", "Chat about it"));
}

#[test]
fn recommendation_suffixes_are_display_only() {
    for (input, label, recommended) in [
        ("Fast (Recommended)", "Fast", true),
        ("Fast (recommended)  ", "Fast", true),
        ("稳妥（推荐）", "稳妥", true),
        ("稳妥 (推荐)", "稳妥", true),
        ("Plain", "Plain", false),
        ("Recommended choice", "Recommended choice", false),
    ] {
        assert_eq!(
            parse_recommended_label(input),
            RecommendedLabel {
                label: label.to_owned(),
                recommended,
            }
        );
    }
}

#[test]
fn plan_review_claims_only_one_reachable_binary_single_choice() {
    let source = plan_questions();
    assert_eq!(
        plan_review_of(&source),
        Some(PlanReview {
            id: "plan-review".to_owned(),
            question: "Approve this plan and leave plan mode?".to_owned(),
            plan: "# Ship the picker\n\n- read the store\n- render the rows\n".to_owned(),
            approve: source[0].options.as_ref().unwrap()[0].clone(),
            decline: Some(source[0].options.as_ref().unwrap()[1].clone()),
        })
    );

    let mut approve_only = source.clone();
    approve_only[0].options = Some(vec![option("Approve", None)]);
    assert_eq!(plan_review_of(&approve_only).unwrap().decline, None);

    let mut cases = Vec::new();
    cases.push(Vec::new());
    cases.push(vec![source[0].clone(), source[0].clone()]);
    let mut no_intent = source.clone();
    no_intent[0].intent = None;
    cases.push(no_intent);
    let mut no_detail = source.clone();
    no_detail[0].detail = None;
    cases.push(no_detail);
    let mut missing_approve = source.clone();
    missing_approve[0].intent.as_mut().unwrap().approve = "Ship it".to_owned();
    cases.push(missing_approve);
    let mut no_options = source.clone();
    no_options[0].options = None;
    cases.push(no_options);
    let mut third_option = source.clone();
    third_option[0]
        .options
        .as_mut()
        .unwrap()
        .push(option("Start over", None));
    cases.push(third_option);
    let mut multi = source;
    multi[0].multi_select = Some(true);
    cases.push(multi);

    for case in cases {
        assert_eq!(plan_review_of(&case), None);
    }
}

#[test]
fn generic_flow_batches_single_custom_and_multi_select_answers() {
    let mut flow = QuestionFlow::new(questions());
    assert_eq!(flow.index(), 0);
    flow.choose("工程落地型 (Recommended)");
    assert_eq!(flow.index(), 1);
    flow.set_custom("要能独立排查线上问题");
    assert_eq!(flow.continue_flow(), QuestionFlowEffect::None);
    assert_eq!(flow.index(), 2);
    flow.choose("系统设计");
    flow.choose("系统设计");
    flow.choose("系统设计");
    flow.choose("代码质量");
    flow.set_custom("沟通能力");
    flow.choose("产品判断");
    assert_eq!(
        flow.continue_flow(),
        QuestionFlowEffect::Answer(AskUserQuestionAnswer {
            answers: vec![
                AskUserQuestionAnswerItem {
                    id: "profile".to_owned(),
                    selected: vec!["工程落地型 (Recommended)".to_owned()],
                    custom: None,
                },
                AskUserQuestionAnswerItem {
                    id: "detail".to_owned(),
                    selected: Vec::new(),
                    custom: Some("要能独立排查线上问题".to_owned()),
                },
                AskUserQuestionAnswerItem {
                    id: "signals".to_owned(),
                    selected: vec![
                        "系统设计".to_owned(),
                        "代码质量".to_owned(),
                        "产品判断".to_owned(),
                    ],
                    custom: Some("沟通能力".to_owned()),
                },
            ],
        })
    );
    assert_eq!(flow.busy(), Some(QuestionBusy::Answer));
    assert!(flow.disabled());
}

#[test]
fn skip_validation_navigation_and_failures_match_component_state() {
    let mut flow = QuestionFlow::new(questions());
    assert_eq!(flow.continue_flow(), QuestionFlowEffect::None);
    assert_eq!(flow.feedback(), Some(&QuestionFeedback::Unanswered));
    assert_eq!(
        flow.feedback().unwrap().locale_key(),
        Some("error.unanswered")
    );

    flow.choose("研究潜力型");
    assert_eq!(flow.index(), 1);
    assert_eq!(flow.skip(), QuestionFlowEffect::None);
    assert_eq!(flow.index(), 2);
    flow.choose("产品判断");
    flow.previous();
    assert_eq!(flow.index(), 1);
    flow.previous();
    assert_eq!(flow.index(), 0);
    flow.next();
    flow.next();
    let effect = flow.continue_flow();
    assert!(matches!(effect, QuestionFlowEffect::Answer(_)));

    flow.fail("网络中断");
    assert_eq!(
        flow.feedback(),
        Some(&QuestionFeedback::Text("网络中断".to_owned()))
    );
    assert_eq!(flow.feedback().unwrap().locale_key(), None);
    assert!(!flow.disabled());
    flow.begin_cancel();
    assert_eq!(flow.busy(), Some(QuestionBusy::Cancel));
    assert_eq!(flow.feedback(), None);
}

#[test]
fn missing_batch_item_returns_to_first_gap_and_single_custom_replaces_selection() {
    let mut flow = QuestionFlow::new(questions());
    flow.choose("工程落地型 (Recommended)");
    flow.next();
    flow.choose("产品判断");
    assert_eq!(flow.continue_flow(), QuestionFlowEffect::None);
    assert_eq!(flow.index(), 1);
    assert_eq!(flow.feedback(), Some(&QuestionFeedback::Incomplete));
    assert_eq!(
        flow.feedback().unwrap().locale_key(),
        Some("error.incomplete")
    );

    flow.previous();
    assert_eq!(flow.index(), 0);
    flow.set_custom("  replacement  ");
    assert!(flow.draft().selected.is_empty());
    flow.next();
    flow.set_custom("  ");
    assert!(!flow.current_answered());
}

fn pending_fixture(
    receipt: Value,
) -> (
    PendingQuestion,
    Rc<RefCell<Vec<PendingClientResponse>>>,
    Rc<PendingWait>,
) {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let observed = calls.clone();
    let responder: PendingResponder = Rc::new(move |response| {
        observed.borrow_mut().push(response);
        let receipt = receipt.clone();
        async move { Ok(receipt) }.boxed_local()
    });
    let wait = Rc::new(PendingWait::new(
        PendingKind::Question,
        RpcId::new("rq"),
        SessionId::new("s1"),
        serde_json::to_value(BTreeMap::from([("questions", questions())])).unwrap(),
        responder,
    ));
    (PendingQuestion::new(wait.clone()), calls, wait)
}

#[tokio::test(flavor = "current_thread")]
async fn pending_domain_face_encodes_answers_cancellation_and_negative_receipts() {
    let (pending, calls, wait) = pending_fixture(json!({"accepted": true}));
    assert_eq!(pending.key(), "q:rq");
    assert_eq!(pending.session_id().as_str(), "s1");
    assert_eq!(pending.questions().unwrap(), questions());
    let answer = AskUserQuestionAnswer {
        answers: vec![AskUserQuestionAnswerItem {
            id: "mode".to_owned(),
            selected: vec!["Fast".to_owned()],
            custom: None,
        }],
    };
    pending.answer(answer.clone()).await.unwrap();
    assert_eq!(
        calls.borrow()[0].result,
        json!({
            "ok": true,
            "value": {"sessionId": "s1", "answer": answer},
        })
    );
    pending.cancel().await.unwrap();
    assert_eq!(
        calls.borrow()[1].result,
        json!({
            "ok": false,
            "error": {
                "code": "cancelled",
                "message": "the user closed this question request",
                "details": {},
            },
        })
    );
    wait.mark_settled();
    assert_eq!(
        pending.cancel().await.unwrap_err(),
        "pending wait q:rq is already settled"
    );

    let (rejected, _, _) = pending_fixture(json!({"accepted": false, "reason": "not-pending"}));
    assert_eq!(
        rejected
            .answer(AskUserQuestionAnswer {
                answers: Vec::new()
            })
            .await,
        Err("question response rejected: not-pending".to_owned())
    );
    assert_eq!(
        rejected.cancel().await,
        Err("question cancellation rejected: not-pending".to_owned())
    );
}
