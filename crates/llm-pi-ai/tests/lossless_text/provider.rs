use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt as _;
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_llm::{
    ContentBlock, FinishReason, GenerateOptions, JsonString, LlmAdapter, Message, MessageRole,
    MessageSource, ModelId, ProviderId, StreamChunk,
};
use seekdeep_llm_pi_ai::{
    adapter::{
        BoxPiEventStream, PiAiAdapter, PiAiAdapterOptions, PiApiKeyResolver, PiExecutionRequest,
        PiProfileSource, PiProtocolExecutor, PiResolvedAuth,
    },
    anthropic_messages::AnthropicMessagesExecutor,
    catalog::builtin_catalog,
    config::{ResolvedPiProviderProfile, resolve_profiles},
    context::PiContext,
    google_generative::GoogleGenerativeExecutor,
    openai_completions::OpenAiCompletionsExecutor,
    openai_responses::OpenAiResponsesExecutor,
};
use seekdeep_lossless_json::JsonValue;
use serde_json::json;

#[allow(dead_code)]
#[path = "../support/mock_server.rs"]
mod mock_server;

use super::support::{samples, source_call, tool_request};
use mock_server::{Behavior, MockServer};

struct Profiles(Arc<IndexMap<String, ResolvedPiProviderProfile>>);
impl PiProfileSource for Profiles {
    fn profiles(&self) -> Arc<IndexMap<String, ResolvedPiProviderProfile>> {
        self.0.clone()
    }
}

struct Key;
#[async_trait]
impl PiApiKeyResolver for Key {
    async fn resolve(
        &self,
        _: &ProviderId,
        _: &ResolvedPiProviderProfile,
    ) -> anyhow::Result<PiResolvedAuth> {
        Ok(PiResolvedAuth::api_key(Some("fixture-key".into())))
    }
}

type ExecutionCapture = Arc<Mutex<Option<PiExecutionRequest>>>;

struct CaptureExecutor {
    inner: Arc<dyn PiProtocolExecutor>,
    request: ExecutionCapture,
}

#[derive(Clone, Copy, Debug)]
enum Placement {
    User,
    Assistant,
    ToolResult,
}

#[derive(Clone, Copy)]
struct ProviderCase {
    provider: &'static str,
    model: &'static str,
    module: &'static str,
    field: &'static str,
}

fn message_request(case: ProviderCase, placement: Placement, text: JsonString) -> GenerateOptions {
    let (role, source) = match placement {
        Placement::User => (MessageRole::User, MessageSource::user()),
        Placement::Assistant => (
            MessageRole::Assistant,
            MessageSource::model(case.provider, case.model),
        ),
        Placement::ToolResult => return tool_request(case.provider, case.model, text),
    };
    GenerateOptions::new(
        ProviderId::new(case.provider),
        ModelId::new(case.model),
        vec![Message::new(
            role,
            vec![ContentBlock::Text { text }],
            source,
        )],
    )
}
impl PiProtocolExecutor for CaptureExecutor {
    fn stream(&self, request: PiExecutionRequest) -> anyhow::Result<BoxPiEventStream> {
        *self.request.lock() = Some(request.clone());
        self.inner.stream(request)
    }
}

#[tokio::test]
async fn real_http_text_payloads_apply_only_the_pinned_provider_normalization() {
    for (provider, model, module, field) in [
        (
            "deepseek",
            "deepseek-v4-flash",
            "openai-completions",
            "messages",
        ),
        ("openai", "gpt-4.1", "openai-responses", "input"),
        (
            "anthropic",
            "claude-sonnet-4-5",
            "anthropic-messages",
            "messages",
        ),
        (
            "google",
            "gemini-2.5-flash",
            "google-generative-ai",
            "contents",
        ),
    ] {
        let case = ProviderCase {
            provider,
            model,
            module,
            field,
        };
        for placement in [Placement::User, Placement::Assistant, Placement::ToolResult] {
            for text in samples() {
                verify_request(case, placement, text).await;
            }
        }
    }
    println!(
        "Pi-ai: 96 source/native cases matched (95 HTTP requests and 1 Google preflight rejection); neutral contexts retained exact UTF16 code units"
    );
}

