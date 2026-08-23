//! Pending carrier, authoritative queue, and durable steering replay parity.

use std::{cell::RefCell, rc::Rc};

use futures::FutureExt;
use seekdeep_client_runtime::*;
use seekdeep_identity::{MessageId, RpcId, SessionId};
use serde_json::{Value, json};

#[test]
fn pending_wait_hides_rpc_id_backfills_response_and_fails_after_settlement() {
    let responses = Rc::new(RefCell::new(Vec::new()));
    let observed = responses.clone();
    let wait = PendingWait::new(
        PendingKind::Approval,
        RpcId::new("rpc-1"),
        SessionId::new("session-1"),
        json!({"toolName":"bash"}),
        Rc::new(move |response| {
            observed.borrow_mut().push(response);
            async { Ok(json!({"accepted":true})) }.boxed_local()
        }),
    );
    assert_eq!(wait.key, "a:rpc-1");
    assert_eq!(wait.kind, PendingKind::Approval);
    let receipt = futures::executor::block_on(wait.respond(json!({"ok":true})).unwrap()).unwrap();
    assert_eq!(receipt, json!({"accepted":true}));
    assert_eq!(responses.borrow()[0].rpc_id.as_str(), "rpc-1");
    assert_eq!(responses.borrow()[0].result, json!({"ok":true}));
    wait.mark_settled();
    let Err(error) = wait.respond(json!({"ok":false})) else {
        panic!("settled wait unexpectedly responded");
    };
    assert_eq!(error.to_string(), "pending wait a:rpc-1 is already settled");

    let question = PendingWait::new(
        PendingKind::Question,
        RpcId::new("rpc-2"),
        SessionId::new("session-1"),
        Value::Null,
        Rc::new(|_| async { Ok(Value::Null) }.boxed_local()),
    );
    assert_eq!(question.key, "q:rpc-2");
}

fn text(value: &str) -> Value {
    json!({"type":"text","text":value})
}

fn queue_item(
    id: &str,
    message_id: &str,
    placement: QueuePlacement,
    content: Vec<Value>,
) -> QueueItemInput {
    QueueItemInput {
        id: id.to_owned(),
        message_id: MessageId::new(message_id),
        placement,
        content,
    }
}

#[test]
fn queue_replacement_projects_preview_editable_text_order_and_membership() {
    let mut queue = SessionQueueMirror::default();
    queue.replace(&[
        queue_item(
            "q-1",
            "m-1",
            QueuePlacement::Queued,
            vec![text("第一条  排队\n消息")],
        ),
        queue_item(
            "q-image",
            "m-2",
            QueuePlacement::Queued,
            vec![text("hi"), json!({"type":"image","data":"x"})],
        ),
    ]);
    let first = queue.snapshot();
    assert_eq!(first[0].preview, "第一条 排队 消息");
    assert_eq!(first[0].text.as_deref(), Some("第一条  排队\n消息"));
    assert_eq!(first[1].preview, "hi [image]");
    assert_eq!(first[1].text, None);
    assert!(Rc::ptr_eq(&first, &queue.snapshot()));

    queue.replace(&[queue_item(
        "q-2",
        "m-3",
        QueuePlacement::Queued,
        vec![text("edited")],
    )]);
    assert_eq!(queue.snapshot()[0].id, "q-2");
    assert_eq!(queue.snapshot().len(), 1);
    queue.replace(&[]);
    assert!(queue.snapshot().is_empty());
}

#[test]
fn queue_preview_caps_unicode_and_durable_handoff_retires_one_current_steering_occurrence() {
    let mut queue = SessionQueueMirror::default();
    let body = "长".repeat(201);
    queue.replace(&[queue_item(
        "long",
        "long-message",
        QueuePlacement::Queued,
        vec![text(&body)],
    )]);
    assert_eq!(queue.snapshot()[0].preview.chars().count(), 201);
    assert!(queue.snapshot()[0].preview.ends_with('…'));
    assert_eq!(queue.snapshot()[0].text.as_deref(), Some(body.as_str()));

    queue.replace(&[
        queue_item("first", "same", QueuePlacement::Steering, vec![text("x")]),
        queue_item("second", "same", QueuePlacement::Steering, vec![text("x")]),
    ]);
    assert!(queue.accept_durable_user_message(&MessageId::new("same")));
    assert_eq!(queue.snapshot()[0].id, "second");
    assert!(queue.reset());
    assert!(!queue.reset());
}

#[test]
fn steering_history_reconstructs_claims_cancellation_user_source_and_reset() {
    let mut history = SteeringHistory::default();
    history.apply(&SteeringHistoryEvent::Splice(InboxSplice {
        target: InboxTarget::NextStep,
        start: 0,
        removed_count: 0,
        inserted: vec![PendingIdentity {
            id: "m1".to_owned(),
        }],
        canceled: false,
    }));
    history.apply(&SteeringHistoryEvent::Splice(InboxSplice {
        target: InboxTarget::NextStep,
        start: 0,
        removed_count: 1,
        inserted: Vec::new(),
        canceled: false,
    }));
    assert!(history.apply(&SteeringHistoryEvent::UserMessage {
        id: "m1".to_owned(),
        source_kind: "user".to_owned(),
    }));
    assert!(!history.apply(&SteeringHistoryEvent::UserMessage {
        id: "m1".to_owned(),
        source_kind: "user".to_owned(),
    }));

    history.apply(&SteeringHistoryEvent::Splice(InboxSplice {
        target: InboxTarget::NextStep,
        start: 0,
        removed_count: 0,
        inserted: vec![PendingIdentity {
            id: "m2".to_owned(),
        }],
        canceled: false,
    }));
    history.apply(&SteeringHistoryEvent::Splice(InboxSplice {
        target: InboxTarget::NextStep,
        start: 0,
        removed_count: 1,
        inserted: Vec::new(),
        canceled: true,
    }));
    assert!(!history.apply(&SteeringHistoryEvent::UserMessage {
        id: "m2".to_owned(),
        source_kind: "user".to_owned(),
    }));

    history.apply(&SteeringHistoryEvent::Splice(InboxSplice {
        target: InboxTarget::NextTurn,
        start: 0,
        removed_count: 0,
        inserted: vec![PendingIdentity {
            id: "queued".to_owned(),
        }],
        canceled: false,
    }));
    assert!(!history.apply(&SteeringHistoryEvent::Other));
    history.reset();
    assert!(!history.apply(&SteeringHistoryEvent::UserMessage {
        id: "queued".to_owned(),
        source_kind: "user".to_owned(),
    }));
}
