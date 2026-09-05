//! Exact request-path mirror of the source approval service oracle.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply};
use seekdeep_core::{
    session::{AppendOptions, Session, SessionEvent, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{AbortSignal, CallId};
use seekdeep_scope::{ScopeKey, create_scope};
use seekdeep_user_approval::{
    APPROVAL, ApprovalAnswer, ApprovalConfig, ApprovalOutcome, ApprovalRequest, install,
};
use serde_json::json;

static NEXT_SESSION: AtomicUsize = AtomicUsize::new(1);

fn detached_session(state: TurnState) -> Arc<Session> {
    let id = SessionId::new(format!(
        "approval-{}",
        NEXT_SESSION.fetch_add(1, Ordering::Relaxed)
    ));
    let session = Session::create(&id, None, None).expect("session");
    if state != TurnState::Idle {
        session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("turn start");
    }
    if state == TurnState::Closed {
        session
            .append(
                "turn/end",
                json!({"turn": 1, "reason": {"kind": "completed"}}),
                AppendOptions::default(),
            )
            .expect("turn end");
    }
    session
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TurnState {
    Idle,
    Open,
    Closed,
}

fn request(session: Arc<Session>, scope: ScopeKey) -> ApprovalRequest {
    ApprovalRequest::new(agent(session, scope), "echo")
}

fn agent(session: Arc<Session>, scope: ScopeKey) -> Arc<Agent> {
    let inbox =
        Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
    Arc::new(Agent::new(
        session.id().clone(),
        AgentOptions::default(),
        session,
        inbox,
        Context::new(),
        scope,
    ))
}

fn mounted() -> (Context, seekdeep_user_approval::ApprovalInstallation) {
    let context = Context::new();
    let installation = install(&context, ApprovalConfig::default()).expect("approval install");
    (context, installation)
}

fn audit(session: &Session) -> Vec<SessionEvent> {
    session
        .events()
        .into_iter()
        .filter(|event| event.event_type.starts_with("approval/"))
        .collect()
}

#[tokio::test]
async fn idle_ask_throws_before_appending_anything() {
    let (_, approval) = mounted();
    let session = detached_session(TurnState::Idle);
    let error = approval
        .request(request(session.clone(), ScopeKey::new()))
        .await
        .expect_err("idle ask");
    assert!(error.to_string().contains("outside an open turn"));
    assert!(audit(&session).is_empty());
}

#[tokio::test]
async fn ask_between_turns_fails_the_enclosure_precondition() {
    let (_, approval) = mounted();
    let session = detached_session(TurnState::Closed);
    let error = approval
        .request(request(session.clone(), ScopeKey::new()))
        .await
        .expect_err("closed ask");
    assert!(error.to_string().contains("outside an open turn"));
    assert!(audit(&session).is_empty());
}

#[tokio::test]
async fn no_listener_fails_closed_and_audits_the_pair() {
    let (_, approval) = mounted();
    let session = detached_session(TurnState::Open);
    let outcome = approval
        .request(
            request(session.clone(), ScopeKey::new())
                .with_call_id(CallId::new("call-1"))
                .with_reason("hook says ask"),
        )
        .await
        .expect("request");
    assert_eq!(outcome, ApprovalOutcome::Unavailable);
    let audit = audit(&session);
    assert_eq!(
        audit
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["approval/asked", "approval/decided"]
    );
    assert_eq!(audit[0].data["toolName"], json!("echo"));
    assert_eq!(audit[0].data["callId"], json!("call-1"));
    assert_eq!(audit[0].data["reason"], json!("hook says ask"));
    assert_eq!(audit[1].data["outcome"], json!("unavailable"));
    assert_eq!(audit[1].data["id"], audit[0].data["id"]);
}

#[tokio::test]
async fn absent_optional_fields_are_omitted_from_the_asked_event() {
    let (_, approval) = mounted();
    let session = detached_session(TurnState::Open);
    approval
        .request(request(session.clone(), ScopeKey::new()))
        .await
        .expect("request");
    let audit = audit(&session);
    let mut keys = audit[0]
        .data
        .as_object()
        .expect("asked data")
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(keys, ["id", "toolName"]);
}

#[tokio::test]
async fn scoped_dispatch_borrows_the_exact_subject_fields_and_audits_them() {
    let (context, approval) = mounted();
    let scope_key = ScopeKey::new();
    let scope = create_scope(&context, scope_key, None).expect("scope");
    let session = detached_session(TurnState::Open);
    let received = Arc::new(Mutex::new(None));
    let answer_received = received.clone();
    approval
        .on_request(
            &scope.context,
            move |request, _| {
                *answer_received.lock() = Some(request);
                async { Ok(ApprovalOutcome::AllowedOnce.into()) }
            },
            EventOptions::default(),
        )
        .expect("answerer");
    let signal = AbortSignal::default();
    let outcome = approval
        .request(
            request(session.clone(), scope_key)
                .with_call_id(CallId::new("scoped-call"))
                .with_reason("scoped reason")
                .with_signal(signal.clone()),
        )
        .await
        .expect("request");
    assert_eq!(outcome, ApprovalOutcome::AllowedOnce);
    let received = received.lock();
    let received = received.as_ref().expect("received");
    assert_eq!(received.agent.scope_key(), scope_key);
    assert!(Arc::ptr_eq(received.agent.session(), &session));
    assert_eq!(received.tool_name, "echo");
    assert_eq!(
        received.call_id.as_ref().map(CallId::as_str),
        Some("scoped-call")
    );
    assert_eq!(received.reason.as_deref(), Some("scoped reason"));
    assert_eq!(received.signal.as_ref(), Some(&signal));
    let audit = audit(&session);
    assert_eq!(audit[0].data["callId"], json!("scoped-call"));
    assert_eq!(audit[0].data["reason"], json!("scoped reason"));
    assert_eq!(audit[1].data["outcome"], json!("allowed-once"));
}

fn live_session(context: &Context, id: &str) -> Arc<Session> {
    let store = context
        .get(seekdeep_core::session_store::SESSIONS)
        .expect("session store");
    let session = store
        .create(
            context,
            Some(SessionId::new(id)),
            CreateSessionOptions::default(),
        )
        .expect("live session");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("turn start");
    session
}

#[tokio::test]
async fn asked_observer_failure_after_commit_does_not_break_the_pair() {
    let context = Context::new();
    SessionStore::install(&context).expect("sessions");
    let approval = install(&context, ApprovalConfig::default()).expect("approval");
    let session = live_session(&context, "asked-observer");
    context
        .events()
        .on_sync(
            &context,
            "session/event",
            |_, args| {
                let event = args.get::<SessionEvent>(1).expect("event");
                if event.event_type == "approval/asked" {
                    anyhow::bail!("observer failed after asked append");
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .expect("observer");
    approval
        .on_request(
            &context,
            |_, _| async { Ok(ApprovalOutcome::AllowedOnce.into()) },
            EventOptions::default(),
        )
        .expect("answerer");
    assert_eq!(
        approval
            .request(request(session.clone(), ScopeKey::new()))
            .await
            .expect("request"),
        ApprovalOutcome::AllowedOnce
    );
    let audit = audit(&session);
    assert_eq!(audit.len(), 2);
    assert_eq!(audit[1].data["id"], audit[0].data["id"]);
}

#[tokio::test]
async fn decided_observer_failure_after_commit_does_not_change_the_outcome() {
    let context = Context::new();
    SessionStore::install(&context).expect("sessions");
    let approval = install(&context, ApprovalConfig::default()).expect("approval");
    let session = live_session(&context, "decided-observer");
    context
        .events()
        .on_sync(
            &context,
            "session/event",
            |_, args| {
                let event = args.get::<SessionEvent>(1).expect("event");
                if event.event_type == "approval/decided" {
                    anyhow::bail!("observer failed after decided append");
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .expect("observer");
    approval
        .on_request(
            &context,
            |_, _| async { Ok(ApprovalOutcome::Rejected.into()) },
            EventOptions::default(),
        )
        .expect("answerer");
    assert_eq!(
        approval
            .request(request(session.clone(), ScopeKey::new()))
            .await
            .expect("request"),
        ApprovalOutcome::Rejected
    );
    assert_eq!(audit(&session)[1].data["outcome"], json!("rejected"));
}

#[tokio::test]
async fn precommit_append_failure_propagates_without_log_growth() {
    let context = Context::new();
    SessionStore::install(&context).expect("sessions");
    let approval = install(&context, ApprovalConfig::default()).expect("approval");
    let session = live_session(&context, "append-failure");
    context
        .events()
        .on_sync(
            &context,
            "internal/dispatch",
            |_, args| {
                let name = args.get::<String>(1).expect("event name");
                if name.as_str() == "session/event" {
                    let carried = args.get::<EventArgs>(2).expect("event args");
                    let event = carried.get::<SessionEvent>(1).expect("session event");
                    if event.event_type == "approval/asked" {
                        anyhow::bail!("append failed before log growth");
                    }
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .expect("precommit failure");
    let error = approval
        .request(request(session.clone(), ScopeKey::new()))
        .await
        .expect_err("append failure");
    assert!(
        error
            .to_string()
            .contains("append failed before log growth")
    );
    assert!(audit(&session).is_empty());
}

#[tokio::test]
async fn first_answering_listener_owns_the_single_decision_slot() {
    let (context, approval) = mounted();
    let second_ran = Arc::new(AtomicBool::new(false));
    approval
        .on_request(
            &context,
            |_, _| async { Ok(ApprovalOutcome::AllowedOnce.into()) },
            EventOptions::default(),
        )
        .expect("first");
    let second_flag = second_ran.clone();
    approval
        .on_request(
            &context,
            move |_, _| {
                second_flag.store(true, Ordering::SeqCst);
                async { Ok(ApprovalOutcome::Rejected.into()) }
            },
            EventOptions::default(),
        )
        .expect("second");
    let outcome = approval
        .request(request(detached_session(TurnState::Open), ScopeKey::new()))
        .await
        .expect("request");
    assert_eq!(outcome, ApprovalOutcome::AllowedOnce);
    assert!(!second_ran.load(Ordering::SeqCst));
}

#[tokio::test]
async fn non_owning_listener_delegates_to_the_unavailable_default() {
    let (context, approval) = mounted();
    approval
        .on_request(
            &context,
            |_, next| async move { next.run().await },
            EventOptions::default(),
        )
        .expect("delegating listener");
    let outcome = approval
        .request(request(detached_session(TurnState::Open), ScopeKey::new()))
        .await
        .expect("request");
    assert_eq!(outcome, ApprovalOutcome::Unavailable);
}

#[tokio::test]
async fn dispatch_selects_global_and_matching_scope_never_foreign_scope() {
    let (context, approval) = mounted();
    let a = ScopeKey::new();
    let b = ScopeKey::new();
    let scope_a = create_scope(&context, a, None).expect("scope a");
    let scope_b = create_scope(&context, b, None).expect("scope b");
    let heard = Arc::new(Mutex::new(Vec::new()));
    let global_heard = heard.clone();
    approval
        .on_request(
            &context,
            move |request, next| {
                global_heard.lock().push(if request.agent.scope_key() == a {
                    "global:A"
                } else {
                    "global:B"
                });
                async move { next.run().await }
            },
            EventOptions::default(),
        )
        .expect("global");
    for (scope, label) in [(&scope_a, "scoped:A"), (&scope_b, "scoped:B")] {
        let scoped_heard = heard.clone();
        approval
            .on_request(
                &scope.context,
                move |_, next| {
                    scoped_heard.lock().push(label);
                    async move { next.run().await }
                },
                EventOptions::default(),
            )
            .expect("scoped");
    }
    for scope in [a, b] {
        assert_eq!(
            approval
                .request(request(detached_session(TurnState::Open), scope))
                .await
                .expect("request"),
            ApprovalOutcome::Unavailable
        );
    }
    assert_eq!(
        *heard.lock(),
        ["global:A", "scoped:A", "global:B", "scoped:B"]
    );
}

#[tokio::test]
async fn scoped_carrier_is_keyed_to_the_exact_request_subject() {
    let (context, approval) = mounted();
    let key = ScopeKey::new();
    let scope = create_scope(&context, key, None).expect("scope");
    let seen = Arc::new(Mutex::new(None));
    let answer_seen = seen.clone();
    approval
        .on_request(
            &scope.context,
            move |request, next| {
                *answer_seen.lock() = Some(request.agent.scope_key());
                async move { next.run().await }
            },
            EventOptions::default(),
        )
        .expect("scoped");
    approval
        .request(request(detached_session(TurnState::Open), key))
        .await
        .expect("request");
    assert_eq!(*seen.lock(), Some(key));
}

#[tokio::test]
async fn throwing_answerer_is_contained_as_unavailable() {
    let (context, approval) = mounted();
    approval
        .on_request(
            &context,
            |_, _| async { anyhow::bail!("transport died") },
            EventOptions::default(),
        )
        .expect("answerer");
    let session = detached_session(TurnState::Open);
    assert_eq!(
        approval
            .request(request(session.clone(), ScopeKey::new()))
            .await
            .expect("contained"),
        ApprovalOutcome::Unavailable
    );
    assert_eq!(audit(&session)[1].data["outcome"], json!("unavailable"));
}

#[tokio::test]
async fn rogue_non_vocabulary_answer_normalizes_to_unavailable() {
    let (context, approval) = mounted();
    approval
        .on_request(
            &context,
            |_, _| async { Ok(ApprovalAnswer::Unknown("yolo".to_owned())) },
            EventOptions::default(),
        )
        .expect("rogue");
    assert_eq!(
        approval
            .request(request(detached_session(TurnState::Open), ScopeKey::new(),))
            .await
            .expect("request"),
        ApprovalOutcome::Unavailable
    );
}

#[tokio::test]
async fn preaborted_signal_cancels_without_asking() {
    let (context, approval) = mounted();
    let asked = Arc::new(AtomicBool::new(false));
    let answer_asked = asked.clone();
    approval
        .on_request(
            &context,
            move |_, _| {
                answer_asked.store(true, Ordering::SeqCst);
                async { Ok(ApprovalOutcome::AllowedOnce.into()) }
            },
            EventOptions::default(),
        )
        .expect("answerer");
    let signal = AbortSignal::default();
    signal.abort();
    let session = detached_session(TurnState::Open);
    assert_eq!(
        approval
            .request(request(session.clone(), ScopeKey::new()).with_signal(signal))
            .await
            .expect("request"),
        ApprovalOutcome::Cancelled
    );
    assert!(!asked.load(Ordering::SeqCst));
    assert_eq!(audit(&session)[1].data["outcome"], json!("cancelled"));
}

#[tokio::test]
async fn mid_question_abort_cancels_and_discards_late_answer() {
    let (context, approval) = mounted();
    let entered = Arc::new(tokio::sync::Notify::new());
    let answer_entered = entered.clone();
    let (settle_late, late_answer) = tokio::sync::oneshot::channel();
    let late_answer = Arc::new(Mutex::new(Some(late_answer)));
    let answer_late = late_answer.clone();
    approval
        .on_request(
            &context,
            move |_, _| {
                answer_entered.notify_one();
                let receiver = answer_late.lock().take().expect("one request");
                async move {
                    receiver.await.expect("late answer signal");
                    Ok(ApprovalOutcome::AllowedOnce.into())
                }
            },
            EventOptions::default(),
        )
        .expect("pending answerer");
    let signal = AbortSignal::default();
    let session = detached_session(TurnState::Open);
    let pending =
        approval.request(request(session.clone(), ScopeKey::new()).with_signal(signal.clone()));
    tokio::pin!(pending);
    tokio::select! {
        () = entered.notified() => {}
        result = &mut pending => panic!("settled before abort: {result:?}"),
    }
    signal.abort();
    assert_eq!(pending.await.expect("request"), ApprovalOutcome::Cancelled);
    settle_late.send(()).expect("settle detached answer");
    tokio::task::yield_now().await;
    assert_eq!(
        audit(&session)
            .iter()
            .filter(|event| event.event_type == "approval/decided")
            .count(),
        1
    );
}

#[tokio::test]
async fn late_answerer_failure_after_abort_is_contained_by_future_cancellation() {
    let (context, approval) = mounted();
    let entered = Arc::new(tokio::sync::Notify::new());
    let answer_entered = entered.clone();
    let (reject_late, late_failure) = tokio::sync::oneshot::channel();
    let late_failure = Arc::new(Mutex::new(Some(late_failure)));
    let answer_late = late_failure.clone();
    approval
        .on_request(
            &context,
            move |_, _| {
                answer_entered.notify_one();
                let receiver = answer_late.lock().take().expect("one request");
                async move {
                    receiver.await.expect("late rejection signal");
                    anyhow::bail!("answered too late")
                }
            },
            EventOptions::default(),
        )
        .expect("pending answerer");
    let signal = AbortSignal::default();
    let pending = approval.request(
        request(detached_session(TurnState::Open), ScopeKey::new()).with_signal(signal.clone()),
    );
    tokio::pin!(pending);
    tokio::select! {
        () = entered.notified() => {}
        result = &mut pending => panic!("settled before abort: {result:?}"),
    }
    signal.abort();
    assert_eq!(pending.await.expect("request"), ApprovalOutcome::Cancelled);
    reject_late.send(()).expect("settle detached rejection");
    tokio::task::yield_now().await;
}

#[tokio::test]
async fn non_aborting_signal_returns_the_answer() {
    let (context, approval) = mounted();
    approval
        .on_request(
            &context,
            |_, _| async { Ok(ApprovalOutcome::Rejected.into()) },
            EventOptions::default(),
        )
        .expect("answerer");
    assert_eq!(
        approval
            .request(
                request(detached_session(TurnState::Open), ScopeKey::new())
                    .with_signal(AbortSignal::default())
            )
            .await
            .expect("request"),
        ApprovalOutcome::Rejected
    );
}

#[tokio::test]
async fn every_request_issues_a_fresh_id() {
    let (_, approval) = mounted();
    let session = detached_session(TurnState::Open);
    for _ in 0..2 {
        approval
            .request(request(session.clone(), ScopeKey::new()))
            .await
            .expect("request");
    }
    let ids = audit(&session)
        .into_iter()
        .filter(|event| event.event_type == "approval/asked")
        .map(|event| event.data["id"].clone())
        .collect::<Vec<_>>();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
}

#[tokio::test]
async fn disposed_answerer_drops_out_of_the_chain() {
    let (context, approval) = mounted();
    let answerer = approval
        .on_request(
            &context,
            |_, _| async { Ok(ApprovalOutcome::AllowedOnce.into()) },
            EventOptions::default(),
        )
        .expect("answerer");
    assert_eq!(
        approval
            .request(request(detached_session(TurnState::Open), ScopeKey::new(),))
            .await
            .expect("first"),
        ApprovalOutcome::AllowedOnce
    );
    answerer.dispose().await.expect("dispose answerer");
    assert_eq!(
        approval
            .request(request(detached_session(TurnState::Open), ScopeKey::new(),))
            .await
            .expect("second"),
        ApprovalOutcome::Unavailable
    );
}

#[test]
fn service_is_published_under_the_exact_typed_key() {
    let (context, approval) = mounted();
    assert!(Arc::ptr_eq(
        &context.get(APPROVAL).expect("service"),
        &approval.service()
    ));
}
