//! Behavioral mirror of packages/session/session-title/tests/service-contracts.spec.ts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionEvent, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::AbortSignal;
use seekdeep_session_title::{
    SessionTitleAutomaticMode, SessionTitleConfig, SessionTitleModelProvenance,
    SessionTitleProvider, SessionTitleProviderId, SessionTitleProviderRequest,
    SessionTitleProviderResult, SessionTitleService, SessionTitleSource,
};
use serde_json::Value;
use serde_json::json;
use tokio::sync::Notify;

const CONFIG: SessionTitleConfig = SessionTitleConfig {
    fallback_max_words: 5,
    fallback_max_bytes: 40,
    max_title_bytes: 80,
};

type Respond = dyn Fn(
        SessionTitleProviderRequest,
        usize,
    ) -> BoxFuture<'static, anyhow::Result<SessionTitleProviderResult>>
    + Send
    + Sync;

struct Provider {
    id: String,
    automatic: SessionTitleAutomaticMode,
    calls: Arc<AtomicUsize>,
    respond: Box<Respond>,
}

#[async_trait]
impl SessionTitleProvider for Provider {
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
        let index = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        (self.respond)(request, index).await
    }
}

fn respond_all(title: &'static str) -> Box<Respond> {
    Box::new(move |request, _index| {
        Box::pin(async move {
            Ok(SessionTitleProviderResult {
                title: title.to_owned(),
                message_seqs: request.messages.iter().map(|message| message.seq).collect(),
                model: None,
            })
        })
    })
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

fn append_prompt(session: &Arc<Session>, text: &str) -> SessionEvent {
    append_surface(session, "user/message", user_message(text))
}

fn start_session(sessions: &Arc<SessionStore>, context: &Context, id: &str) -> Arc<Session> {
    let session = sessions
        .create(
            context,
            Some(SessionId::new(id)),
            CreateSessionOptions::default(),
        )
        .expect("session");
    append(&session, "turn/start", json!({"turn": 1}));
    session
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

#[test]
fn requires_explicit_positive_limits_and_a_fallback_cap_no_larger_than_accepted() {
    let context = Context::new();
    SessionStore::install(&context).expect("sessions");
    // Rust's typed Config makes the source's undefined/null/fractional
    // configuration cases unrepresentable; only the numeric bounds port.
    let zero_words = SessionTitleService::install(
        &context,
        SessionTitleConfig {
            fallback_max_words: 0,
            ..CONFIG
        },
    )
    .err()
    .expect("zero words");
    assert!(format!("{zero_words:#}").contains("fallbackMaxWords must be a positive integer"));

    let oversized_fallback = SessionTitleService::install(
        &context,
        SessionTitleConfig {
            fallback_max_bytes: 81,
            ..CONFIG
        },
    )
    .err()
    .expect("oversized fallback");
    assert!(
        format!("{oversized_fallback:#}")
            .contains("fallbackMaxBytes must not exceed maxTitleBytes")
    );
}

#[tokio::test]
async fn returns_no_title_for_empty_input_and_rejects_detached_or_pre_aborted_refreshes() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("title");

    let empty = sessions
        .create(
            &context,
            Some(SessionId::new("empty-fallback")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    assert!(
        service
            .refresh(&empty, None)
            .await
            .expect("refresh")
            .is_none()
    );

    let with_provider = Context::new();
    let provider_sessions = SessionStore::install(&with_provider).expect("sessions");
    let provider_service = SessionTitleService::install(&with_provider, CONFIG).expect("title");
    let calls = Arc::new(AtomicUsize::new(0));
    let empty_provider = Provider {
        id: "empty-provider".to_owned(),
        automatic: SessionTitleAutomaticMode::FirstPrompt,
        calls: Arc::clone(&calls),
        respond: respond_all("unused"),
    };
    provider_service
        .register(Arc::new(empty_provider))
        .expect("register");
    let provider_empty = provider_sessions
        .create(
            &with_provider,
            Some(SessionId::new("empty-provider")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    assert!(
        provider_service
            .refresh(&provider_empty, None)
            .await
            .expect("refresh")
            .is_none()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let detached = Session::create(&SessionId::new("detached"), None, None).expect("detached");
    let detached_error = provider_service
        .refresh(&detached, None)
        .await
        .expect_err("detached refresh");
    assert!(format!("{detached_error:#}").contains("not live in this store"));

    let signal = AbortSignal::default();
    signal.abort_with_reason(json!("already cancelled"));
    let pre_aborted = provider_service
        .refresh(&provider_empty, Some(signal))
        .await
        .expect_err("pre-aborted refresh");
    assert!(format!("{pre_aborted:#}").contains("already cancelled"));
}

#[tokio::test]
async fn passes_an_absent_route_and_caller_cancellation_into_explicit_generation() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("title");

    let observed_route = Arc::new(Mutex::new(None));
    let observed_aborted = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Provider {
        id: "explicit-no-route".to_owned(),
        automatic: SessionTitleAutomaticMode::FirstPrompt,
        calls: Arc::clone(&calls),
        respond: {
            let observed_route = Arc::clone(&observed_route);
            let observed_aborted = Arc::clone(&observed_aborted);
            Box::new(move |request, _index| {
                let observed_route = Arc::clone(&observed_route);
                let observed_aborted = Arc::clone(&observed_aborted);
                Box::pin(async move {
                    *observed_route.lock() = request.route.clone();
                    observed_aborted.store(request.signal.is_aborted(), Ordering::SeqCst);
                    Ok(SessionTitleProviderResult {
                        title: "Explicit title".to_owned(),
                        message_seqs: request.messages.iter().map(|message| message.seq).collect(),
                        model: None,
                    })
                })
            })
        },
    };
    service.register(Arc::new(provider)).expect("register");

    let session = start_session(&sessions, &context, "explicit-no-route");
    append_prompt(&session, "Refresh before any request header");
    wait_for_title(&service, &session).await;
    let signal = AbortSignal::default();

    let refreshed = service
        .refresh(&session, Some(signal))
        .await
        .expect("refresh")
        .expect("snapshot");
    assert_eq!(refreshed.event.title, "Explicit title");
    assert!(observed_route.lock().is_none());
    assert!(!observed_aborted.load(Ordering::SeqCst));
}

#[tokio::test]
async fn propagates_explicit_cancellation_to_active_work() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("title");

    let gate = Arc::new(Notify::new());
    let aborted = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Provider {
        id: "caller-cancel".to_owned(),
        automatic: SessionTitleAutomaticMode::FirstPrompt,
        calls: Arc::clone(&calls),
        respond: {
            let gate = Arc::clone(&gate);
            let aborted = Arc::clone(&aborted);
            Box::new(move |request, _index| {
                let gate = Arc::clone(&gate);
                let aborted = Arc::clone(&aborted);
                Box::pin(async move {
                    gate.notified().await;
                    aborted.store(request.signal.is_aborted(), Ordering::SeqCst);
                    Ok(SessionTitleProviderResult {
                        title: "ignored".to_owned(),
                        message_seqs: request.messages.iter().map(|message| message.seq).collect(),
                        model: None,
                    })
                })
            })
        },
    };
    service.register(Arc::new(provider)).expect("register");

    let session = start_session(&sessions, &context, "caller-cancel");
    append_prompt(&session, "Cancel this refresh");
    wait_for_title(&service, &session).await;
    let signal = AbortSignal::default();
    let refresh = {
        let service = Arc::clone(&service);
        let session = Arc::clone(&session);
        let signal = signal.clone();
        tokio::spawn(async move { service.refresh(&session, Some(signal)).await })
    };
    wait_for_calls(&calls, 1).await;
    signal.abort_with_reason(json!("caller cancelled"));
    gate.notify_one();
    let error = refresh.await.expect("join").expect_err("refresh rejects");
    assert!(format!("{error:#}").contains("caller cancelled"));
    assert!(aborted.load(Ordering::SeqCst));
}

#[tokio::test]
async fn shares_one_fallback_across_concurrent_refreshes() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("title");

    let seed =
        Session::create(&SessionId::new("fallback-concurrency-seed"), None, None).expect("seed");
    append(&seed, "turn/start", json!({"turn": 1}));
    let source = append_prompt(&seed, "Create exactly one fallback title");
    append(
        &seed,
        "turn/end",
        json!({"turn": 1, "reason": {"kind": "completed"}}),
    );
    let seed_events = seed.events();
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("fallback-concurrency")),
            CreateSessionOptions {
                seed: Some(seed_events.clone()),
                seed_length: Some(u64::try_from(seed_events.len()).expect("seed length")),
                ..CreateSessionOptions::default()
            },
        )
        .expect("session");

    let (first, second) = tokio::join!(
        service.refresh(&session, None),
        service.refresh(&session, None)
    );
    let first = first.expect("first refresh").expect("first snapshot");
    let second = second.expect("second refresh").expect("second snapshot");
    assert_eq!(first, second);
    assert_eq!(
        session
            .events()
            .iter()
            .filter(|event| event.event_type == "session/title")
            .count(),
        1
    );
    assert_eq!(
        session
            .events()
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        [
            "turn/start",
            "user/message",
            "turn/end",
            "session/end-seed",
            "session/title"
        ]
    );
    assert_eq!(
        service.get(&session).expect("snapshot").event.message_seqs,
        vec![source.seq]
    );
}

#[tokio::test]
async fn reuses_a_title_accepted_before_the_queued_fallback_commits() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("title");

    let session = start_session(&sessions, &context, "fallback-already-accepted");
    let source = append_prompt(&session, "Reuse the title that wins the fallback race");

    let refresh = service.refresh(&session, None);
    session
        .append(
            "session/title",
            json!({"title": "Already accepted", "messageSeqs": [source.seq], "source": {"kind": "fallback"}}),
            AppendOptions::default(),
        )
        .expect("manual title");

    let refreshed = refresh.await.expect("refresh").expect("snapshot");
    assert_eq!(refreshed.event.title, "Already accepted");
    assert_eq!(
        session
            .events()
            .iter()
            .filter(|event| event.event_type == "session/title")
            .count(),
        1
    );
}

