//! Behavioral mirror of packages/schedule/schedule/tests/invariant.spec.ts.

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionEvent, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_schedule::invariant::register_invariant;
use serde_json::{Value, json};

async fn harness() -> (Context, Arc<SessionStore>) {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let invariants =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&invariants).expect("register");
    registration.await_ready().await.expect("ready");
    (context, sessions)
}

fn create(id: &str) -> Value {
    json!({
        "version": 1,
        "operation": "create",
        "schedule": {
            "id": id,
            "kind": "after",
            "prompt": "check logs",
            "afterSeconds": 1,
            "scheduledAt": "2026-08-05T12:00:01.000Z",
        },
    })
}

fn create_every(id: &str) -> Value {
    json!({
        "version": 1,
        "operation": "create",
        "schedule": {
            "id": id,
            "kind": "every",
            "prompt": "check metrics",
            "everySeconds": 300,
            "scheduledAt": "2026-08-05T12:05:00.000Z",
        },
    })
}

fn malformed_event(seq: u64) -> SessionEvent {
    SessionEvent {
        event_type: "schedule/change".to_owned(),
        seq,
        time: 1,
        data: json!({"version": 9, "operation": "delete", "id": "schedule-1"}),
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    }
}

fn session(
    context: &Context,
    sessions: &SessionStore,
    id: &str,
    options: CreateSessionOptions,
) -> Arc<Session> {
    sessions
        .create(context, Some(SessionId::new(id)), options)
        .expect("session")
}

#[tokio::test]
async fn accepts_valid_candidates_and_rejects_invalid_transitions_before_append() {
    let (context, sessions) = harness().await;
    let session = session(
        &context,
        &sessions,
        "schedule-invariant",
        CreateSessionOptions::default(),
    );
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("turn/start");
    session
        .append(
            "schedule/change",
            create("schedule-1"),
            AppendOptions::default(),
        )
        .expect("create");

    let error = session
        .append(
            "schedule/change",
            json!({"version": 1, "operation": "delete", "id": "missing"}),
            AppendOptions::default(),
        )
        .expect_err("delete missing");
    assert!(error.to_string().contains("seekdeep-schedule"));

    session
        .append(
            "schedule/change",
            json!({"version": 1, "operation": "dispatch", "id": "schedule-1"}),
            AppendOptions::default(),
        )
        .expect("dispatch");
}

#[tokio::test]
async fn requires_a_decision_time_for_every_dispatch_and_advances_the_live_stream() {
    let (context, sessions) = harness().await;
    let session = session(
        &context,
        &sessions,
        "schedule-every-invariant",
        CreateSessionOptions::default(),
    );
    session
        .append(
            "schedule/change",
            create_every("schedule-every"),
            AppendOptions::default(),
        )
        .expect("create every");

    let error = session
        .append(
            "schedule/change",
            json!({"version": 1, "operation": "dispatch", "id": "schedule-every"}),
            AppendOptions::default(),
        )
        .expect_err("dispatch without decision time");
    assert!(error.to_string().contains("seekdeep-schedule"));

    session
        .append(
            "schedule/change",
            json!({
                "version": 1,
                "operation": "dispatch",
                "id": "schedule-every",
                "acceptedAt": "2026-08-05T12:17:34.000Z",
            }),
            AppendOptions::default(),
        )
        .expect("dispatch with decision time");
}

#[tokio::test]
async fn rejects_a_malformed_existing_owned_stream_during_companion_setup() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let invariants =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    session(
        &context,
        &sessions,
        "schedule-invalid-seed",
        CreateSessionOptions {
            seed: Some(vec![malformed_event(0)]),
            ..CreateSessionOptions::default()
        },
    );
    let registration = register_invariant(&invariants).expect("register");
    let error = registration
        .await_ready()
        .await
        .expect_err("malformed seed");
    assert!(error.to_string().contains("seekdeep-schedule"));
}

#[tokio::test]
async fn rejects_a_malformed_seeded_session_created_after_companion_setup() {
    let (context, sessions) = harness().await;
    let error = sessions
        .create(
            &context,
            Some(SessionId::new("schedule-invalid-future-seed")),
            CreateSessionOptions {
                seed: Some(vec![malformed_event(0)]),
                ..CreateSessionOptions::default()
            },
        )
        .expect_err("malformed future seed");
    assert!(error.to_string().contains("seekdeep-schedule"));
}

#[tokio::test]
async fn ignores_inherited_schedule_events_before_a_fork_seed_boundary() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let invariants =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let child = session(
        &context,
        &sessions,
        "schedule-fork",
        CreateSessionOptions {
            seed: Some(vec![malformed_event(0)]),
            parent_session: Some(SessionId::new("parent")),
            seed_length: Some(1),
            ..CreateSessionOptions::default()
        },
    );
    let registration = register_invariant(&invariants).expect("register");
    registration.await_ready().await.expect("inherited ignored");

    child
        .append("schedule/change", create("child"), AppendOptions::default())
        .expect("child create");
}