async fn verify_request(case: ProviderCase, placement: Placement, text: JsonString) {
    let ProviderCase { module, field, .. } = case;
    let server = MockServer::start(vec![Behavior {
        status: Some(400),
        body: Some(r#"{"error":{"message":"captured request"}}"#.into()),
        ..Behavior::default()
    }])
    .await;
    let (adapter, captured) = fixture_adapter(case, &server.url);
    let request = message_request(case, placement, text.clone());
    let source_messages = JsonValue::from_serialize(&request.messages).unwrap();
    let events = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        adapter.stream(request).collect::<Vec<_>>(),
    )
    .await
    .unwrap();
    let request = captured
        .lock()
        .take()
        .expect("adapter must reach its protocol executor");
    let source = source_call(&JsonValue::object([
        ("op", json!("provider").into()),
        ("module", json!(module).into()),
        ("messages", source_messages),
        ("model", JsonValue::from_serialize(&request.model).unwrap()),
    ]));
    assert_eq!(
        request.context,
        source
            .get("context")
            .unwrap()
            .deserialize::<PiContext>()
            .unwrap(),
        "{module} {placement:?} neutral text {:?}",
        text.utf16_units()
    );
    let expected: serde_json::Value = source.get("body").unwrap().deserialize().unwrap();
    if module == "google-generative-ai" && expected[field].as_array().is_some_and(Vec::is_empty) {
        assert!(
            source
                .get("errorMessage")
                .unwrap()
                .deserialize::<String>()
                .unwrap()
                .contains("contents are required")
        );
        assert_eq!(source.get("networkCalls").unwrap().as_u64(), Some(0));
        assert!(server.requests().is_empty());
        assert!(events.iter().any(|event| matches!(event, Ok(StreamChunk::Finish { reason: FinishReason::Error { failure }, .. }) if failure.message == "contents are required")));
        return;
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), server.wait_for_closed(1))
        .await
        .unwrap_or_else(|_| panic!("{module} {placement:?} text {:?}: no completed HTTP request; events {events:?}; captured {:?}", text.utf16_units(), server.requests()));
    assert_eq!(server.requests().len(), 1);
    let actual = server.requests().remove(0).body.unwrap();
    assert_eq!(
        actual[field],
        expected[field],
        "{module} {placement:?} text {:?}",
        text.utf16_units()
    );
}

fn fixture_adapter(case: ProviderCase, url: &str) -> (PiAiAdapter, ExecutionCapture) {
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let executor: Arc<dyn PiProtocolExecutor> = match case.module {
        "openai-completions" => Arc::new(OpenAiCompletionsExecutor::new(http)),
        "openai-responses" => Arc::new(OpenAiResponsesExecutor::new(http)),
        "anthropic-messages" => Arc::new(AnthropicMessagesExecutor::new(http)),
        "google-generative-ai" => Arc::new(GoogleGenerativeExecutor::new(http)),
        _ => unreachable!("closed provider cases"),
    };
    let profile = json!({case.provider: {"apiKeyEnv":"FIXTURE_KEY", "baseURL":url, "cacheRetention":"none", "models":[{"id":case.model}]}});
    let profiles = Arc::new(resolve_profiles(Some(&profile), builtin_catalog()).unwrap());
    let captured = Arc::new(Mutex::new(None));
    let adapter = PiAiAdapter::new(PiAiAdapterOptions {
        profiles: Arc::new(Profiles(profiles)),
        api_keys: Arc::new(Key),
        attachments: None,
        executor: Arc::new(CaptureExecutor {
            inner: executor,
            request: captured.clone(),
        }),
    });
    (adapter, captured)
}
