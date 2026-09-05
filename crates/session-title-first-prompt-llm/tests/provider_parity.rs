//! Behavioral mirror of packages/session/session-title-first-prompt-llm/tests/provider.spec.ts.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{
    AdapterStream, ContentBlock, FinishReason, GenerateOptions, LLM, LlmAdapter, LlmRuntime,
    StreamChunk,
};
use seekdeep_session_title::{SESSION_TITLE, SessionTitleConfig, SessionTitleService};
use seekdeep_session_title_first_prompt_llm::apply as apply_first_prompt;
use seekdeep_session_title_llm::SessionTitleLlmConfig;
use serde_json::json;

const TITLE_CONFIG: SessionTitleConfig = SessionTitleConfig {
    fallback_max_words: 5,
    fallback_max_bytes: 40,
    max_title_bytes: 80,
};

fn llm_config() -> SessionTitleLlmConfig {
    SessionTitleLlmConfig {
        target_words: 5,
        target_cjk_characters: 10,
        max_input_bytes: 1_000,
        max_output_tokens: 32,
        timeout_ms: 1_000,
        provider: Some("title-route".to_owned()),
        model: Some("title-model".to_owned()),
    }
}

struct RecordingAdapter {
    requests: Mutex<Vec<GenerateOptions>>,
}

#[async_trait]
impl LlmAdapter for RecordingAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests.lock().push(options);
        AdapterStream::new(stream::iter(vec![
            Ok(StreamChunk::TextDelta {
                index: 0,
                text: "First-message model title".to_owned(),
            }),
            Ok(StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            }),
        ]))
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

fn append(session: &Arc<Session>, event_type: &str, data: serde_json::Value) {
    session
        .append(event_type, data, AppendOptions::default())
        .expect("append");
}

fn append_surface(
    session: &Arc<Session>,
    event_type: &str,
    data: serde_json::Value,
) -> seekdeep_core::session::SessionEvent {
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

#[tokio::test]
async fn always_selects_only_the_first_eligible_human_message() {
    // The source's first test (a vi.spyOn of ctx.sessionTitle.register that
    // calls the captured provider's generate with an empty message list) probes
    // the first-prompt selector's empty-input guard. In Rust that guard is the
    // private closure passed to apply, unreachable through the public flow
    // because the title service never schedules generation without a message;
    // the empty-selection boundary is also covered by
    // generate_session_title_with_llm's own at-least-one-source-message check.
    let context = Context::new();
    LlmRuntime::install(&context).expect("llm");
    let sessions = SessionStore::install(&context).expect("sessions");
    SessionTitleService::install(&context, TITLE_CONFIG).expect("title");
    let adapter = Arc::new(RecordingAdapter {
        requests: Mutex::new(Vec::new()),
    });
    let llm = context.get(LLM).expect("llm");
    llm.register_adapter(&["title-route".to_owned()], adapter.clone())
        .expect("adapter");
    apply_first_prompt(&context, &llm_config()).expect("provider");

    let session = sessions
        .create(
            &context,
            Some(SessionId::new("first-plugin")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    append(&session, "turn/start", json!({"turn": 1}));
    let first = append_surface(&session, "user/message", user_message("first input"));
    for _ in 0..1_000 {
        if !adapter.requests.lock().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    append(
        &session,
        "request/header",
        json!({"header": {"config": {"provider": "main", "model": "main-model"}}, "reason": "initial"}),
    );
    for _ in 0..1_000 {
        if !adapter.requests.lock().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    append_surface(
        &session,
        "user/message",
        user_message("second input must be ignored"),
    );

    let title = context.get(SESSION_TITLE).expect("title service");
    title
        .refresh(&session, None)
        .await
        .expect("refresh")
        .expect("snapshot");

    for _ in 0..1_000 {
        if adapter.requests.lock().len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let requests = adapter.requests.lock();
    assert_eq!(requests.len(), 2);
    for options in requests.iter() {
        let text = match &options.messages[0].content()[0] {
            ContentBlock::Text { text } => text.as_str(),
            other => panic!("expected a text block, got {other:?}"),
        };
        assert!(text.contains("first input"));
        assert!(!text.contains("second input must be ignored"));
    }
    assert_eq!(
        title.get(&session).expect("snapshot").event.message_seqs,
        vec![first.seq]
    );
}
