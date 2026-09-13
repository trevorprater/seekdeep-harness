//! Live search, ranking, cursor, and disabled-mode parity for the `SQLite` engine.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use seekdeep_core::{
    session::{AppendOptions, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_session_persistence::SESSION_PERSISTENCE;
use seekdeep_session_persistence_jsonl::{JsonlCompression, JsonlConfig, install as install_jsonl};
use seekdeep_session_query::{
    SessionEventSurface, SessionQueryEngine as _, SessionQueryError, SessionQueryErrorCode,
    SessionSearchCursor,
    types::{SessionEventSearchRequest, SessionSearchRequest},
};
use seekdeep_session_query_sqlite::{OpenAt, SqliteSessionQueryConfig, SqliteSessionQueryEngine};
use serde_json::json;

fn config(open_at: OpenAt) -> SqliteSessionQueryConfig {
    SqliteSessionQueryConfig {
        path: ":memory:".to_owned(),
        open_at,
        default_limit: 2,
        max_limit: 3,
        snippet_chars: 20,
        ..SqliteSessionQueryConfig::default()
    }
}

fn search(query: &str, limit: Option<u64>) -> SessionSearchRequest {
    SessionSearchRequest {
        query: query.to_owned(),
        session_filters: None,
        event_filters: None,
        limit,
        cursor: None,
    }
}

fn event_search(id: &str, query: &str) -> SessionEventSearchRequest {
    SessionEventSearchRequest {
        session_id: SessionId::new(id),
        query: query.to_owned(),
        filters: None,
        limit: None,
        cursor: None,
    }
}

fn cursor_with_offset(
    cursor: &SessionSearchCursor,
    offset: serde_json::Value,
) -> SessionSearchCursor {
    let decoded = URL_SAFE_NO_PAD.decode(cursor.as_str()).unwrap();
    let mut payload: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
    payload["offset"] = offset;
    SessionSearchCursor::new(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap()))
}