#[tokio::test]
async fn lets_the_newest_overlapping_explicit_refresh_win() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("title");

    let session = start_session(&sessions, &context, "refresh-order");
    let source = append_prompt(&session, "Keep the newest explicit refresh");
    wait_for_title(&service, &session).await;
    append(
        &session,
        "turn/end",
        json!({"turn": 1, "reason": {"kind": "completed"}}),
    );

    // Each generate parks on its own gate and records its request signal.
    let gates: Arc<Mutex<Vec<Arc<Notify>>>> = Arc::new(Mutex::new(Vec::new()));
    let signals: Arc<Mutex<Vec<AbortSignal>>> = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Provider {
        id: "refresh-order".to_owned(),
        automatic: SessionTitleAutomaticMode::FirstPrompt,
        calls: Arc::clone(&calls),
        respond: {
            let gates = Arc::clone(&gates);
            let signals = Arc::clone(&signals);
            Box::new(move |request, index| {
                let gates = Arc::clone(&gates);
                let signals = Arc::clone(&signals);
                Box::pin(async move {
                    let gate = Arc::new(Notify::new());
                    gates.lock().push(Arc::clone(&gate));
                    signals.lock().push(request.signal.clone());
                    gate.notified().await;
                    let title = if index == 1 {
                        "Obsolete title"
                    } else {
                        "Newest explicit title"
                    };
                    Ok(SessionTitleProviderResult {
                        title: title.to_owned(),
                        message_seqs: request.messages.iter().map(|message| message.seq).collect(),
                        model: None,
                    })
                })
            })
        },
    };
    service.register(Arc::new(provider)).expect("register");

    let older = {
        let service = Arc::clone(&service);
        let session = Arc::clone(&session);
        tokio::spawn(async move { service.refresh(&session, None).await })
    };
    wait_for_calls(&calls, 1).await;
    let newer = {
        let service = Arc::clone(&service);
        let session = Arc::clone(&session);
        tokio::spawn(async move { service.refresh(&session, None).await })
    };
    wait_for_calls(&calls, 2).await;

    {
        let observed = signals.lock();
        assert!(observed[0].is_aborted());
        assert!(!observed[1].is_aborted());
    }

    gates.lock()[0].notify_one();
    let older_error = older.await.expect("join older").expect_err("older rejects");
    assert!(format!("{older_error:#}").contains("superseded"));

    gates.lock()[1].notify_one();
    let newer_result = newer.await.expect("join newer").expect("newer ok");
    let snapshot = newer_result.expect("newer snapshot");
    assert_eq!(snapshot.event.title, "Newest explicit title");
    let _ = source;
}

