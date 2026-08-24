//! Literal FTS, ranking, surface, metadata, and header reconstruction parity.

use seekdeep_core::{
    session::{AppendOptions, SessionId, SurfaceOp, SurfaceReplace},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_session_query::{
    SessionAvailability, SessionEventSurface, SessionQueryEngine as _, SessionResultBound,
    SessionResultFilter,
    types::{SessionEventMetadataFilter, SessionEventSearchRequest, SessionSearchRequest},
};
use seekdeep_session_query_sqlite::{OpenAt, SqliteSessionQueryConfig, SqliteSessionQueryEngine};
use serde_json::json;

fn config(snippet_chars: usize) -> SqliteSessionQueryConfig {
    SqliteSessionQueryConfig {
        path: ":memory:".to_owned(),
        open_at: OpenAt::FirstSearch,
        default_limit: 10,
        max_limit: 20,
        snippet_chars,
        ..SqliteSessionQueryConfig::default()
    }
}

fn create_seeded(
    sessions: &SessionStore,
    context: &seekdeep_cordis::Context,
    id: &str,
    text: &str,
    time: i64,
) {
    sessions
        .create(
            context,
            Some(SessionId::new(id)),
            CreateSessionOptions {
                seed: Some(vec![seekdeep_core::session::SessionEvent {
                    event_type: "user/message".to_owned(),
                    seq: 0,
                    time,
                    data: json!({
                        "id": format!("message-{id}"),
                        "role": "user",
                        "source": {"kind": "user"},
                        "content": [{"type": "text", "text": text}]
                    }),
                    source_event_seqs: None,
                    surface_op: Some(SurfaceOp::append()),
                    ignorable: None,
                }]),
                created_at: Some(1),
                ..CreateSessionOptions::default()
            },
        )
        .expect("seeded session");
}

fn session_search(query: &str) -> SessionSearchRequest {
    SessionSearchRequest {
        query: query.to_owned(),
        session_filters: None,
        event_filters: None,
        limit: None,
        cursor: None,
    }
}

#[tokio::test]
async fn literal_phrases_stable_ties_and_unicode_snippets_match_the_oracle() {
    let context = seekdeep_cordis::Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    for (id, text) in [
        ("a", "😀😀 alpha beta BRAID 😀😀"),
        ("b", "alpha beta"),
        ("c", "alpha middle beta"),
        ("d", "alpha beta"),
        ("operator", "needle OR absent"),
        ("only", "needle only"),
        ("quote", "say \"needle\" exactly"),
    ] {
        create_seeded(&sessions, &context, id, text, 10);
    }
    let engine = SqliteSessionQueryEngine::new(&context, config(5)).expect("engine");

    let phrase = engine
        .search_sessions(session_search("alpha beta"), None)
        .await
        .expect("phrase");
    assert_eq!(
        phrase
            .items
            .iter()
            .map(|item| item.record.header.id.as_str())
            .collect::<Vec<_>>(),
        ["b", "d", "a"]
    );
    assert!(
        phrase
            .items
            .iter()
            .all(|item| item.best_match.snippet.chars().count() <= 5)
    );
    assert!(
        engine
            .search_sessions(session_search("AI"), None)
            .await
            .expect("token boundary")
            .items
            .is_empty()
    );
    assert_eq!(
        engine
            .search_sessions(session_search("needle OR absent"), None)
            .await
            .expect("literal operator")
            .items[0]
            .record
            .header
            .id
            .as_str(),
        "operator"
    );
    assert_eq!(
        engine
            .search_sessions(session_search("say \"needle\""), None)
            .await
            .expect("literal quote")
            .items[0]
            .record
            .header
            .id
            .as_str(),
        "quote"
    );
    assert!(
        engine
            .search_sessions(session_search("*"), None)
            .await
            .expect("literal wildcard")
            .items
            .is_empty()
    );
}

fn surface_fixture() -> (
    std::sync::Arc<seekdeep_core::session::Session>,
    std::sync::Arc<SqliteSessionQueryEngine>,
) {
    let context = seekdeep_cordis::Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let parent = SessionId::new("parent");
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("surface")),
            CreateSessionOptions {
                cwd: Some("/work".to_owned()),
                parent_session: Some(parent.clone()),
                created_at: Some(20),
                seed_length: Some(1),
                delegation_depth: Some(2),
                agent_preset: Some("minimal".to_owned()),
                ..CreateSessionOptions::default()
            },
        )
        .expect("session");
    session
        .append(
            "user/message",
            json!({
                "id": "original",
                "role": "user",
                "source": {"kind": "user"},
                "content": [{"type": "text", "text": "needle original"}]
            }),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("original");
    session
        .append(
            "tool/call",
            json!({"name": "needle raw", "arguments": "{}"}),
            AppendOptions::default(),
        )
        .expect("log-only");
    session
        .append(
            "user/message",
            json!({
                "id": "replacement",
                "role": "user",
                "source": {"kind": "plugin", "plugin": "test"},
                "content": [{"type": "text", "text": "needle summary"}]
            }),
            AppendOptions {
                source_event_seqs: Some(vec![0]),
                surface_op: Some(SurfaceOp::Replace(SurfaceReplace {
                    op: "replace".to_owned(),
                    start: 0,
                    end: 0,
                })),
                ..AppendOptions::default()
            },
        )
        .expect("replacement");
    session
        .append(
            "turn/end",
            json!({
                "turn": 1,
                "reason": {"kind": "error", "error": {"message": "needle failure", "code": "UNKNOWN"}}
            }),
            AppendOptions::default(),
        )
        .expect("failure");
    let engine = SqliteSessionQueryEngine::new(&context, config(240)).expect("engine");
    (session, engine)
}

