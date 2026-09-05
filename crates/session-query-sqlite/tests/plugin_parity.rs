//! Real Cordis load, service, persistence, and disposal integration.

use seekdeep_core::{
    session::{AppendOptions, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_session_persistence::SESSION_PERSISTENCE;
use seekdeep_session_persistence_sqlite::{SqliteConfig, install as install_persistence};
use seekdeep_session_query::{SESSION_QUERY, types::SessionSearchRequest};
use seekdeep_session_query_sqlite::{OpenAt, SqliteSessionQueryConfig, install as install_query};
use serde_json::json;

fn search(query: &str) -> SessionSearchRequest {
    SessionSearchRequest {
        query: query.to_owned(),
        session_filters: None,
        event_filters: None,
        limit: None,
        cursor: None,
    }
}

#[tokio::test]
async fn plugin_mounts_real_persistence_searches_and_withdraws_cleanly() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let context = seekdeep_cordis::Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let persistence = install_persistence(
        &context,
        SqliteConfig::new(temporary.path().join("canonical.db")),
    )
    .expect("persistence plugin");
    persistence
        .await_settled()
        .await
        .expect("persistence active");

    let owner_fiber = seekdeep_cordis::Fiber::active_child("query fixture owner");
    let owner = context.with_fiber(owner_fiber.clone());
    let session = sessions
        .create(
            &owner,
            Some(SessionId::new("plugin-path")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("turn start");
    session
        .append(
            "user/message",
            json!({
                "id": "plugin-user-message",
                "role": "user",
                "source": {"kind": "user"},
                "content": [{"type": "text", "text": "real plugin Loader needle"}]
            }),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("message");
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("turn end");
    sessions.flush(&session).await.expect("flush");
    owner_fiber.dispose().await.expect("retire session");

    let query = install_query(
        &context,
        SqliteSessionQueryConfig {
            path: temporary
                .path()
                .join("derived.db")
                .to_string_lossy()
                .into_owned(),
            ..SqliteSessionQueryConfig::default()
        },
    )
    .expect("query plugin");
    query.await_settled().await.expect("query active");
    let service = context.get(SESSION_QUERY).expect("query service");
    let page = service
        .search_sessions(search("Loader needle"), None)
        .await
        .expect("search");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].record.header.id.as_str(), "plugin-path");
    assert!(!page.items[0].record.live);
    assert!(page.items[0].record.persisted);
    assert_eq!(service.list_sessions(None).await.expect("list").len(), 1);

    query.dispose().await.expect("dispose query");
    assert!(context.get(SESSION_QUERY).is_none());
    let persistence_service = context
        .get(SESSION_PERSISTENCE)
        .expect("persistence remains");
    assert_eq!(
        persistence_service
            .persistence()
            .load(&SessionId::new("plugin-path"))
            .await
            .expect("load after query disposal")
            .meta
            .id
            .as_str(),
        "plugin-path"
    );
    persistence.dispose().await.expect("dispose persistence");
}

#[tokio::test]
async fn first_search_and_never_plugins_do_not_touch_the_database_until_allowed() {
    for open_at in [OpenAt::FirstSearch, OpenAt::Never] {
        let temporary = tempfile::tempdir().expect("tempdir");
        let path = temporary.path().join("unopened.db");
        let context = seekdeep_cordis::Context::new();
        SessionStore::install(&context).expect("sessions");
        let mounted = install_query(
            &context,
            SqliteSessionQueryConfig {
                path: path.to_string_lossy().into_owned(),
                open_at,
                ..SqliteSessionQueryConfig::default()
            },
        )
        .expect("query plugin");
        mounted.await_settled().await.expect("active");
        assert!(!path.exists());
        mounted.dispose().await.expect("dispose");
        assert!(!path.exists());
    }
}

#[test]
fn direct_engine_defaults_and_validation_match_the_source_contract() {
    let context = seekdeep_cordis::Context::new();
    SessionStore::install(&context).expect("sessions");
    let engine = seekdeep_session_query_sqlite::SqliteSessionQueryEngine::new(
        &context,
        SqliteSessionQueryConfig {
            path: ":memory:".to_owned(),
            ..SqliteSessionQueryConfig::default()
        },
    )
    .expect("default engine");
    assert_eq!(engine.config.open_at, OpenAt::Startup);
    assert_eq!(engine.config.default_limit, 20);
    assert_eq!(engine.config.max_limit, 100);
    assert_eq!(engine.config.persisted_inspect_concurrency, 4);

    let invalid = [
        SqliteSessionQueryConfig {
            path: String::new(),
            ..SqliteSessionQueryConfig::default()
        },
        SqliteSessionQueryConfig {
            path: ":memory:".to_owned(),
            default_limit: 0,
            ..SqliteSessionQueryConfig::default()
        },
        SqliteSessionQueryConfig {
            path: ":memory:".to_owned(),
            max_limit: 0,
            ..SqliteSessionQueryConfig::default()
        },
        SqliteSessionQueryConfig {
            path: ":memory:".to_owned(),
            snippet_chars: 0,
            ..SqliteSessionQueryConfig::default()
        },
        SqliteSessionQueryConfig {
            path: ":memory:".to_owned(),
            default_limit: 3,
            max_limit: 2,
            ..SqliteSessionQueryConfig::default()
        },
        SqliteSessionQueryConfig {
            path: ":memory:".to_owned(),
            persisted_inspect_concurrency: 9_007_199_254_740_992,
            ..SqliteSessionQueryConfig::default()
        },
        SqliteSessionQueryConfig {
            path: ":memory:".to_owned(),
            journal_mode: "memory".to_owned(),
            ..SqliteSessionQueryConfig::default()
        },
    ];
    for config in invalid {
        let error = seekdeep_session_query_sqlite::SqliteSessionQueryEngine::new(&context, config)
            .expect_err("invalid config");
        assert_eq!(
            error
                .downcast_ref::<seekdeep_session_query::SessionQueryError>()
                .expect("typed config error")
                .code,
            seekdeep_session_query::SessionQueryErrorCode::SessionQueryInvalidConfig
        );
    }
    assert!(
        serde_json::from_value::<SqliteSessionQueryConfig>(json!({
            "path": ":memory:",
            "openAt": "later"
        }))
        .is_err()
    );
}
