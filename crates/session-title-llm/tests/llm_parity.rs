//! Behavioral mirror of packages/session/session-title-llm/tests/llm.spec.ts.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{
    AbortSignal, AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter,
    LlmFailure, LlmRequestPurpose, LlmRuntime, StreamChunk,
};
use seekdeep_session_title::{SessionTitleModelProvenance, SessionTitleProviderId};
use seekdeep_session_title_llm::{
    SessionTitleLlmConfig, generate_session_title_with_llm, resolve_session_title_llm_config,
};
use seekdeep_util::timeout::TimeoutReason;
use serde_json::json;

fn script() -> Vec<StreamChunk> {
    vec![
        StreamChunk::BlockStart {
            index: 0,
            block_type: "text".to_owned(),
        },
        StreamChunk::TextDelta {
            index: 0,
            text: "  五个字标题  ".to_owned(),
        },
        StreamChunk::Finish {
            reason: FinishReason::Stop,
            replay_state: None,
        },
    ]
}

fn config() -> SessionTitleLlmConfig {
    SessionTitleLlmConfig {
        target_words: 5,
        target_cjk_characters: 10,
        max_input_bytes: 1_000,
        max_output_tokens: 32,
        timeout_ms: 1_000,
        provider: None,
        model: None,
    }
}

const TITLE_PROVIDER: &str = "test-title-provider";

struct RecordingAdapter {
    script: Vec<StreamChunk>,
    requests: Mutex<Vec<GenerateOptions>>,
    on_dispatch: Option<Box<dyn Fn() + Send + Sync + 'static>>,
}

#[async_trait]
impl LlmAdapter for RecordingAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        if let Some(callback) = &self.on_dispatch {
            callback();
        }
        self.requests.lock().push(options);
        AdapterStream::new(stream::iter(self.script.clone().into_iter().map(Ok)))
    }
}

struct CooperativeAdapter;

#[async_trait]
impl LlmAdapter for CooperativeAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        let signal = options.signal.expect("signal");
        AdapterStream::new(stream::once(async move {
            signal.cancelled().await;
            let timeout = signal
                .typed_reason::<TimeoutReason>()
                .expect("timeout reason");
            Err(anyhow::Error::new((*timeout).clone()))
        }))
    }
}

struct DelayedSuccessAdapter {
    delay_ms: u64,
}

#[async_trait]
impl LlmAdapter for DelayedSuccessAdapter {
    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        let delay = Duration::from_millis(self.delay_ms);
        AdapterStream::new(
            stream::once(async move {
                tokio::time::sleep(delay).await;
                Ok(StreamChunk::BlockStart {
                    index: 0,
                    block_type: "text".to_owned(),
                })
            })
            .chain(stream::iter(vec![
                Ok(StreamChunk::TextDelta {
                    index: 0,
                    text: "  五个字标题  ".to_owned(),
                }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                }),
            ])),
        )
    }
}

fn user_message(text: &str) -> serde_json::Value {
    json!({
        "id": "u-message",
        "role": "user",
        "content": [{"type": "text", "text": text}],
        "source": {"kind": "user"},
    })
}

