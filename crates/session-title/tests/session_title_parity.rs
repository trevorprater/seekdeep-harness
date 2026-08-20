//! Behavioral mirror of packages/session/session-title/tests/session-title.spec.ts.

use std::sync::Arc;
use std::time::Duration;

use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionEvent, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_session_title::{
    SessionTitleConfig, SessionTitleModelProvenance, SessionTitleProviderId, SessionTitleService,
    SessionTitleSource, fallback_session_title, fold_session_title, normalize_session_title,
    truncate_title_utf8,
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

fn plugin_message(text: &str) -> Value {
    json!({
        "id": "u-plugin",
        "role": "user",
        "content": [{"type": "text", "text": text}],
        "source": {"kind": "plugin", "plugin": "seed"},
    })
}

fn reasoning_message() -> Value {
    json!({
        "id": "u-reasoning",
        "role": "user",
        "content": [{"type": "reasoning", "text": "not visible text"}],
        "source": {"kind": "user"},
    })
}

fn append(session: &Arc<Session>, event_type: &str, data: Value) -> SessionEvent {
    session
        .append(event_type, data, AppendOptions::default())
        .expect("append")
}

fn append_surface(session: &Arc<Session>, event_type: &str, data: Value) -> SessionEvent {
    session
        .append(
            event_type,
            data,
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("append surface")
}

async fn wait_for_title(service: &SessionTitleService, session: &Arc<Session>) {
    for _ in 0..1_000 {
        if service.get(session).is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

#[test]
fn normalization_removes_terminal_controls_collapses_whitespace_and_applies_caps() {
    assert_eq!(
        normalize_session_title(
            "]0;stolen  Hello	 brave
new world  ",
            80
        ),
        "Hello brave new world"
    );
    assert_eq!(
        fallback_session_title("one two three four", 3, 80),
        "one two three"
    );
    assert_eq!(fallback_session_title("你好世界", 5, 7), "你好");
    assert_eq!(fallback_session_title("😀😀", 5, 5).len(), 4);
}

#[test]
#[should_panic(expected = "maxBytes must be a positive integer")]
fn rejects_non_positive_byte_limit() {
    let _ = truncate_title_utf8("title", 0);
}

#[test]
#[should_panic(expected = "maxWords must be a positive integer")]
fn rejects_non_positive_word_limit() {
    // The source also rejects a fractional limit (1.5); a Rust usize is always
    // integral, so that case collapses into the non-positive check.
    let _ = fallback_session_title("title", 0, 10);
}

#[tokio::test]
async fn logs_and_folds_an_immediate_fallback_after_the_first_eligible_message() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("service");
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");

    append(&session, "turn/start", json!({"turn": 1}));
    let message = append_surface(
        &session,
        "user/message",
        user_message(
            "  Build
log-backed session titles please  ",
        ),
    );
    wait_for_title(&service, &session).await;

    let events = session.events();
    let title_event = events
        .iter()
        .rev()
        .find(|event| event.event_type == "session/title")
        .expect("title event");
    assert_eq!(title_event.seq, 2);
    assert_eq!(
        title_event.data["title"],
        "Build log-backed session titles please"
    );
    assert_eq!(title_event.data["messageSeqs"], json!([message.seq]));
    assert_eq!(title_event.data["source"]["kind"], "fallback");

    let snapshot = service.get(&session).expect("snapshot");
    assert_eq!(
        snapshot.event.title,
        "Build log-backed session titles please"
    );
    assert_eq!(snapshot.event.message_seqs, vec![message.seq]);
    assert_eq!(snapshot.event.source, SessionTitleSource::Fallback);
    assert_eq!(snapshot.event_seq, 2);
    assert_eq!(
        snapshot.updated_at,
        u64::try_from(title_event.time).expect("non-negative time")
    );
    assert_eq!(session.derive_messages().len(), 1);
    assert_eq!(session.surface_nodes(), vec![message.seq]);
}

#[tokio::test]
async fn derives_a_fallback_title_from_the_direct_prompt() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("service");
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");

    append(&session, "turn/start", json!({"turn": 1}));
    append_surface(
        &session,
        "user/message",
        user_message("Explain this referenced session"),
    );
    wait_for_title(&service, &session).await;

    assert_eq!(
        service.get(&session).expect("snapshot").event.title,
        "Explain this referenced session"
    );
}

#[tokio::test]
async fn waits_through_synthetic_empty_and_non_text_messages_then_keeps_the_first_fallback() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("service");
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");

    append(&session, "turn/start", json!({"turn": 1}));
    append_surface(&session, "user/message", plugin_message("plugin text"));
    append_surface(&session, "user/message", reasoning_message());
    append_surface(
        &session,
        "user/message",
        user_message(
            " 
	 ",
        ),
    );
    assert!(service.get(&session).is_none());

    let eligible = append_surface(&session, "user/message", user_message("first real prompt"));
    wait_for_title(&service, &session).await;
    let first = service.get(&session).expect("first fallback");

    append_surface(&session, "user/message", user_message("later prompt"));
    wait_for_title(&service, &session).await;

    assert_eq!(first.event.message_seqs, vec![eligible.seq]);
    assert_eq!(service.get(&session), Some(first.clone()));
    let title_count = session
        .events()
        .iter()
        .filter(|event| event.event_type == "session/title")
        .count();
    assert_eq!(title_count, 1);
}

#[test]
fn folds_the_latest_title_event_during_replay() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let session = sessions
        .create(&context, None, CreateSessionOptions::default())
        .expect("session");
    session
        .append(
            "session/title",
            json!({"title": "Earlier", "messageSeqs": [1], "source": {"kind": "fallback"}}),
            AppendOptions::default(),
        )
        .expect("first title");
    session
        .append(
            "session/title",
            json!({
                "title": "Later",
                "messageSeqs": [1, 4],
                "source": {
                    "kind": "provider",
                    "provider": "test-provider",
                    "model": {"provider": "mock", "model": "title-model"},
                },
            }),
            AppendOptions::default(),
        )
        .expect("second title");

    let events = session.events();
    let folded = fold_session_title(&events).expect("folded");
    assert_eq!(folded.event.title, "Later");
    assert_eq!(folded.event.message_seqs, vec![1, 4]);
    assert_eq!(
        folded.event.source,
        SessionTitleSource::Provider {
            provider: SessionTitleProviderId::new("test-provider"),
            model: Some(SessionTitleModelProvenance {
                provider: "mock".to_owned(),
                model: "title-model".to_owned(),
            }),
        }
    );
    assert_eq!(folded.event_seq, 1);
    assert_eq!(
        folded.updated_at,
        u64::try_from(events[1].time).expect("non-negative time")
    );
}