#[tokio::test]
async fn aborts_pending_and_active_work_on_service_unload() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let fiber = context
        .plugin(seekdeep_session_title::plugin(), json!(CONFIG))
        .expect("plugin");
    fiber.await_settled().await.expect("settled");
    let service = context
        .get(seekdeep_session_title::SESSION_TITLE)
        .expect("service");

    let gate = Arc::new(Notify::new());
    let signals: Arc<Mutex<Vec<AbortSignal>>> = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Provider {
        id: "service-unload".to_owned(),
        automatic: SessionTitleAutomaticMode::AllPrompts,
        calls: Arc::clone(&calls),
        respond: {
            let gate = Arc::clone(&gate);
            let signals = Arc::clone(&signals);
            Box::new(move |request, _index| {
                let gate = Arc::clone(&gate);
                let signals = Arc::clone(&signals);
                Box::pin(async move {
                    signals.lock().push(request.signal.clone());
                    gate.notified().await;
                    Ok(SessionTitleProviderResult {
                        title: "Ignored service abort".to_owned(),
                        message_seqs: request.messages.iter().map(|message| message.seq).collect(),
                        model: None,
                    })
                })
            })
        },
    };
    service.register(Arc::new(provider)).expect("register");

    let active = start_session(&sessions, &context, "service-unload-active");
    append_prompt(&active, "Active provider work");
    wait_for_title(&service, &active).await;
    let refresh = {
        let service = Arc::clone(&service);
        let active = Arc::clone(&active);
        tokio::spawn(async move { service.refresh(&active, None).await })
    };
    wait_for_calls(&calls, 1).await;

    let pending = start_session(&sessions, &context, "service-unload-pending");
    append_prompt(&pending, "Pending provider work");

    let disposed = Arc::new(AtomicBool::new(false));
    let dispose_flag = Arc::clone(&disposed);
    let disposal = tokio::spawn(async move {
        fiber.dispose().await.expect("dispose");
        dispose_flag.store(true, Ordering::SeqCst);
    });
    for _ in 0..1_000 {
        if signals.lock()[0].is_aborted() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(signals.lock()[0].is_aborted());
    assert!(!disposed.load(Ordering::SeqCst));

    gate.notify_one();
    disposal.await.expect("join disposal");
    assert!(disposed.load(Ordering::SeqCst));
    let outcome = refresh
        .await
        .expect("join refresh")
        .expect_err("refresh rejects");
    assert!(format!("{outcome:#}").contains("session-title service disposed"));
}