fn request(
    sessions: &Arc<SessionStore>,
    context: &Context,
    signal: AbortSignal,
    routed: bool,
) -> seekdeep_session_title::SessionTitleProviderRequest {
    let session = sessions
        .create(context, None, CreateSessionOptions::default())
        .expect("session");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("turn");
    let first = session
        .append(
            "user/message",
            user_message("first prompt"),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("first");
    let second = session
        .append(
            "user/message",
            user_message("第二个问题"),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("second");
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("turn end");
    seekdeep_session_title::SessionTitleProviderRequest {
        session,
        messages: vec![
            seekdeep_session_title::SessionTitleUserMessage {
                seq: first.seq,
                text: "first prompt".to_owned(),
            },
            seekdeep_session_title::SessionTitleUserMessage {
                seq: second.seq,
                text: "第二个问题".to_owned(),
            },
        ],
        route: if routed {
            Some(SessionTitleModelProvenance {
                provider: "current-route".to_owned(),
                model: "current-model".to_owned(),
            })
        } else {
            None
        },
        signal,
    }
}

fn harness() -> (Context, Arc<SessionStore>, Arc<LlmRuntime>) {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let llm = LlmRuntime::install(&context).expect("llm");
    (context, sessions, llm)
}

#[tokio::test]
async fn uses_the_exact_logged_route_targets_full_input_and_output_cap() {
    let (context, sessions, llm) = harness();
    let provider_request = request(&sessions, &context, AbortSignal::default(), true);
    let logged_at_dispatch = Arc::new(AtomicBool::new(false));
    let session_for_dispatch = provider_request.session.clone();
    let flag = Arc::clone(&logged_at_dispatch);
    let adapter = Arc::new(RecordingAdapter {
        script: script(),
        requests: Mutex::new(Vec::new()),
        on_dispatch: Some(Box::new(move || {
            flag.store(
                session_for_dispatch
                    .events()
                    .iter()
                    .any(|event| event.event_type == "session/title-llm-request"),
                Ordering::SeqCst,
            );
        })),
    });
    llm.register_adapter(&["current-route".to_owned()], adapter.clone())
        .expect("adapter");

    let result = generate_session_title_with_llm(
        &context,
        &resolve_session_title_llm_config(&config()).expect("config"),
        &provider_request,
        &provider_request.messages,
        &SessionTitleProviderId::new(TITLE_PROVIDER),
    )
    .await
    .expect("result");

    assert_eq!(result.title, "五个字标题");
    assert_eq!(
        result.message_seqs,
        provider_request
            .messages
            .iter()
            .map(|message| message.seq)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        result.model,
        Some(SessionTitleModelProvenance {
            provider: "current-route".to_owned(),
            model: "current-model".to_owned(),
        })
    );
    assert!(logged_at_dispatch.load(Ordering::SeqCst));

    let requests = adapter.requests.lock();
    assert_eq!(requests.len(), 1);
    let options = &requests[0];
    assert!(!seekdeep_llm::is_agent_loop_request(options));
    assert_eq!(options.provider.as_str(), "current-route");
    assert_eq!(options.model.as_str(), "current-model");
    assert_eq!(options.max_tokens, Some(32));
    assert_eq!(
        options.session_id,
        Some(provider_request.session.id().clone())
    );
    assert_eq!(options.purpose, Some(LlmRequestPurpose::SessionTitle));
    let system = options.system.as_deref().expect("system");
    assert!(system.contains("5 words"));
    assert!(system.contains("10 CJK characters"));
    let prompt = match &options.messages[0].content()[0] {
        ContentBlock::Text { text } => text.as_str(),
        other => panic!("expected text block, got {other:?}"),
    };
    assert!(prompt.contains("first prompt"));
    assert!(prompt.contains("第二个问题"));
}

#[tokio::test]
async fn uses_paired_explicit_overrides_and_bounds_the_framed_input() {
    let (context, sessions, llm) = harness();
    let adapter = Arc::new(RecordingAdapter {
        script: script(),
        requests: Mutex::new(Vec::new()),
        on_dispatch: None,
    });
    llm.register_adapter(&["explicit-route".to_owned()], adapter.clone())
        .expect("adapter");

    let oversized = request(&sessions, &context, AbortSignal::default(), true);
    let selected = oversized.messages[0].clone();
    let raw_input_bytes = "first prompt".len();
    let mut tight = resolve_session_title_llm_config(&config()).expect("config");
    tight.provider = Some("explicit-route".to_owned());
    tight.model = Some("explicit-model".to_owned());
    tight.max_input_bytes = raw_input_bytes as u64;

    let error = generate_session_title_with_llm(
        &context,
        &tight,
        &oversized,
        &[selected],
        &SessionTitleProviderId::new(TITLE_PROVIDER),
    )
    .await
    .expect_err("oversized input");
    assert!(format!("{error:#}").contains("maxInputBytes"));
    assert!(adapter.requests.lock().is_empty());
    assert!(
        !oversized
            .session
            .events()
            .iter()
            .any(|event| event.event_type == "session/title-llm-request")
    );

    let mut within = tight;
    within.max_input_bytes = 1_000;
    let within_request = request(&sessions, &context, AbortSignal::default(), true);
    generate_session_title_with_llm(
        &context,
        &within,
        &within_request,
        &within_request.messages[..1],
        &SessionTitleProviderId::new(TITLE_PROVIDER),
    )
    .await
    .expect("within limit");
    let requests = adapter.requests.lock();
    assert_eq!(requests[0].provider.as_str(), "explicit-route");
    assert_eq!(requests[0].model.as_str(), "explicit-model");
}

#[test]
fn requires_every_deployment_limit_and_a_complete_route_pair() {
    let base = config();
    assert!(
        resolve_session_title_llm_config(&SessionTitleLlmConfig {
            target_words: 0,
            ..base.clone()
        })
        .is_err_and(|e| format!("{e:#}").contains("targetWords"))
    );

    assert!(
        resolve_session_title_llm_config(&SessionTitleLlmConfig {
            provider: Some("only-provider".to_owned()),
            model: None,
            ..base.clone()
        })
        .is_err_and(|e| format!("{e:#}").contains("supplied together"))
    );
    assert!(
        resolve_session_title_llm_config(&SessionTitleLlmConfig {
            provider: None,
            model: Some("only-model".to_owned()),
            ..base.clone()
        })
        .is_err_and(|e| format!("{e:#}").contains("supplied together"))
    );
    assert!(
        resolve_session_title_llm_config(&SessionTitleLlmConfig {
            provider: Some(String::new()),
            model: Some("model".to_owned()),
            ..base.clone()
        })
        .is_err_and(|e| format!("{e:#}").contains("non-empty strings"))
    );
    assert!(
        resolve_session_title_llm_config(&SessionTitleLlmConfig {
            timeout_ms: 2_147_483_648,
            ..base
        })
        .is_err_and(|e| format!("{e:#}").contains("must not exceed"))
    );
    assert!(resolve_session_title_llm_config(&config()).is_ok());
}

#[tokio::test]
async fn rejects_an_absent_route_empty_selection_and_pre_aborted_caller() {
    let (context, sessions, llm) = harness();
    let adapter = Arc::new(RecordingAdapter {
        script: script(),
        requests: Mutex::new(Vec::new()),
        on_dispatch: None,
    });
    llm.register_adapter(&["current-route".to_owned()], adapter.clone())
        .expect("adapter");
    let resolved = resolve_session_title_llm_config(&config()).expect("config");

    let unrouted = request(&sessions, &context, AbortSignal::default(), false);
    let error = generate_session_title_with_llm(
        &context,
        &resolved,
        &unrouted,
        &unrouted.messages,
        &SessionTitleProviderId::new(TITLE_PROVIDER),
    )
    .await
    .expect_err("no route");
    assert!(format!("{error:#}").contains("no logged request route"));

    let empty = request(&sessions, &context, AbortSignal::default(), true);
    let error = generate_session_title_with_llm(
        &context,
        &resolved,
        &empty,
        &[],
        &SessionTitleProviderId::new(TITLE_PROVIDER),
    )
    .await
    .expect_err("empty selection");
    assert!(format!("{error:#}").contains("at least one source message"));

    let aborted_signal = AbortSignal::default();
    aborted_signal.abort_with_reason(json!("caller stopped"));
    let aborted = request(&sessions, &context, aborted_signal, true);
    let error = generate_session_title_with_llm(
        &context,
        &resolved,
        &aborted,
        &aborted.messages,
        &SessionTitleProviderId::new(TITLE_PROVIDER),
    )
    .await
    .expect_err("pre-aborted");
    assert!(format!("{error:#}").contains("caller stopped"));
    assert!(adapter.requests.lock().is_empty());
}

#[tokio::test]
async fn preserves_terminal_failure_details() {
    let (context, sessions, llm) = harness();
    let adapter = Arc::new(RecordingAdapter {
        script: vec![StreamChunk::Finish {
            reason: FinishReason::Error {
                failure: LlmFailure {
                    message: "provider failed".to_owned(),
                    code: "SERVER".to_owned(),
                    status: None,
                    provider_retry_after_ms: None,
                    request_id: None,
                },
            },
            replay_state: None,
        }],
        requests: Mutex::new(Vec::new()),
        on_dispatch: None,
    });
    llm.register_adapter(&["current-route".to_owned()], adapter.clone())
        .expect("adapter");
    let provider_request = request(&sessions, &context, AbortSignal::default(), true);
    let error = generate_session_title_with_llm(
        &context,
        &resolve_session_title_llm_config(&config()).expect("config"),
        &provider_request,
        &provider_request.messages,
        &SessionTitleProviderId::new(TITLE_PROVIDER),
    )
    .await
    .expect_err("error finish");
    let llm_error = error
        .downcast_ref::<seekdeep_llm::LlmError>()
        .expect("llm error");
    assert_eq!(llm_error.message(), "provider failed");
    assert_eq!(llm_error.code(), "SERVER");
}

#[tokio::test]
async fn rejects_tool_call_blocks_and_no_text_responses() {
    let (context, sessions, llm) = harness();

    let tool_adapter = Arc::new(RecordingAdapter {
        script: vec![
            StreamChunk::BlockStart {
                index: 0,
                block_type: "tool-call".to_owned(),
            },
            StreamChunk::ToolCallDelta {
                index: 0,
                id: CallId::new("title-tool"),
                name: Some("unexpected".to_owned()),
                arguments_delta: "{}".to_owned(),
            },
            StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            },
        ],
        requests: Mutex::new(Vec::new()),
        on_dispatch: None,
    });
    llm.register_adapter(&["current-route".to_owned()], tool_adapter)
        .expect("tool adapter");
    let tool_request = request(&sessions, &context, AbortSignal::default(), true);
    let error = generate_session_title_with_llm(
        &context,
        &resolve_session_title_llm_config(&config()).expect("config"),
        &tool_request,
        &tool_request.messages,
        &SessionTitleProviderId::new(TITLE_PROVIDER),
    )
    .await
    .expect_err("tool call");
    assert!(format!("{error:#}").contains("text only"));
}

#[tokio::test]
async fn rejects_a_successful_response_with_no_text() {
    let (context, sessions, llm) = harness();
    let reasoning_adapter = Arc::new(RecordingAdapter {
        script: vec![
            StreamChunk::BlockStart {
                index: 0,
                block_type: "reasoning".to_owned(),
            },
            StreamChunk::ReasoningDelta {
                index: 0,
                text: "no final title".to_owned(),
            },
            StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            },
        ],
        requests: Mutex::new(Vec::new()),
        on_dispatch: None,
    });
    llm.register_adapter(&["current-route".to_owned()], reasoning_adapter)
        .expect("reasoning adapter");
    let reasoning_request = request(&sessions, &context, AbortSignal::default(), true);
    let error = generate_session_title_with_llm(
        &context,
        &resolve_session_title_llm_config(&config()).expect("config"),
        &reasoning_request,
        &reasoning_request.messages,
        &SessionTitleProviderId::new(TITLE_PROVIDER),
    )
    .await
    .expect_err("no text");
    assert!(format!("{error:#}").contains("produced no text"));
}

