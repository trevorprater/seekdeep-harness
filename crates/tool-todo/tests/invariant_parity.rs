//! Behavioral mirror of `packages/todo/tool-todo/tests/invariant.spec.ts`.

use std::sync::Arc;

use seekdeep_cordis::{Context, EventArgs};
use seekdeep_core::{
    session::{AppendOptions, Session, SessionError, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_tool_todo::register_invariant;
use serde_json::{Value, json};

async fn setup() -> (Context, Arc<Session>) {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");
    let invariants =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&invariants).expect("register");
    registration.await_ready().await.expect("ready");
    (context, session)
}

#[allow(clippy::needless_pass_by_value)]
fn write(session: &Session, todos: Value) -> Result<(), SessionError> {
    session
        .append(
            "todo/write",
            json!({"todos": todos}),
            AppendOptions::default(),
        )
        .map(|_| ())
}

#[tokio::test]
async fn accepts_historical_and_live_parallel_snapshots() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");
    let parallel = json!([
        {"content": "Inspect state", "status": "completed"},
        {"content": "Apply fix", "status": "in_progress"},
        {"content": "Watch background build", "status": "in_progress"},
        {"content": "Run checks", "status": "pending"},
    ]);
    // Historical append (before the invariant installs) may carry parallel in_progress.
    write(&session, parallel.clone()).expect("historical parallel");
    let invariants =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&invariants).expect("register");
    registration
        .await_ready()
        .await
        .expect("seeds historical parallel");
    // Live validation accepts the same parallel snapshot too.
    write(&session, parallel).expect("live parallel");
}

#[tokio::test]
async fn rejects_an_incoherent_durable_todo_snapshot() {
    let cases: Vec<(&str, Value, &str)> = vec![
        ("not an array", json!("not-an-array"), "must be an array"),
        ("null entry", json!([null]), "entries must be objects"),
        ("number entry", json!([42]), "entries must be objects"),
        (
            "number content",
            json!([{"content": 42, "status": "pending"}]),
            "content must be non-empty",
        ),
        (
            "empty content",
            json!([{"content": "", "status": "pending"}]),
            "content must be non-empty",
        ),
        (
            "padded content",
            json!([{"content": " padded ", "status": "pending"}]),
            "already trimmed",
        ),
        (
            "duplicate content",
            json!([
                {"content": "same", "status": "pending"},
                {"content": "same", "status": "completed"},
            ]),
            "repeats content",
        ),
        (
            "number status",
            json!([{"content": "task", "status": 42}]),
            "unknown status",
        ),
        (
            "paused status",
            json!([{"content": "task", "status": "paused"}]),
            "unknown status",
        ),
    ];

    for (name, todos, pattern) in cases {
        let (_, session) = setup().await;
        let error = write(&session, todos).expect_err("invalid");
        assert!(
            error.to_string().contains(pattern),
            "{name}: expected {pattern:?} in {error}"
        );
    }
}

#[tokio::test]
async fn ignores_unrelated_dispatches_and_session_events() {
    let (context, session) = setup().await;
    context
        .events()
        .emit(&context, "tools/change", &EventArgs::new())
        .expect("unrelated dispatch");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("unrelated session event");
}

#[tokio::test]
async fn rejects_an_invalid_existing_snapshot_on_late_registration() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("todo-invariant-late")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    write(
        &session,
        json!([
            {"content": "duplicate", "status": "pending"},
            {"content": "duplicate", "status": "completed"},
        ]),
    )
    .expect("append before invariant");
    let invariants =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&invariants).expect("register");
    let error = registration
        .await_ready()
        .await
        .expect_err("late registration");
    assert!(error.to_string().contains("repeats content \"duplicate\""));
}