#[tokio::test]
async fn leaves_a_title_absent_when_the_byte_cap_cannot_hold_the_first_code_point() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(
        &context,
        SessionTitleConfig {
            fallback_max_words: 5,
            fallback_max_bytes: 1,
            max_title_bytes: 2,
        },
    )
    .expect("title");

    let session = start_session(&sessions, &context, "no-code-point");
    append_prompt(&session, "😀");
    settle().await;
    assert!(service.get(&session).is_none());
    assert!(
        service
            .refresh(&session, None)
            .await
            .expect("refresh")
            .is_none()
    );
}

#[tokio::test]
async fn rejects_malformed_provider_registrations_before_publishing_them() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("title");
    let _ = sessions;

    // Rust's typed SessionTitleProvider trait makes the source's null/string/
    // non-object/non-generate registrations unrepresentable; only the empty-id
    // case ports.
    let empty_id_calls = Arc::new(AtomicUsize::new(0));
    let empty_id = Provider {
        id: String::new(),
        automatic: SessionTitleAutomaticMode::FirstPrompt,
        calls: Arc::clone(&empty_id_calls),
        respond: respond_all("title"),
    };
    let error = service.register(Arc::new(empty_id)).expect_err("empty id");
    assert!(format!("{error:#}").contains("id must be a non-empty string"));
}

