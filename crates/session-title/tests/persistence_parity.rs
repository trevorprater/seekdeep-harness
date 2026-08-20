//! Behavioral mirror of packages/session/session-title/tests/persistence.spec.ts.

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_session_persistence::SESSION_PERSISTENCE;
use seekdeep_session_persistence_jsonl::{JsonlCompression, JsonlConfig, install as install_jsonl};
use seekdeep_session_persistence_sqlite::{SqliteConfig, install as install_sqlite};
use seekdeep_session_title::{
    SessionTitleConfig, SessionTitleService, SessionTitleSource, fold_session_title,
};
use serde_json::Value;
use serde_json::json;

const CONFIG: SessionTitleConfig = SessionTitleConfig {
    fallback_max_words: 5,
    fallback_max_bytes: 40,
    max_title_bytes: 80,
};

fn user_message(text: &str) -> Value {
    json!({
        "id": "u-message",
        "role": "user",
        "content": [{"type": "text", "text": text}],
        "source": {"kind": "user"},
    })
}

async fn append_persisted_title(
    sessions: &Arc<SessionStore>,
    context: &Context,
    service: &SessionTitleService,
    id: &SessionId,
) {
    let session = sessions
        .create(context, Some(id.clone()), CreateSessionOptions::default())
        .expect("session");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("turn start");
    session
        .append(
            "user/message",
            user_message("Persist this session title"),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("user message");
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("turn end");
    service
        .refresh(&session, None)
        .await
        .expect("refresh")
        .expect("fallback title");
}

async fn expect_persisted_title(context: &Context, id: &SessionId) {
    let persistence = context.get(SESSION_PERSISTENCE).expect("service");
    let inspection = persistence.persistence().load(id).await.expect("load");
    let folded = fold_session_title(&inspection.events).expect("folded");
    assert_eq!(folded.event.title, "Persist this session title");
    assert_eq!(folded.event.message_seqs, vec![1]);
    assert_eq!(folded.event.source, SessionTitleSource::Fallback);
    assert_eq!(folded.event_seq, 3);
    let types = inspection
        .events
        .iter()
        .map(|event| event.event_type.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        types,
        ["turn/start", "user/message", "turn/end", "session/title"]
    );
}

#[tokio::test]
async fn round_trips_through_a_remounted_jsonl_backend() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = temporary.path().to_owned();
    let id = SessionId::new("title-jsonl");

    let writer = Context::new();
    let sessions = SessionStore::install(&writer).expect("sessions");
    let mut writer_config = JsonlConfig::new(root.clone());
    writer_config.compression = JsonlCompression::None;
    let mounted = install_jsonl(&writer, writer_config).expect("jsonl");
    let service = SessionTitleService::install(&writer, CONFIG).expect("title");
    mounted.await_settled().await.expect("jsonl settled");
    append_persisted_title(&sessions, &writer, &service, &id).await;
    mounted.dispose().await.expect("dispose and drain writer");

    let reader = Context::new();
    SessionStore::install(&reader).expect("sessions");
    let mut reader_config = JsonlConfig::new(root);
    reader_config.compression = JsonlCompression::None;
    let mounted = install_jsonl(&reader, reader_config).expect("jsonl");
    mounted.await_settled().await.expect("jsonl settled");
    expect_persisted_title(&reader, &id).await;
    mounted.dispose().await.expect("dispose reader");
}

#[tokio::test]
async fn round_trips_through_a_remounted_sqlite_backend() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let path = temporary.path().join("sessions.db");
    let id = SessionId::new("title-sqlite");

    let writer = Context::new();
    let sessions = SessionStore::install(&writer).expect("sessions");
    let mounted = install_sqlite(&writer, SqliteConfig::new(path.clone())).expect("sqlite");
    let service = SessionTitleService::install(&writer, CONFIG).expect("title");
    mounted.await_settled().await.expect("sqlite settled");
    append_persisted_title(&sessions, &writer, &service, &id).await;
    mounted.dispose().await.expect("dispose and drain writer");

    let reader = Context::new();
    SessionStore::install(&reader).expect("sessions");
    let mounted = install_sqlite(&reader, SqliteConfig::new(path)).expect("sqlite");
    mounted.await_settled().await.expect("sqlite settled");
    expect_persisted_title(&reader, &id).await;
    mounted.dispose().await.expect("dispose reader");
}
