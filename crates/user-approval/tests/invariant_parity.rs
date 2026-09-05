//! Exact mirror of `packages/interaction/user-approval/tests/invariant.spec.ts`.

use std::sync::Arc;

use seekdeep_cordis::{Context, EventArgs};
use seekdeep_core::{
    session::{AppendOptions, Session, SessionEvent, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_user_approval::invariant::register_invariant;
use serde_json::json;

async fn setup() -> (Context, Arc<SessionStore>) {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let invariants =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&invariants).expect("approval invariant");
    registration.await_ready().await.expect("invariant ready");
    (context, sessions)
}

fn create(context: &Context, sessions: &SessionStore, id: &str) -> Arc<Session> {
    sessions
        .create(
            context,
            Some(SessionId::new(id)),
            CreateSessionOptions::default(),
        )
        .expect("session")
}

fn start(session: &Session) {
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("turn start");
}

#[tokio::test]
async fn accepts_paired_audit_events_and_closed_policy_values() {
    let (context, sessions) = setup().await;
    let session = create(&context, &sessions, "paired");
    start(&session);
    session
        .append(
            "approval/asked",
            json!({"id": "ask-1", "toolName": "bash"}),
            AppendOptions::default(),
        )
        .expect("asked");
    session
        .append(
            "approval/decided",
            json!({"id": "ask-1", "outcome": "allowed-once"}),
            AppendOptions::default(),
        )
        .expect("decided");
    session
        .append(
            "approval/policy",
            json!({"policy": "never"}),
            AppendOptions::default(),
        )
        .expect("policy");
}

#[tokio::test]
async fn rebuilds_an_unmatched_question_from_an_existing_session() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let session = create(&context, &sessions, "resume");
    start(&session);
    session
        .append(
            "approval/asked",
            json!({"id": "ask-resume", "toolName": "bash"}),
            AppendOptions::default(),
        )
        .expect("asked before invariant");
    let invariants =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&invariants).expect("approval invariant");
    registration.await_ready().await.expect("seed ready");
    session
        .append(
            "approval/decided",
            json!({"id": "ask-resume", "outcome": "cancelled"}),
            AppendOptions::default(),
        )
        .expect("matching decision");
}

#[tokio::test]
async fn adopts_a_bare_session_first_observed_through_publication() {
    let (context, _) = setup().await;
    let session = Session::create(&SessionId::new("bare-approval"), None, None).expect("bare");
    for event in [
        SessionEvent {
            event_type: "turn/start".to_owned(),
            seq: 0,
            time: 0,
            data: json!({"turn": 1}),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        },
        SessionEvent {
            event_type: "approval/asked".to_owned(),
            seq: 1,
            time: 1,
            data: json!({"id": "bare-ask", "toolName": "bash"}),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        },
        SessionEvent {
            event_type: "approval/decided".to_owned(),
            seq: 2,
            time: 2,
            data: json!({"id": "bare-ask", "outcome": "rejected"}),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        },
    ] {
        context
            .events()
            .emit(
                &context,
                "session/event",
                &EventArgs::from_values(vec![session.clone(), Arc::new(event)]),
            )
            .expect("bare publication");
    }
}

#[tokio::test]
async fn rejects_audit_events_outside_any_open_turn() {
    let (context, sessions) = setup().await;
    let session = create(&context, &sessions, "outside");
    let asked = session
        .append(
            "approval/asked",
            json!({"id": "ask-1", "toolName": "bash"}),
            AppendOptions::default(),
        )
        .expect_err("asked outside turn");
    assert!(asked.to_string().contains("outside any open turn"));
    let decided = session
        .append(
            "approval/decided",
            json!({"id": "ask-1", "outcome": "rejected"}),
            AppendOptions::default(),
        )
        .expect_err("decided outside turn");
    assert!(decided.to_string().contains("outside any open turn"));
}

#[tokio::test]
async fn rejects_unenclosed_audit_when_replaying_existing_session() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let session = create(&context, &sessions, "bad-replay");
    start(&session);
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("turn end");
    session
        .append(
            "approval/asked",
            json!({"id": "ask-replay", "toolName": "bash"}),
            AppendOptions::default(),
        )
        .expect("unenforced append");
    let invariants =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&invariants).expect("registration");
    let error = registration
        .await_ready()
        .await
        .expect_err("replay failure");
    assert!(error.to_string().contains("outside any open turn"));
}

#[tokio::test]
async fn rejects_malformed_and_unpaired_audit_events() {
    let (context, sessions) = setup().await;
    let session = create(&context, &sessions, "malformed");
    start(&session);
    let empty = session
        .append(
            "approval/asked",
            json!({"id": "ask-1", "toolName": ""}),
            AppendOptions::default(),
        )
        .expect_err("empty tool");
    assert!(empty.to_string().contains("toolName must be non-empty"));
    session
        .append(
            "approval/asked",
            json!({"id": "ask-1", "toolName": "bash"}),
            AppendOptions::default(),
        )
        .expect("valid ask");
    let repeated = session
        .append(
            "approval/asked",
            json!({"id": "ask-1", "toolName": "bash"}),
            AppendOptions::default(),
        )
        .expect_err("repeated ask");
    assert!(repeated.to_string().contains("repeated open id"));
    let missing = session
        .append(
            "approval/decided",
            json!({"id": "missing", "outcome": "rejected"}),
            AppendOptions::default(),
        )
        .expect_err("missing ask");
    assert!(missing.to_string().contains("no matching approval/asked"));
    let outcome = session
        .append(
            "approval/decided",
            json!({"id": "ask-1", "outcome": "maybe"}),
            AppendOptions::default(),
        )
        .expect_err("unknown outcome");
    assert!(outcome.to_string().contains("unknown outcome"));
    let policy = session
        .append(
            "approval/policy",
            json!({"policy": "always"}),
            AppendOptions::default(),
        )
        .expect_err("unknown policy");
    assert!(policy.to_string().contains("unknown policy"));
}
