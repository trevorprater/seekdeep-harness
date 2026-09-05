//! Behavioral mirror of packages/session/session-title/tests/provider.spec.ts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionEvent, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{GenerateOptions, LlmRuntime, ModelId, ProviderId};
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
    fallback_max_bytes: 24,
    max_title_bytes: 24,
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
    requests: Arc<Mutex<Vec<SessionTitleProviderRequest>>>,
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
        self.requests.lock().push(request.clone());
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

fn append_human_prompt(session: &Arc<Session>, text: &str) -> SessionEvent {
    append_surface(session, "user/message", user_message(text))
}

fn append_route(session: &Arc<Session>, reason: &str) -> SessionEvent {
    append(
        session,
        "request/header",
        json!({"header": {"config": {"provider": "main-route", "model": "chat-model"}}, "reason": reason}),
    )
}

async fn wait_for_title(service: &SessionTitleService, session: &Arc<Session>) {
    for _ in 0..1_000 {
        if service.get(session).is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

async fn wait_for_title_matching(
    service: &SessionTitleService,
    session: &Arc<Session>,
    expected: &str,
) {
    for _ in 0..1_000 {
        if service
            .get(session)
            .is_some_and(|snapshot| snapshot.event.title == expected)
        {
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

#[tokio::test]
async fn inherits_title_events_across_forks_and_skips_first_prompt_retitling() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("title");

    let parent = sessions
        .create(
            &context,
            Some(SessionId::new("title-parent")),
            CreateSessionOptions::default(),
        )
        .expect("parent");
    append(&parent, "turn/start", json!({"turn": 1}));
    let inherited = append_human_prompt(&parent, "Inherited title prompt");
    wait_for_title(&service, &parent).await;
    append(
        &parent,
        "turn/end",
        json!({"turn": 1, "reason": {"kind": "completed"}}),
    );

    let child = sessions
        .fork(&context, &parent, None, Some(SessionId::new("title-child")))
        .expect("fork");
    assert_eq!(service.get(&child), service.get(&parent));

    let first_calls = Arc::new(AtomicUsize::new(0));
    let first_requests = Arc::new(Mutex::new(Vec::new()));
    let first_provider = Provider {
        id: "fork-first".to_owned(),
        automatic: SessionTitleAutomaticMode::FirstPrompt,
        calls: Arc::clone(&first_calls),
        requests: Arc::clone(&first_requests),
        respond: respond_all("Should not run"),
    };
    let dispose_first = service
        .register(Arc::new(first_provider))
        .expect("register first");

    append(&child, "turn/start", json!({"turn": 2}));
    let child_message = append_human_prompt(&child, "Child follow-up prompt");
    wait_for_title(&service, &child).await;
    append_route(&child, "initial");
    settle().await;
    append(
        &child,
        "turn/end",
        json!({"turn": 2, "reason": {"kind": "completed"}}),
    );
    assert_eq!(first_calls.load(Ordering::SeqCst), 0);
    dispose_first.dispose().await.expect("dispose first");

    let all_calls = Arc::new(AtomicUsize::new(0));
    let all_requests = Arc::new(Mutex::new(Vec::new()));
    let all_provider = Provider {
        id: "fork-all".to_owned(),
        automatic: SessionTitleAutomaticMode::AllPrompts,
        calls: Arc::clone(&all_calls),
        requests: Arc::clone(&all_requests),
        respond: respond_all("Fork all prompts"),
    };
    service
        .register(Arc::new(all_provider))
        .expect("register all");

    append(&child, "turn/start", json!({"turn": 3}));
    let latest = append_human_prompt(&child, "Retitle the fork now");
    wait_for_title_matching(&service, &child, "Fork all prompts").await;
    append_route(&child, "change");
    settle().await;
    append(
        &child,
        "turn/end",
        json!({"turn": 3, "reason": {"kind": "completed"}}),
    );

    assert_eq!(all_calls.load(Ordering::SeqCst), 1);
    let snapshot = service.get(&child).expect("child snapshot");
    assert_eq!(snapshot.event.title, "Fork all prompts");
    assert_eq!(
        snapshot.event.message_seqs,
        vec![inherited.seq, child_message.seq, latest.seq]
    );
    assert_eq!(
        snapshot.event.source,
        SessionTitleSource::Provider {
            provider: SessionTitleProviderId::new("fork-all"),
            model: None,
        }
    );
    assert_eq!(
        service.get(&parent).expect("parent snapshot").event.title,
        "Inherited title prompt"
    );
}

#[tokio::test]
async fn runs_a_first_prompt_provider_once_and_retries_only_through_refresh() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("title");

    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Provider {
        id: "first-model".to_owned(),
        automatic: SessionTitleAutomaticMode::FirstPrompt,
        calls: Arc::clone(&calls),
        requests: Arc::clone(&requests),
        respond: Box::new(move |request, _index| {
            Box::pin(async move {
                Ok(SessionTitleProviderResult {
                    title: "[31m  A   model-generated title that is too long  ".to_owned(),
                    message_seqs: vec![request.messages[0].seq],
                    model: Some(SessionTitleModelProvenance {
                        provider: "aux-route".to_owned(),
                        model: "title-model".to_owned(),
                    }),
                })
            })
        }),
    };
    service.register(Arc::new(provider)).expect("register");

    let session = sessions
        .create(
            &context,
            Some(SessionId::new("first-provider")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    append(&session, "turn/start", json!({"turn": 1}));
    let first = append_human_prompt(&session, "Explain asynchronous title generation");
    wait_for_title(&service, &session).await;
    assert_eq!(
        service.get(&session).expect("snapshot").event.source,
        SessionTitleSource::Fallback
    );

    append_route(&session, "initial");
    wait_for_calls(&calls, 1).await;

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    {
        let recorded = requests.lock();
        assert_eq!(recorded[0].messages.len(), 1);
        assert_eq!(recorded[0].messages[0].seq, first.seq);
        assert_eq!(
            recorded[0].messages[0].text,
            "Explain asynchronous title generation"
        );
        assert_eq!(
            recorded[0].route,
            Some(SessionTitleModelProvenance {
                provider: "main-route".to_owned(),
                model: "chat-model".to_owned(),
            })
        );
    }
    let snapshot = service.get(&session).expect("snapshot");
    assert_eq!(snapshot.event.title, "A model-generated title");
    assert_eq!(snapshot.event.message_seqs, vec![first.seq]);
    assert_eq!(
        snapshot.event.source,
        SessionTitleSource::Provider {
            provider: SessionTitleProviderId::new("first-model"),
            model: Some(SessionTitleModelProvenance {
                provider: "aux-route".to_owned(),
                model: "title-model".to_owned(),
            }),
        }
    );

    let second = append_human_prompt(&session, "A later prompt");
    append_route(&session, "change");
    settle().await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    service
        .refresh(&session, None)
        .await
        .expect("refresh")
        .expect("refreshed snapshot");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let recorded = requests.lock();
    assert_eq!(
        recorded[1]
            .messages
            .iter()
            .map(|message| message.seq)
            .collect::<Vec<_>>(),
        vec![first.seq, second.seq]
    );
}

#[tokio::test]
async fn rejects_a_second_provider_and_drains_stale_work_when_disposed() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("title");

    let gate = Arc::new(Notify::new());
    let aborted = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let winner = Provider {
        id: "winner".to_owned(),
        automatic: SessionTitleAutomaticMode::AllPrompts,
        calls: Arc::clone(&calls),
        requests: Arc::clone(&requests),
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
                        title: "stale provider result".to_owned(),
                        message_seqs: request.messages.iter().map(|message| message.seq).collect(),
                        model: None,
                    })
                })
            })
        },
    };
    let dispose = service.register(Arc::new(winner)).expect("register winner");

    let duplicate_calls = Arc::new(AtomicUsize::new(0));
    let duplicate_requests = Arc::new(Mutex::new(Vec::new()));
    let duplicate = Provider {
        id: "duplicate".to_owned(),
        automatic: SessionTitleAutomaticMode::FirstPrompt,
        calls: Arc::clone(&duplicate_calls),
        requests: Arc::clone(&duplicate_requests),
        respond: respond_all("duplicate"),
    };
    let duplicate_error = service
        .register(Arc::new(duplicate))
        .expect_err("duplicate registration");
    assert!(format!("{duplicate_error:#}").contains("already registered"));

    let session = sessions
        .create(
            &context,
            Some(SessionId::new("dispose-provider")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    append(&session, "turn/start", json!({"turn": 1}));
    let _ = append_human_prompt(&session, "Generate this title");
    wait_for_title(&service, &session).await;
    append_route(&session, "initial");
    wait_for_calls(&calls, 1).await;

    let observed_signal = requests.lock()[0].signal.clone();
    assert!(!observed_signal.is_aborted());

    let disposed = Arc::new(AtomicBool::new(false));
    let dispose_flag = Arc::clone(&disposed);
    let disposal = tokio::spawn(async move {
        dispose.dispose().await.expect("dispose winner");
        dispose_flag.store(true, Ordering::SeqCst);
    });
    for _ in 0..1_000 {
        if observed_signal.is_aborted() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(observed_signal.is_aborted());
    assert!(!disposed.load(Ordering::SeqCst));

    gate.notify_one();
    disposal.await.expect("join disposal");
    assert!(disposed.load(Ordering::SeqCst));
    assert_eq!(
        service.get(&session).expect("snapshot").event.source,
        SessionTitleSource::Fallback
    );

    let replacement_calls = Arc::new(AtomicUsize::new(0));
    let replacement_requests = Arc::new(Mutex::new(Vec::new()));
    let replacement = Provider {
        id: "replacement".to_owned(),
        automatic: SessionTitleAutomaticMode::FirstPrompt,
        calls: Arc::clone(&replacement_calls),
        requests: Arc::clone(&replacement_requests),
        respond: respond_all("replacement"),
    };
    let dispose_replacement = service
        .register(Arc::new(replacement))
        .expect("replacement");
    dispose_replacement
        .dispose()
        .await
        .expect("dispose replacement");
}

#[tokio::test]
async fn supersedes_an_older_all_messages_revision_and_cannot_commit_an_ignored_abort() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("title");

    let gate = Arc::new(Notify::new());
    let aborted = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Provider {
        id: "all-model".to_owned(),
        automatic: SessionTitleAutomaticMode::AllPrompts,
        calls: Arc::clone(&calls),
        requests: Arc::clone(&requests),
        respond: {
            let gate = Arc::clone(&gate);
            let aborted = Arc::clone(&aborted);
            Box::new(move |request, index| {
                let gate = Arc::clone(&gate);
                let aborted = Arc::clone(&aborted);
                Box::pin(async move {
                    if index == 1 {
                        gate.notified().await;
                        aborted.store(request.signal.is_aborted(), Ordering::SeqCst);
                        Ok(SessionTitleProviderResult {
                            title: "Old ignored result".to_owned(),
                            message_seqs: vec![request.messages[0].seq],
                            model: None,
                        })
                    } else {
                        Ok(SessionTitleProviderResult {
                            title: "Newest complete title".to_owned(),
                            message_seqs: request
                                .messages
                                .iter()
                                .map(|message| message.seq)
                                .collect(),
                            model: None,
                        })
                    }
                })
            })
        },
    };
    service.register(Arc::new(provider)).expect("register");

    let session = sessions
        .create(
            &context,
            Some(SessionId::new("supersede")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    append(&session, "turn/start", json!({"turn": 1}));
    let first = append_human_prompt(&session, "First prompt");
    wait_for_title(&service, &session).await;
    append_route(&session, "initial");
    wait_for_calls(&calls, 1).await;

    let second = append_human_prompt(&session, "Second prompt");
    assert!(requests.lock()[0].signal.is_aborted());
    append_route(&session, "change");
    wait_for_calls(&calls, 2).await;
    wait_for_title_matching(&service, &session, "Newest complete title").await;

    let snapshot = service.get(&session).expect("snapshot");
    assert_eq!(snapshot.event.title, "Newest complete title");
    assert_eq!(snapshot.event.message_seqs, vec![first.seq, second.seq]);

    gate.notify_one();
    for _ in 0..1_000 {
        if aborted.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    assert!(aborted.load(Ordering::SeqCst));
    assert_eq!(
        service.get(&session).expect("snapshot").event.title,
        "Newest complete title"
    );
}

#[tokio::test]
async fn runs_an_all_messages_revision_when_the_next_main_request_reuses_its_logged_header() {
    let context = Context::new();
    let llm = LlmRuntime::install(&context).expect("llm");
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("title");

    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Provider {
        id: "unchanged-route".to_owned(),
        automatic: SessionTitleAutomaticMode::AllPrompts,
        calls: Arc::clone(&calls),
        requests: Arc::clone(&requests),
        respond: {
            let calls = Arc::clone(&calls);
            Box::new(move |request, _index| {
                let calls = Arc::clone(&calls);
                Box::pin(async move {
                    Ok(SessionTitleProviderResult {
                        title: format!("Revision {}", calls.load(Ordering::SeqCst)),
                        message_seqs: request.messages.iter().map(|message| message.seq).collect(),
                        model: None,
                    })
                })
            })
        },
    };
    service.register(Arc::new(provider)).expect("register");

    let session = sessions
        .create(
            &context,
            Some(SessionId::new("unchanged-route")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    append(&session, "turn/start", json!({"turn": 1}));
    let first = append_human_prompt(&session, "First routed prompt");
    wait_for_title(&service, &session).await;
    append(&session, "step/start", json!({"turn": 1, "step": 1}));
    append_route(&session, "initial");
    wait_for_calls(&calls, 1).await;
    append(&session, "step/end", json!({"turn": 1, "step": 1}));
    append(
        &session,
        "turn/end",
        json!({"turn": 1, "reason": {"kind": "completed"}}),
    );

    append(&session, "turn/start", json!({"turn": 2}));
    let second = append_human_prompt(&session, "Second prompt on the same route");
    wait_for_title(&service, &session).await;
    append(&session, "step/start", json!({"turn": 2, "step": 1}));
    let mut options = GenerateOptions::new(
        ProviderId::new("main-route"),
        ModelId::new("chat-model"),
        session.derive_messages(),
    );
    options.session_id = Some(session.id().clone());
    let _ = llm
        .stream(options.mark_agent_loop_request())
        .collect::<Vec<_>>()
        .await;
    wait_for_calls(&calls, 2).await;

    assert_eq!(
        session
            .events()
            .iter()
            .filter(|event| event.event_type == "request/header")
            .count(),
        1
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let recorded = requests.lock();
    assert_eq!(
        recorded[1]
            .messages
            .iter()
            .map(|message| message.seq)
            .collect::<Vec<_>>(),
        vec![first.seq, second.seq]
    );
    assert_eq!(
        recorded[1].route,
        Some(SessionTitleModelProvenance {
            provider: "main-route".to_owned(),
            model: "chat-model".to_owned(),
        })
    );
    let _ = llm;
}

#[tokio::test]
async fn ignores_model_streams_that_are_not_a_matching_loop_request() {
    let context = Context::new();
    let llm = LlmRuntime::install(&context).expect("llm");
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("title");

    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Provider {
        id: "request-filter".to_owned(),
        automatic: SessionTitleAutomaticMode::AllPrompts,
        calls: Arc::clone(&calls),
        requests: Arc::clone(&requests),
        respond: respond_all("Unexpected title"),
    };
    service.register(Arc::new(provider)).expect("register");

    let base = || {
        GenerateOptions::new(
            ProviderId::new("main-route"),
            ModelId::new("chat-model"),
            Vec::new(),
        )
    };

    let _ = llm.stream(base()).collect::<Vec<_>>().await;
    let _ = llm
        .stream(base().mark_agent_loop_request())
        .collect::<Vec<_>>()
        .await;

    let mut missing = base().mark_agent_loop_request();
    missing.session_id = Some(SessionId::new("missing"));
    let _ = llm.stream(missing).collect::<Vec<_>>().await;

    let quiet = sessions
        .create(
            &context,
            Some(SessionId::new("quiet")),
            CreateSessionOptions::default(),
        )
        .expect("quiet");
    let mut quiet_options = base().mark_agent_loop_request();
    quiet_options.session_id = Some(quiet.id().clone());
    let _ = llm.stream(quiet_options).collect::<Vec<_>>().await;

    let pending = sessions
        .create(
            &context,
            Some(SessionId::new("unmatched-boundary")),
            CreateSessionOptions::default(),
        )
        .expect("pending");
    append(&pending, "turn/start", json!({"turn": 1}));
    append_human_prompt(&pending, "Wait for a matching request boundary");
    wait_for_title(&service, &pending).await;
    let mut pending_options = base().mark_agent_loop_request();
    pending_options.session_id = Some(pending.id().clone());
    let _ = llm.stream(pending_options).collect::<Vec<_>>().await;
    settle().await;

    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn contains_automatic_failures_but_lets_explicit_refresh_reject() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let service = SessionTitleService::install(&context, CONFIG).expect("title");

    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = Provider {
        id: "failing".to_owned(),
        automatic: SessionTitleAutomaticMode::AllPrompts,
        calls: Arc::clone(&calls),
        requests: Arc::clone(&requests),
        respond: Box::new(move |_request, _index| {
            Box::pin(async move { Err(anyhow::anyhow!("title backend failed")) })
        }),
    };
    service.register(Arc::new(provider)).expect("register");

    let session = sessions
        .create(
            &context,
            Some(SessionId::new("failure")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    append(&session, "turn/start", json!({"turn": 1}));
    append_human_prompt(&session, "Keep a fallback");
    wait_for_title(&service, &session).await;
    append_route(&session, "initial");
    settle().await;

    assert_eq!(
        service.get(&session).expect("snapshot").event.source,
        SessionTitleSource::Fallback
    );
    let error = service
        .refresh(&session, None)
        .await
        .expect_err("refresh rejects");
    assert!(format!("{error:#}").contains("title backend failed"));
}
