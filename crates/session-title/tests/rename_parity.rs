//! Behavioral mirror of packages/session/session-title/tests/rename.spec.ts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionEvent, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_session_title::{
    SessionTitleAutomaticMode, SessionTitleConfig, SessionTitleProvider, SessionTitleProviderId,
    SessionTitleProviderRequest, SessionTitleProviderResult, SessionTitleService,
    SessionTitleSource, fold_session_title,
};
use serde_json::Value;
use serde_json::json;
use tokio::sync::Notify;

const CONFIG: SessionTitleConfig = SessionTitleConfig {
    fallback_max_words: 5,
    fallback_max_bytes: 40,
    max_title_bytes: 40,
};

struct MockProvider {
    id: String,
    automatic: SessionTitleAutomaticMode,
    title: String,
    calls: Arc<AtomicUsize>,
    aborted: Arc<AtomicBool>,
    gate: Option<Arc<Notify>>,
}

#[async_trait]
impl SessionTitleProvider for MockProvider {
    fn id(&self) -> SessionTitleProviderId {
        SessionTitleProviderId::new(self.id.as_str())
    }

    fn automatic(&self) -> SessionTitleAutomaticMode {
        self.automatic
    }

    async fn generate(
        &self,
        request: SessionTitleProviderRequest,
    ) -> anyhow::Result<SessionTitleProviderResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        self.aborted
            .store(request.signal.is_aborted(), Ordering::SeqCst);
        Ok(SessionTitleProviderResult {
            title: self.title.clone(),
            message_seqs: request.messages.iter().map(|message| message.seq).collect(),
            model: None,
        })
    }
}

