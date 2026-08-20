//! Behavioral mirror of packages/session/session-title/tests/projection.spec.ts.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionEvent},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_session_projection::SessionProjectionRegistry;
use seekdeep_session_title::{SessionTitleConfig, SessionTitleService, plugin};
use serde_json::{Value, json};

const CONFIG: SessionTitleConfig = SessionTitleConfig {
    fallback_max_words: 8,
    fallback_max_bytes: 64,
    max_title_bytes: 256,
};

fn harness(
    with_title_service: bool,
) -> (
    Context,
    Arc<SessionStore>,
    Arc<SessionProjectionRegistry>,
    Arc<Session>,
) {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let projections = SessionProjectionRegistry::install(&context).expect("projections");
    if with_title_service {
        SessionTitleService::install(&context, CONFIG).expect("title service");
    }
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");
    (context, sessions, projections, session)
}

fn append_title(session: &Arc<Session>, title: &str) -> SessionEvent {
    session
        .append(
            "session/title",
            json!({"title": title, "messageSeqs": [1], "source": {"kind": "fallback"}}),
            AppendOptions::default(),
        )
        .expect("append title")
}

fn title_value(value: &Value) -> &str {
    value.as_str().expect("title string")
}

#[test]
fn serves_null_before_the_first_title_event() {
    let (_, _, projections, session) = harness(true);
    let snapshot = projections.snapshot(&session).expect("snapshot");
    assert!(snapshot.values["title"].is_null());
}

#[test]
fn serves_the_latest_title_last_wins_and_notifies_the_change_feed() {
    let (context, _, projections, session) = harness(true);
    let changes = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&changes);
    projections
        .on_changed(
            &context,
            Arc::new(move |_session, key, value, seq| {
                observed.lock().push((key.to_owned(), value.clone(), seq));
                Ok(())
            }),
        )
        .expect("listener");

    let first_seq = append_title(&session, "First title");
    let second_seq = append_title(&session, "Second title");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("unrelated");

    assert_eq!(
        *changes.lock(),
        vec![
            ("title".to_owned(), json!("First title"), first_seq.seq),
            ("title".to_owned(), json!("Second title"), second_seq.seq),
        ]
    );
    let snapshot = projections.snapshot(&session).expect("snapshot");
    assert_eq!(title_value(&snapshot.values["title"]), "Second title");
    assert_eq!(
        snapshot.as_of_seq,
        i64::try_from(session.seq() - 1).expect("seq")
    );
}

#[test]
fn folds_titles_already_in_the_log_when_the_service_mounts_late() {
    let (context, _, projections, session) = harness(false);
    append_title(&session, "Pre-mount title");
    SessionTitleService::install(&context, CONFIG).expect("title service");
    let snapshot = projections.snapshot(&session).expect("snapshot");
    assert_eq!(title_value(&snapshot.values["title"]), "Pre-mount title");
}

#[tokio::test]
async fn has_no_title_key_without_the_service_and_drops_it_on_unload() {
    let (context, _, projections, session) = harness(false);
    assert!(
        !projections
            .snapshot(&session)
            .expect("snapshot")
            .values
            .contains_key("title")
    );

    let fiber = context.plugin(plugin(), json!(CONFIG)).expect("plugin");
    fiber.await_settled().await.expect("settled");
    append_title(&session, "Ephemeral");
    assert_eq!(
        title_value(&projections.snapshot(&session).expect("snapshot").values["title"]),
        "Ephemeral"
    );

    fiber.dispose().await.expect("dispose");
    assert!(
        !projections
            .snapshot(&session)
            .expect("snapshot")
            .values
            .contains_key("title")
    );
}