#[tokio::test]
async fn all_surfaces_are_searchable_and_metadata_filters_apply_before_ranking() {
    let (session, engine) = surface_fixture();
    let all = engine
        .search_events(
            SessionEventSearchRequest {
                session_id: session.id().clone(),
                query: "needle".to_owned(),
                filters: None,
                limit: None,
                cursor: None,
            },
            None,
        )
        .await
        .expect("all surfaces");
    let surfaces = all
        .page
        .items
        .iter()
        .map(|item| item.record.surface)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        surfaces,
        [
            SessionEventSurface::Current,
            SessionEventSurface::Shadowed,
            SessionEventSurface::LogOnly,
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(all.session, session.header().clone());

    let event_time = session.events()[2].time;
    let filtered = engine
        .search_events(
            SessionEventSearchRequest {
                session_id: session.id().clone(),
                query: "needle".to_owned(),
                filters: Some(vec![
                    SessionEventMetadataFilter::Seq {
                        from: Some(2.into()),
                        to: Some(2.into()),
                    },
                    SessionEventMetadataFilter::Time {
                        from: Some(SessionResultBound::from(event_time)),
                        to: Some(SessionResultBound::from(event_time)),
                    },
                    SessionEventMetadataFilter::Type {
                        values: vec!["user/message".to_owned()],
                    },
                    SessionEventMetadataFilter::Surface {
                        values: vec![SessionEventSurface::Current],
                    },
                ]),
                limit: None,
                cursor: None,
            },
            None,
        )
        .await
        .expect("event filters");
    assert_eq!(filtered.page.items.len(), 1);
    assert_eq!(filtered.page.items[0].record.seq, 2);
}

#[tokio::test]
async fn session_filters_reconstruct_complete_headers_and_select_shadowed_matches() {
    let (session, engine) = surface_fixture();
    let grouped = engine
        .search_sessions(
            SessionSearchRequest {
                query: "needle".to_owned(),
                session_filters: Some(vec![
                    SessionResultFilter::Id {
                        values: vec![session.id().clone()],
                    },
                    SessionResultFilter::Cwd {
                        values: vec![Some("/work".to_owned())],
                    },
                    SessionResultFilter::CreatedAt {
                        from: Some(20.into()),
                        to: Some(20.into()),
                    },
                    SessionResultFilter::Parent {
                        values: vec![Some(SessionId::new("parent"))],
                    },
                    SessionResultFilter::Availability {
                        values: vec![SessionAvailability::Live],
                    },
                ]),
                event_filters: Some(vec![SessionEventMetadataFilter::Surface {
                    values: vec![SessionEventSurface::Shadowed],
                }]),
                limit: None,
                cursor: None,
            },
            None,
        )
        .await
        .expect("grouped filters");
    assert_eq!(grouped.items.len(), 1);
    assert_eq!(grouped.items[0].best_match.record.seq, 0);
    assert_eq!(
        grouped.items[0].record.header.agent_preset.as_deref(),
        Some("minimal")
    );
}

#[tokio::test]
async fn assistant_reasoning_is_not_indexed_but_visible_answer_text_is() {
    let context = seekdeep_cordis::Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("reasoning")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    session
        .append(
            "assistant/message",
            json!({
                "turn": 1,
                "step": 1,
                "message": {
                    "id": "assistant-message",
                    "role": "assistant",
                    "source": {"kind": "model", "provider": "mock", "model": "mock"},
                    "content": [
                        {"type": "reasoning", "text": "private-chain-marker"},
                        {"type": "text", "text": "visible-answer-marker"}
                    ]
                }
            }),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("assistant message");
    let engine = SqliteSessionQueryEngine::new(&context, config(240)).expect("engine");
    assert!(
        engine
            .search_sessions(session_search("private-chain-marker"), None)
            .await
            .expect("private search")
            .items
            .is_empty()
    );
    assert_eq!(
        engine
            .search_sessions(session_search("visible-answer-marker"), None)
            .await
            .expect("visible search")
            .items
            .len(),
        1
    );
}