#[tokio::test]
async fn drops_automatic_work_when_its_provider_is_disposed_before_the_queued_start() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("title");

    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Provider {
        id: "queued-dispose".to_owned(),
        automatic: SessionTitleAutomaticMode::AllPrompts,
        calls: Arc::clone(&calls),
        respond: respond_all("too late"),
    };
    let dispose = service.register(Arc::new(provider)).expect("register");

    let session = start_session(&sessions, &context, "queued-dispose");
    append_prompt(&session, "Queue provider work");
    wait_for_title(&service, &session).await;
    append(
        &session,
        "request/header",
        json!({"header": {"config": {"provider": "main", "model": "main"}}, "reason": "initial"}),
    );
    let pending = start_session(&sessions, &context, "pending-provider-dispose");
    append_prompt(&pending, "Drop pending provider work");
    dispose.dispose().await.expect("dispose provider");
    settle().await;

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        service.get(&session).expect("snapshot").event.source,
        SessionTitleSource::Fallback
    );
    assert_eq!(
        service.get(&pending).expect("snapshot").event.source,
        SessionTitleSource::Fallback
    );
}

#[tokio::test]
async fn rejects_malformed_provider_results_without_replacing_the_fallback() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("title");

    // Rust's typed SessionTitleProviderResult makes the source's non-object,
    // non-string-title, non-integer-seq, and malformed-model cases
    // unrepresentable; only the representable value-level rejections port.
    let next: Arc<Mutex<SessionTitleProviderResult>> =
        Arc::new(Mutex::new(SessionTitleProviderResult {
            title: "valid".to_owned(),
            message_seqs: vec![0],
            model: None,
        }));
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Provider {
        id: "invalid-results".to_owned(),
        automatic: SessionTitleAutomaticMode::FirstPrompt,
        calls: Arc::clone(&calls),
        respond: {
            let next = Arc::clone(&next);
            Box::new(move |_request, _index| {
                let next = Arc::clone(&next);
                Box::pin(async move { Ok(next.lock().clone()) })
            })
        },
    };
    service.register(Arc::new(provider)).expect("register");

    let session = start_session(&sessions, &context, "invalid-results");
    let first = append_prompt(&session, "First source");
    wait_for_title(&service, &session).await;
    let second = append_prompt(&session, "Second source");
    wait_for_title(&service, &session).await;

    let cases = [
        (
            SessionTitleProviderResult {
                title: "[31m".to_owned(),
                message_seqs: vec![first.seq],
                model: None,
            },
            "empty title",
        ),
        (
            SessionTitleProviderResult {
                title: "valid".to_owned(),
                message_seqs: vec![],
                model: None,
            },
            "at least one source message",
        ),
        (
            SessionTitleProviderResult {
                title: "valid".to_owned(),
                message_seqs: vec![999],
                model: None,
            },
            "unique, ordered seqs",
        ),
        (
            SessionTitleProviderResult {
                title: "valid".to_owned(),
                message_seqs: vec![first.seq, first.seq],
                model: None,
            },
            "unique, ordered seqs",
        ),
        (
            SessionTitleProviderResult {
                title: "valid".to_owned(),
                message_seqs: vec![second.seq, first.seq],
                model: None,
            },
            "unique, ordered seqs",
        ),
    ];
    for (result, expected) in cases {
        *next.lock() = result;
        let error = service
            .refresh(&session, None)
            .await
            .expect_err("malformed result rejects");
        assert!(format!("{error:#}").contains(expected));
        assert_eq!(
            service.get(&session).expect("snapshot").event.source,
            SessionTitleSource::Fallback
        );
    }
    let _ = SessionTitleModelProvenance {
        provider: "p".to_owned(),
        model: "m".to_owned(),
    };
}
