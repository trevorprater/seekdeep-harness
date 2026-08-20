//! Behavioral mirror of packages/session/session-title-all-prompts-llm/tests/provider.spec.ts.

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
    ModelId, ProviderId, StreamChunk,
};
use seekdeep_session_title::{SessionTitleConfig, SessionTitleService};
use seekdeep_session_title_all_prompts_llm::apply as apply_all_prompts;
use seekdeep_session_title_llm::SessionTitleLlmConfig;
use serde_json::json;

const TITLE_CONFIG: SessionTitleConfig = SessionTitleConfig {
    fallback_max_words: 5,
    fallback_max_bytes: 40,
    max_title_bytes: 80,
};

const LLM_CONFIG: SessionTitleLlmConfig = SessionTitleLlmConfig {
    target_words: 5,
    target_cjk_characters: 10,
    max_input_bytes: 1_000,
    max_output_tokens: 32,
    timeout_ms: 1_000,
    provider: None,
    model: None,
};

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
                text: "All messages model title".to_owned(),
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
async fn includes_seeded_history_and_the_latest_prompt_while_inheriting_the_route() {
    let seed = Session::create(&SessionId::new("seed-source"), None, None).expect("seed");
    append(&seed, "turn/start", json!({"turn": 1}));
    let inherited = append_surface(&seed, "user/message", user_message("inherited prompt"));
    append(
        &seed,
        "session/title",
        json!({"title": "Inherited fallback", "messageSeqs": [inherited.seq], "source": {"kind": "fallback"}}),
    );
    append(
        &seed,
        "turn/end",
        json!({"turn": 1, "reason": {"kind": "completed"}}),
    );
    let seed_events = seed.events();

    let context = Context::new();
    LlmRuntime::install(&context).expect("llm");
    let sessions = SessionStore::install(&context).expect("sessions");
    SessionTitleService::install(&context, TITLE_CONFIG).expect("title");
    let adapter = Arc::new(RecordingAdapter {
        requests: Mutex::new(Vec::new()),
    });
    let llm = context.get(LLM).expect("llm");
    llm.register_adapter(&["current-route".to_owned()], adapter.clone())
        .expect("adapter");
    apply_all_prompts(&context, &LLM_CONFIG).expect("provider");

    let session = sessions
        .create(
            &context,
            Some(SessionId::new("all-plugin")),
            CreateSessionOptions {
                seed: Some(seed_events.clone()),
                seed_length: Some(u64::try_from(seed_events.len()).expect("seed length")),
                parent_session: Some(seed.id().clone()),
                ..CreateSessionOptions::default()
            },
        )
        .expect("session");
    append(&session, "turn/start", json!({"turn": 2}));
    let latest = append_surface(&session, "user/message", user_message("latest prompt"));
    append(
        &session,
        "request/header",
        json!({"header": {"config": {"provider": "current-route", "model": "current-model"}}, "reason": "resume"}),
    );

    for _ in 0..1_000 {
        if !adapter.requests.lock().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let requests = adapter.requests.lock();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].provider, ProviderId::new("current-route"));
    assert_eq!(requests[0].model, ModelId::new("current-model"));
    let text = match &requests[0].messages[0].content()[0] {
        ContentBlock::Text { text } => text.as_str(),
        other => panic!("expected a text block, got {other:?}"),
    };
    assert!(text.contains("inherited prompt"));
    assert!(text.contains("latest prompt"));

    let title = context
        .get(seekdeep_session_title::SESSION_TITLE)
        .expect("title service");
    assert_eq!(
        title.get(&session).expect("snapshot").event.message_seqs,
        vec![inherited.seq, latest.seq]
    );
}
