//! Lossless JSONL and `SQLite` retry-event persistence.

use std::sync::Arc;

use seekdeep_core::{
    session::{AppendOptions, Session, SessionId},
    session_store::SessionStore,
};
use seekdeep_session_persistence::SessionPersistence;
use seekdeep_session_persistence_jsonl::{JsonlConfig, JsonlSessionPersistence};
use seekdeep_session_persistence_sqlite::{SqliteConfig, SqliteSessionPersistence};
use serde_json::json;

fn retry_session(id: &str) -> Arc<Session> {
    let session = Session::create(&SessionId::new(id), None, None).unwrap();
    for (event_type, data) in [
        ("turn/start", json!({"turn":1})),
        ("step/start", json!({"turn":1,"step":1})),
        (
            "request/header",
            json!({"header":{"config":{"provider":"mock","model":"mock"}},"reason":"initial"}),
        ),
        (
            "llm/retry",
            json!({
                "retryId":format!("{id}-chain"),
                "turn":1,
                "step":1,
                "provider":"mock",
                "mode":"always",
                "policyKey":"[\"always\",500,10000,0.1]",
                "retry":1,
                "delayMs":750,
                "failure":{"message":"provider busy","code":"RATE_LIMIT","status":429}
            }),
        ),
        ("step/end", json!({"turn":1,"step":1})),
        (
            "turn/end",
            json!({
                "turn":1,
                "reason":{"kind":"error","error":{"message":"provider busy","code":"RATE_LIMIT","status":429}}
            }),
        ),
    ] {
        session
            .append(event_type, data, AppendOptions::default())
            .unwrap();
    }
    session
}

async fn assert_round_trip(backend: &dyn SessionPersistence, session: &Arc<Session>) {
    let expected = session
        .events()
        .into_iter()
        .find(|event| event.event_type == "llm/retry")
        .unwrap();
    assert!(session.derive_messages().is_empty());
    backend.create(session.header()).await.unwrap();
    backend
        .append(session.id(), &session.events())
        .await
        .unwrap();
    let loaded = backend.load(session.id()).await.unwrap();
    assert_eq!(
        loaded
            .events
            .into_iter()
            .find(|event| event.event_type == "llm/retry")
            .unwrap(),
        expected
    );
}

#[tokio::test]
async fn jsonl_round_trips_retry_without_adding_a_model_message() {
    let temporary = tempfile::tempdir().unwrap();
    let context = seekdeep_cordis::Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let backend =
        JsonlSessionPersistence::new(sessions, JsonlConfig::new(temporary.path())).unwrap();
    assert_round_trip(backend.as_ref(), &retry_session("retry-jsonl")).await;
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn sqlite_round_trips_retry_without_adding_a_model_message() {
    let context = seekdeep_cordis::Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let backend = SqliteSessionPersistence::new(sessions, SqliteConfig::new(":memory:")).unwrap();
    assert_round_trip(backend.as_ref(), &retry_session("retry-sqlite")).await;
    context.fiber().dispose().await.unwrap();
}
