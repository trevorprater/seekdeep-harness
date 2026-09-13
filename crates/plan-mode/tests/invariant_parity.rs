//! Behavioral mirror of packages/plan/plan-mode/tests/invariant.spec.ts.

use std::sync::Arc;

use seekdeep_cordis::{Context, EventArgs};
use seekdeep_core::{
    session::{AppendOptions, Session, SessionError, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_plan_mode::invariant::register_invariant;
use serde_json::{Value, json};

async fn setup() -> (Context, Arc<SessionStore>) {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let invariants =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&invariants).expect("register");
    registration.await_ready().await.expect("ready");
    (context, sessions)
}

fn session(context: &Context, sessions: &SessionStore, id: &str) -> Arc<Session> {
    sessions
        .create(
            context,
            Some(SessionId::new(id)),
            CreateSessionOptions::default(),
        )
        .expect("session")
}

/// Appends one plan/mode event with an optional active field (absent models the source's
/// undefined).
fn plan_mode(session: &Session, active: Option<Value>) -> Result<(), SessionError> {
    let data = match active {
        Some(active) => json!({"active": active}),
        None => json!({}),
    };
    session
        .append("plan/mode", data, AppendOptions::default())
        .map(|_| ())
}

#[tokio::test]
async fn accepts_either_boolean_state() {
    let (context, sessions) = setup().await;
    let session = session(&context, &sessions, "plan-state");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("turn/start");
    plan_mode(&session, Some(json!(true))).expect("active true");
    plan_mode(&session, Some(json!(false))).expect("active false");
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("turn/end");
}

#[tokio::test]
async fn rejects_invalid_durable_plan_state() {
    let cases: Vec<(&str, Option<Value>)> = vec![
        ("number", Some(json!(42))),
        ("string", Some(json!("plan"))),
        ("missing", None),
    ];
    for (name, active) in cases {
        let (context, sessions) = setup().await;
        let session = session(&context, &sessions, &format!("invalid-{name}"));
        session
            .append("turn/start", json!({"turn": 1}), AppendOptions::default())
            .expect("turn/start");
        let error = plan_mode(&session, active).expect_err("invalid");
        assert!(
            error.to_string().contains("expected a boolean"),
            "{name}: expected a boolean in {error}"
        );
    }
}

#[tokio::test]
async fn accepts_standalone_plan_state_between_turns() {
    let (context, sessions) = setup().await;
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");
    plan_mode(&session, Some(json!(true))).expect("standalone");
}

#[tokio::test]
async fn ignores_unrelated_dispatches_and_session_events() {
    let (context, sessions) = setup().await;
    let session = session(&context, &sessions, "unrelated");
    context
        .events()
        .emit(&context, "tools/change", &EventArgs::new())
        .expect("unrelated dispatch");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("unrelated session event");
}

#[tokio::test]
async fn rejects_invalid_existing_state_on_late_registration() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("turn/start");
    plan_mode(&session, Some(json!("plan"))).expect("invalid plan/mode");
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("turn/end");
    let invariants =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&invariants).expect("register");
    let error = registration
        .await_ready()
        .await
        .expect_err("late registration");
    assert!(error.to_string().contains("expected a boolean"));
}

#[tokio::test]
async fn replays_enclosed_existing_plan_state_through_its_closing_boundary() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("turn/start");
    plan_mode(&session, Some(json!(true))).expect("valid plan/mode");
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("turn/end");
    let invariants =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&invariants).expect("register");
    registration.await_ready().await.expect("valid replay");
}

#[tokio::test]
async fn accepts_standalone_existing_plan_state_on_late_registration() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");
    plan_mode(&session, Some(json!(true))).expect("standalone");
    let invariants =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&invariants).expect("register");
    registration.await_ready().await.expect("valid standalone");
}