fn append_user(session: &seekdeep_core::session::Session, text: &str) {
    let turn = u64::try_from(
        session
            .events()
            .iter()
            .filter(|event| event.event_type == "turn/start")
            .count(),
    )
    .unwrap()
        + 1;
    session
        .append(
            "turn/start",
            json!({"turn": turn}),
            AppendOptions::default(),
        )
        .unwrap();
    session
        .append(
            "user/message",
            json!({
                "id": format!("user-message-{turn}"),
                "role": "user",
                "source": {"kind": "user"},
                "content": [{"type": "text", "text": text}]
            }),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap();
    session
        .append(
            "turn/end",
            json!({"turn": turn, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .unwrap();
}

#[tokio::test]
async fn searches_live_sessions_and_events_with_literal_unicode_fts() {
    let context = seekdeep_cordis::Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let first = sessions
        .create(
            &context,
            Some(SessionId::new("first")),
            CreateSessionOptions::default(),
        )
        .unwrap();
    append_user(&first, "alpha AI café");
    let engine = SqliteSessionQueryEngine::new(&context, config(OpenAt::FirstSearch)).unwrap();

    let page = engine
        .search_sessions(search("AI", None), None)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].record.header.id.as_str(), "first");
    assert!(page.items[0].record.live);
    assert!(!page.items[0].record.persisted);
    assert_eq!(
        page.items[0].best_match.record.surface,
        SessionEventSurface::Current
    );
    assert!(page.items[0].best_match.snippet.contains("AI"));

    let events = engine
        .search_events(event_search("first", "café"), None)
        .await
        .unwrap();
    assert_eq!(events.page.items.len(), 1);
    assert_eq!(events.page.items[0].record.event_type, "user/message");
    assert_eq!(events.session.id.as_str(), "first");

    assert!(
        engine
            .search_sessions(search("BRAID", None), None)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    engine.close().await;
}

#[tokio::test]
async fn cursors_bind_requests_and_relevant_generations() {
    let context = seekdeep_cordis::Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    for id in ["a", "b", "c"] {
        let session = sessions
            .create(
                &context,
                Some(SessionId::new(id)),
                CreateSessionOptions::default(),
            )
            .unwrap();
        append_user(&session, "shared needle");
        if id == "a" {
            append_user(&session, "shared needle second");
        }
    }
    let engine = SqliteSessionQueryEngine::new(&context, config(OpenAt::FirstSearch)).unwrap();
    let first = engine
        .search_sessions(search("needle", Some(1)), None)
        .await
        .unwrap();
    assert_eq!(first.items.len(), 1);
    let mut next = search("needle", Some(1));
    next.cursor = first.next_cursor.clone();
    let second = engine.search_sessions(next, None).await.unwrap();
    assert_eq!(second.items.len(), 1);
    assert_ne!(
        first.items[0].record.header.id,
        second.items[0].record.header.id
    );

    let mut wrong = search("different", Some(1));
    wrong.cursor = first.next_cursor;
    let error = engine.search_sessions(wrong, None).await.unwrap_err();
    assert_eq!(
        error.downcast_ref::<SessionQueryError>().unwrap().code,
        SessionQueryErrorCode::SessionQueryInvalidCursor
    );

    let event_first = engine
        .search_events(
            SessionEventSearchRequest {
                limit: Some(1),
                ..event_search("a", "needle")
            },
            None,
        )
        .await
        .unwrap();
    let event_cursor = event_first.page.next_cursor.clone().unwrap();
    let unsafe_cursor = cursor_with_offset(&event_cursor, json!(u64::MAX));
    let error = engine
        .search_events(
            SessionEventSearchRequest {
                limit: Some(1),
                cursor: Some(unsafe_cursor),
                ..event_search("a", "needle")
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<SessionQueryError>().unwrap().code,
        SessionQueryErrorCode::SessionQueryInvalidCursor
    );
    let unrelated = sessions.get(&SessionId::new("b")).unwrap();
    unrelated
        .append(
            "todo/write",
            json!({"todos": [{"status": "pending", "content": "needle"}]}),
            AppendOptions::default(),
        )
        .unwrap();
    let mut continuation = SessionEventSearchRequest {
        limit: Some(1),
        ..event_search("a", "needle")
    };
    continuation.cursor = Some(event_cursor.clone());
    engine.search_events(continuation, None).await.unwrap();
    let target = sessions.get(&SessionId::new("a")).unwrap();
    append_user(&target, "needle target change");
    let error = engine
        .search_events(
            SessionEventSearchRequest {
                limit: Some(1),
                cursor: Some(event_cursor),
                ..event_search("a", "needle")
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<SessionQueryError>().unwrap().code,
        SessionQueryErrorCode::SessionQueryStaleCursor
    );
}

#[tokio::test]
async fn session_cursors_stale_after_transient_persistence_topology_changes() {
    let context = seekdeep_cordis::Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    for id in ["first", "second"] {
        let session = sessions
            .create(
                &context,
                Some(SessionId::new(id)),
                CreateSessionOptions::default(),
            )
            .unwrap();
        append_user(&session, "shared needle");
    }
    let engine = SqliteSessionQueryEngine::new(&context, config(OpenAt::FirstSearch)).unwrap();
    let cursor = engine
        .search_sessions(search("needle", Some(1)), None)
        .await
        .unwrap()
        .next_cursor
        .unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let persistence = install_jsonl(
        &context,
        JsonlConfig {
            root: temporary.path().to_owned(),
            pack_chunks: true,
            compression: JsonlCompression::None,
            write_batch_max_delay_ms: 60_000,
            prepared_session_cache_size: 5,
        },
    )
    .unwrap();
    persistence.await_settled().await.unwrap();
    persistence.dispose().await.unwrap();
    let error = engine
        .search_sessions(
            SessionSearchRequest {
                cursor: Some(cursor),
                ..search("needle", Some(1))
            },
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<SessionQueryError>().unwrap().code,
        SessionQueryErrorCode::SessionQueryStaleCursor
    );
}

#[tokio::test]
async fn never_mode_refuses_search_before_normalization_but_exact_reads_work() {
    let context = seekdeep_cordis::Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("exact")),
            CreateSessionOptions::default(),
        )
        .unwrap();
    append_user(&session, "exact text");
    let engine = SqliteSessionQueryEngine::new(&context, config(OpenAt::Never)).unwrap();
    let error = engine
        .search_sessions(search(" ", None), None)
        .await
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<SessionQueryError>().unwrap().code,
        SessionQueryErrorCode::SessionQuerySearchDisabled
    );
    assert_eq!(engine.list_sessions(None).await.unwrap().len(), 1);
    assert_eq!(
        engine
            .read_session(SessionId::new("exact"))
            .await
            .unwrap()
            .events
            .len(),
        3
    );
    engine.close().await;
}

#[tokio::test]
async fn persisted_rows_reconcile_live_shadow_reveal_and_dynamic_unmount() {
    let temporary = tempfile::tempdir().unwrap();
    let context = seekdeep_cordis::Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let persistence = install_jsonl(
        &context,
        JsonlConfig {
            root: temporary.path().to_owned(),
            pack_chunks: true,
            compression: JsonlCompression::None,
            write_batch_max_delay_ms: 60_000,
            prepared_session_cache_size: 5,
        },
    )
    .unwrap();
    persistence.await_settled().await.unwrap();
    let owner_fiber = seekdeep_cordis::Fiber::active_child("persisted-session");
    let owner = context.with_fiber(owner_fiber.clone());
    let session = sessions
        .create(
            &owner,
            Some(SessionId::new("durable")),
            CreateSessionOptions::default(),
        )
        .unwrap();
    append_user(&session, "durable baseline needle");
    sessions.flush(&session).await.unwrap();
    owner_fiber.dispose().await.unwrap();
    assert!(sessions.get(&SessionId::new("durable")).is_none());

    let engine = SqliteSessionQueryEngine::new(&context, config(OpenAt::FirstSearch)).unwrap();
    let persisted = engine
        .search_sessions(search("baseline", None), None)
        .await
        .unwrap();
    assert_eq!(persisted.items.len(), 1);
    assert!(!persisted.items[0].record.live);
    assert!(persisted.items[0].record.persisted);

    let persistence_backend = context.get(SESSION_PERSISTENCE).unwrap().persistence();
    let mut preparation = persistence_backend
        .prepare(&sessions, &SessionId::new("durable"), None)
        .await
        .unwrap();
    let live_fiber = seekdeep_cordis::Fiber::active_child("live-shadow");
    let live_owner = context.with_fiber(live_fiber.clone());
    let live = preparation.session().clone();
    let detach = sessions.enter(&live).unwrap();
    live_owner.own(detach).unwrap();
    sessions.announce(&live).unwrap();
    preparation.release();
    let overlaid = engine
        .search_sessions(search("baseline", None), None)
        .await
        .unwrap();
    assert_eq!(overlaid.items.len(), 1);
    assert!(overlaid.items[0].record.live);
    assert!(overlaid.items[0].record.persisted);

    live_fiber.dispose().await.unwrap();
    let revealed = engine
        .search_sessions(search("baseline", None), None)
        .await
        .unwrap();
    assert_eq!(revealed.items.len(), 1);
    assert!(!revealed.items[0].record.live);
    assert!(revealed.items[0].record.persisted);
    persistence.dispose().await.unwrap();
    assert!(
        engine
            .search_sessions(search("baseline", None), None)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    engine.close().await;
}