fn user_message(text: &str) -> Value {
    json!({
        "id": "u-message",
        "role": "user",
        "content": [{"type": "text", "text": text}],
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

fn append_human_prompt(session: &Arc<Session>, text: &str) -> SessionEvent {
    append_surface(session, "user/message", user_message(text))
}

async fn wait_for_title(service: &SessionTitleService, session: &Arc<Session>) {
    for _ in 0..1_000 {
        if service.get(session).is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

async fn wait_for_calls(calls: &Arc<AtomicUsize>, target: usize) {
    for _ in 0..1_000 {
        if calls.load(Ordering::SeqCst) >= target {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

async fn settle() {
    for _ in 0..3 {
        tokio::task::yield_now().await;
    }
}

fn harness() -> (Context, Arc<SessionStore>, Arc<SessionTitleService>) {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("service");
    (context, sessions, service)
}

fn fresh_session(context: &Context, sessions: &Arc<SessionStore>) -> Arc<Session> {
    sessions
        .create(context, None, CreateSessionOptions::default())
        .expect("session")
}

#[tokio::test]
async fn appends_a_normalized_user_source_title() {
    let (context, sessions, service) = harness();
    let session = fresh_session(&context, &sessions);
    append(&session, "turn/start", json!({"turn": 1}));
    append_human_prompt(&session, "Original prompt text");
    wait_for_title(&service, &session).await;

    let accepted = service
        .rename(&session, "  Hand	picked   name  ")
        .expect("rename");
    assert_eq!(accepted.event.title, "Hand picked name");
    assert_eq!(accepted.event.message_seqs, Vec::<u64>::new());
    assert_eq!(accepted.event.source, SessionTitleSource::User);

    let events = session.events();
    let event = events
        .iter()
        .rev()
        .find(|event| event.event_type == "session/title")
        .expect("title event");
    assert_eq!(
        event.data,
        json!({"title": "Hand picked name", "messageSeqs": [], "source": {"kind": "user"}})
    );
    let folded = fold_session_title(&events).expect("folded");
    assert_eq!(folded.event.source, SessionTitleSource::User);
}

#[tokio::test]
async fn rejects_titles_that_normalize_to_empty_and_dead_sessions() {
    let (context, sessions, service) = harness();
    let session = fresh_session(&context, &sessions);

    let empty = service
        .rename(&session, "  [31m  ")
        .expect_err("empty title");
    assert!(format!("{empty:#}").contains("visible characters"));

    let detached = Session::create(&SessionId::new("detached"), None, None).expect("detached");
    let dead = service.rename(&detached, "name").expect_err("dead session");
    assert!(format!("{dead:#}").contains("not live in this store"));
}

#[tokio::test]
async fn pins_the_title_and_refresh_unpins() {
    let (context, sessions, service) = harness();
    let calls = Arc::new(AtomicUsize::new(0));
    let aborted = Arc::new(AtomicBool::new(false));
    let provider = MockProvider {
        id: "pin-provider".to_owned(),
        automatic: SessionTitleAutomaticMode::AllPrompts,
        title: "Provider title".to_owned(),
        calls: Arc::clone(&calls),
        aborted: Arc::clone(&aborted),
        gate: None,
    };
    service.register(Arc::new(provider)).expect("register");

    let session = fresh_session(&context, &sessions);
    append(&session, "turn/start", json!({"turn": 1}));
    append_human_prompt(&session, "First prompt");
    wait_for_title(&service, &session).await;
    service.rename(&session, "Pinned by hand").expect("rename");

    append_human_prompt(&session, "Second prompt after the pin");
    wait_for_title(&service, &session).await;
    append(
        &session,
        "request/header",
        json!({"header": {"config": {"provider": "main-route", "model": "chat-model"}}, "reason": "change"}),
    );
    settle().await;

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        service.get(&session).expect("snapshot").event.title,
        "Pinned by hand"
    );

    let refreshed = service
        .refresh(&session, None)
        .await
        .expect("refresh")
        .expect("refreshed snapshot");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(refreshed.event.title, "Provider title");
    assert_eq!(
        service.get(&session).expect("snapshot").event.source,
        SessionTitleSource::Provider {
            provider: SessionTitleProviderId::new("pin-provider"),
            model: None,
        }
    );
}

#[tokio::test]
async fn fallback_only_refresh_unpins() {
    let (context, sessions, service) = harness();
    let session = fresh_session(&context, &sessions);
    append(&session, "turn/start", json!({"turn": 1}));
    append_human_prompt(&session, "Derivable prompt words");
    wait_for_title(&service, &session).await;
    service
        .rename(&session, "Pinned without provider")
        .expect("rename");
    assert_eq!(
        service.get(&session).expect("snapshot").event.source,
        SessionTitleSource::User
    );

    let refreshed = service
        .refresh(&session, None)
        .await
        .expect("refresh")
        .expect("refreshed snapshot");
    assert_eq!(refreshed.event.title, "Derivable prompt words");
    assert_eq!(refreshed.event.source, SessionTitleSource::Fallback);
    assert_eq!(
        service.get(&session).expect("snapshot").event.source,
        SessionTitleSource::Fallback
    );
}

#[tokio::test]
async fn supersedes_in_flight_generation() {
    let (context, sessions, service) = harness();
    let calls = Arc::new(AtomicUsize::new(0));
    let aborted = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(Notify::new());
    let provider = MockProvider {
        id: "deferred-provider".to_owned(),
        automatic: SessionTitleAutomaticMode::AllPrompts,
        title: "Late provider title".to_owned(),
        calls: Arc::clone(&calls),
        aborted: Arc::clone(&aborted),
        gate: Some(Arc::clone(&gate)),
    };
    service.register(Arc::new(provider)).expect("register");

    let session = fresh_session(&context, &sessions);
    append(&session, "turn/start", json!({"turn": 1}));
    append_human_prompt(&session, "Prompt that triggers generation");
    append(
        &session,
        "request/header",
        json!({"header": {"config": {"provider": "main-route", "model": "chat-model"}}, "reason": "change"}),
    );
    wait_for_calls(&calls, 1).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    service.rename(&session, "User wins").expect("rename");
    gate.notify_one();
    for _ in 0..1_000 {
        if aborted.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(aborted.load(Ordering::SeqCst));

    let events = session.events();
    let latest = events
        .iter()
        .rev()
        .find(|event| event.event_type == "session/title")
        .expect("title event");
    assert_eq!(latest.data["title"], "User wins");
    assert_eq!(latest.data["source"]["kind"], "user");
}

#[tokio::test]
async fn fallback_only_refresh_keeps_user_title_when_fallback_is_empty() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(
        &context,
        SessionTitleConfig {
            fallback_max_words: 5,
            fallback_max_bytes: 3,
            max_title_bytes: 40,
        },
    )
    .expect("service");
    let session = fresh_session(&context, &sessions);
    append(&session, "turn/start", json!({"turn": 1}));
    append_human_prompt(&session, "😀😀");
    wait_for_title(&service, &session).await;
    service
        .rename(&session, "Sticky emoji pin")
        .expect("rename");

    let refreshed = service
        .refresh(&session, None)
        .await
        .expect("refresh")
        .expect("refreshed snapshot");
    assert_eq!(refreshed.event.title, "Sticky emoji pin");
    assert_eq!(
        service.get(&session).expect("snapshot").event.source,
        SessionTitleSource::User
    );
}
