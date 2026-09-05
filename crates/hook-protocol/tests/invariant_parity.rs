//! Replay and live-dispatch parity for the hook invoked/result pairing invariant.

use std::sync::Arc;

use seekdeep_cordis::{Context, EventArgs};
use seekdeep_core::{
    session::{AppendOptions, Session, SessionEvent, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_hook_protocol::invariant::register_invariant;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use serde_json::{Value, json};

async fn setup() -> (Context, Arc<SessionStore>) {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let registry =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&registry).expect("registration");
    registration.await_ready().await.expect("invariant ready");
    (context, sessions)
}

fn invoked(overrides: &[(&str, Value)]) -> Value {
    let mut data = serde_json::Map::new();
    data.insert("turn".to_owned(), json!(1));
    data.insert("point".to_owned(), json!("PreToolUse"));
    data.insert("dialect".to_owned(), json!("claude-code"));
    data.insert("handlerId".to_owned(), json!("hook-1"));
    for (key, value) in overrides {
        data.insert((*key).to_owned(), value.clone());
    }
    Value::Object(data)
}

fn result(overrides: &[(&str, Value)]) -> Value {
    let mut data = serde_json::Map::new();
    data.insert("turn".to_owned(), json!(1));
    data.insert("point".to_owned(), json!("PreToolUse"));
    data.insert("handlerId".to_owned(), json!("hook-1"));
    data.insert("decision".to_owned(), json!("pass"));
    data.insert("durationMs".to_owned(), json!(3));
    for (key, value) in overrides {
        data.insert((*key).to_owned(), value.clone());
    }
    Value::Object(data)
}

fn start_turn(session: &Session, turn: u64) {
    session
        .append(
            "turn/start",
            json!({"turn": turn}),
            AppendOptions::default(),
        )
        .expect("turn start");
}

fn raw_event(event_type: &str, seq: u64, time: i64, data: Value) -> SessionEvent {
    SessionEvent {
        event_type: event_type.to_owned(),
        seq,
        time,
        data,
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    }
}

fn emit(context: &Context, session: Arc<Session>, event: SessionEvent) -> anyhow::Result<()> {
    context.events().emit(
        context,
        "session/event",
        &EventArgs::from_values(vec![session, Arc::new(event)]),
    )
}

fn message(error: &anyhow::Error) -> String {
    format!("{error:#}")
}

#[tokio::test]
async fn pairs_serial_and_repeated_handler_invocations() {
    let (context, sessions) = setup().await;
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");
    start_turn(&session, 1);
    session
        .append("hook/invoked", invoked(&[]), AppendOptions::default())
        .expect("first invoked");
    session
        .append("hook/invoked", invoked(&[]), AppendOptions::default())
        .expect("second invoked");
    session
        .append(
            "step/start",
            json!({"turn": 1, "step": 1}),
            AppendOptions::default(),
        )
        .expect("step start");
    session
        .append("hook/result", result(&[]), AppendOptions::default())
        .expect("first result");
    session
        .append("hook/result", result(&[]), AppendOptions::default())
        .expect("second result");
}

#[tokio::test]
async fn rebuilds_pending_hook_invocations_from_an_existing_session() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");
    start_turn(&session, 1);
    session
        .append("hook/invoked", invoked(&[]), AppendOptions::default())
        .expect("invoked before companion");
    let registry =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("registry");
    let registration = register_invariant(&registry).expect("registration");
    registration
        .await_ready()
        .await
        .expect("rebuilds pending invocations");
    session
        .append("hook/result", result(&[]), AppendOptions::default())
        .expect("result");
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("turn end");
}

#[tokio::test]
async fn adopts_a_bare_session_first_observed_through_publication() {
    let (context, _) = setup().await;
    let session =
        Session::create(&SessionId::new("bare-hook-session"), None, None).expect("bare session");
    emit(
        &context,
        session.clone(),
        raw_event("turn/start", 0, 0, json!({"turn": 1})),
    )
    .expect("turn start");
    emit(
        &context,
        session.clone(),
        raw_event("hook/invoked", 1, 1, invoked(&[])),
    )
    .expect("invoked");
    emit(
        &context,
        session.clone(),
        raw_event("hook/result", 2, 2, result(&[])),
    )
    .expect("result");
}

#[tokio::test]
async fn rejects_hook_events_outside_or_for_a_different_open_turn() {
    let (context, sessions) = setup().await;
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");
    let outside = session
        .append("hook/invoked", invoked(&[]), AppendOptions::default())
        .expect_err("outside any turn");
    assert!(message(&anyhow::Error::new(outside)).contains("outside any open turn"));

    start_turn(&session, 1);
    let wrong_turn = session
        .append(
            "hook/invoked",
            invoked(&[("turn", json!(2))]),
            AppendOptions::default(),
        )
        .expect_err("wrong turn");
    assert!(message(&anyhow::Error::new(wrong_turn)).contains("but open turn is 1"));
}

#[tokio::test]
async fn rejects_an_unenclosed_hook_event_when_replaying_an_existing_session() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");
    start_turn(&session, 1);
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("turn end");
    session
        .append("hook/invoked", invoked(&[]), AppendOptions::default())
        .expect("unenclosed invoked before companion");
    let registry =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("registry");
    let registration = register_invariant(&registry).expect("registration");
    let error = registration
        .await_ready()
        .await
        .expect_err("replay rejects");
    assert!(message(&error).contains("outside any open turn"));
}

#[tokio::test]
async fn rejects_malformed_hook_invocations() {
    let (context, sessions) = setup().await;
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");
    start_turn(&session, 1);
    for (overrides, expected) in [
        (
            &[("point", json!(""))][..],
            "point and handlerId must be non-empty",
        ),
        (
            &[("handlerId", json!(""))][..],
            "point and handlerId must be non-empty",
        ),
        (&[("dialect", json!("other"))][..], "unknown dialect"),
    ] {
        let error = session
            .append("hook/invoked", invoked(overrides), AppendOptions::default())
            .expect_err("malformed invocation");
        assert!(
            message(&anyhow::Error::new(error)).contains(expected),
            "expected {expected}"
        );
    }
}

#[tokio::test]
async fn rejects_unmatched_and_malformed_results() {
    let (context, sessions) = setup().await;
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");
    start_turn(&session, 1);
    let unmatched = session
        .append("hook/result", result(&[]), AppendOptions::default())
        .expect_err("unmatched result");
    assert!(message(&anyhow::Error::new(unmatched)).contains("no matching hook/invoked"));

    session
        .append("hook/invoked", invoked(&[]), AppendOptions::default())
        .expect("invoked");
    let negative = session
        .append(
            "hook/result",
            result(&[("durationMs", json!(-1))]),
            AppendOptions::default(),
        )
        .expect_err("negative duration");
    assert!(
        message(&anyhow::Error::new(negative))
            .contains("durationMs must be a non-negative finite number")
    );
    let mismatched_point = session
        .append(
            "hook/result",
            result(&[("point", json!("Stop"))]),
            AppendOptions::default(),
        )
        .expect_err("mismatched point");
    assert!(message(&anyhow::Error::new(mismatched_point)).contains("no matching hook/invoked"));
}