#[tokio::test(start_paused = true)]
async fn aborts_a_cooperative_stream_at_the_deadline() {
    let (context, sessions, llm) = harness();
    llm.register_adapter(&["current-route".to_owned()], Arc::new(CooperativeAdapter))
        .expect("adapter");
    let provider_request = request(&sessions, &context, AbortSignal::default(), true);
    let mut tight = resolve_session_title_llm_config(&config()).expect("config");
    tight.timeout_ms = 10;

    let pending = {
        let context = context.clone();
        let provider_request = provider_request.clone();
        let messages = provider_request.messages.clone();
        tokio::spawn(async move {
            generate_session_title_with_llm(
                &context,
                &tight,
                &provider_request,
                &messages,
                &SessionTitleProviderId::new(TITLE_PROVIDER),
            )
            .await
        })
    };
    tokio::time::advance(Duration::from_millis(10)).await;
    let error = pending.await.expect("join").expect_err("timeout");
    let reason = error
        .downcast_ref::<TimeoutReason>()
        .expect("timeout reason");
    assert_eq!(reason.code, "SESSION_TITLE_TIMEOUT");
    assert!((reason.timeout_ms - 10.0).abs() < f64::EPSILON);
}

#[tokio::test(start_paused = true)]
async fn rejects_a_successful_stream_that_completes_after_the_deadline() {
    let (context, sessions, llm) = harness();
    llm.register_adapter(
        &["current-route".to_owned()],
        Arc::new(DelayedSuccessAdapter { delay_ms: 20 }),
    )
    .expect("adapter");
    let provider_request = request(&sessions, &context, AbortSignal::default(), true);
    let mut tight = resolve_session_title_llm_config(&config()).expect("config");
    tight.timeout_ms = 10;

    let pending = {
        let context = context.clone();
        let provider_request = provider_request.clone();
        let messages = provider_request.messages.clone();
        tokio::spawn(async move {
            generate_session_title_with_llm(
                &context,
                &tight,
                &provider_request,
                &messages,
                &SessionTitleProviderId::new(TITLE_PROVIDER),
            )
            .await
        })
    };
    tokio::time::advance(Duration::from_millis(20)).await;
    let error = pending.await.expect("join").expect_err("timeout");
    let reason = error
        .downcast_ref::<TimeoutReason>()
        .expect("timeout reason");
    assert_eq!(reason.code, "SESSION_TITLE_TIMEOUT");
    assert!((reason.timeout_ms - 10.0).abs() < f64::EPSILON);
}
