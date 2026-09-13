//! OpenAI-compatible Chat Completions executor integration tests.

use std::sync::Arc;

use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use indexmap::IndexMap;
use seekdeep_llm::{
    AdapterRejection, GenerateOptions, LlmAdapter, ModelId, ProviderId, SessionId, StreamChunk,
    user_agent,
};
use seekdeep_llm_pi_ai::{
    adapter::{PiAiAdapter, PiAiAdapterOptions, PiApiKeyResolver, PiProfileSource, PiResolvedAuth},
    catalog::builtin_catalog,
    config::{ResolvedPiProviderProfile, resolve_profiles},
    openai_completions::OpenAiCompletionsExecutor,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
};

struct StaticProfiles(Arc<IndexMap<String, ResolvedPiProviderProfile>>);

impl PiProfileSource for StaticProfiles {
    fn profiles(&self) -> Arc<IndexMap<String, ResolvedPiProviderProfile>> {
        self.0.clone()
    }
}

struct StaticKey(Option<String>);

#[async_trait]
impl PiApiKeyResolver for StaticKey {
    async fn resolve(
        &self,
        _provider: &ProviderId,
        _profile: &ResolvedPiProviderProfile,
    ) -> anyhow::Result<PiResolvedAuth> {
        Ok(PiResolvedAuth::api_key(self.0.clone()))
    }
}

struct Captured {
    request: String,
    body: Value,
}

async fn server(
    status: u16,
    events: Vec<String>,
    error_body: Option<String>,
) -> (String, oneshot::Receiver<Captured>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let header_end;
        loop {
            let mut buffer = [0_u8; 4096];
            let read = socket.read(&mut buffer).await.unwrap();
            if read == 0 {
                return;
            }
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                header_end = index + 4;
                break;
            }
        }
        let headers = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
        let length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or_default();
        while bytes.len() - header_end < length {
            let mut buffer = vec![0_u8; length - (bytes.len() - header_end)];
            let read = socket.read(&mut buffer).await.unwrap();
            bytes.extend_from_slice(&buffer[..read]);
        }
        let body: Value = serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
        let _ = tx.send(Captured {
            request: headers,
            body,
        });
        if status != 200 {
            let body = error_body.unwrap_or_else(|| "{}".to_owned());
            socket.write_all(format!(
                "HTTP/1.1 {status} Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            ).as_bytes()).await.unwrap();
            return;
        }
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        for event in events {
            socket
                .write_all(format!("data: {event}\n\n").as_bytes())
                .await
                .unwrap();
        }
    });
    (format!("http://{address}"), rx)
}

fn adapter(base_url: &str, key: Option<String>, overrides: Value) -> PiAiAdapter {
    let Value::Object(overrides) = overrides else {
        panic!("test overrides must be an object")
    };
    let mut profile = json!({
        "apiKeyEnv":"PI_TEST_KEY",
        "baseURL":base_url,
        "headers":{"x-company":"private","User-Agent":"wrong"}
    });
    profile.as_object_mut().unwrap().extend(overrides);
    let raw = json!({"deepseek":profile});
    let profiles = Arc::new(resolve_profiles(Some(&raw), builtin_catalog()).unwrap());
    PiAiAdapter::new(PiAiAdapterOptions {
        profiles: Arc::new(StaticProfiles(profiles)),
        api_keys: Arc::new(StaticKey(key)),
        executor: Arc::new(OpenAiCompletionsExecutor::new(reqwest::Client::new())),
        attachments: None,
    })
}

fn mistral_adapter(base_url: &str) -> PiAiAdapter {
    let raw = json!({"mistral":{
        "apiKeyEnv":"MISTRAL_KEY","baseURL":base_url,"models":[{"id":"codestral-latest"}]
    }});
    let profiles = Arc::new(resolve_profiles(Some(&raw), builtin_catalog()).unwrap());
    PiAiAdapter::new(PiAiAdapterOptions {
        profiles: Arc::new(StaticProfiles(profiles)),
        api_keys: Arc::new(StaticKey(Some("mistral-key".to_owned()))),
        executor: Arc::new(OpenAiCompletionsExecutor::new_mistral(
            reqwest::Client::new(),
        )),
        attachments: None,
    })
}

fn request() -> GenerateOptions {
    GenerateOptions::new(
        ProviderId::new("deepseek"),
        ModelId::new("deepseek-v4-flash"),
        vec![],
    )
}

async fn collect(
    adapter: &PiAiAdapter,
    options: GenerateOptions,
) -> anyhow::Result<Vec<StreamChunk>> {
    adapter
        .stream(options)
        .map(|result| result.map_err(AdapterRejection::into_anyhow))
        .try_collect()
        .await
}

fn text_events() -> Vec<String> {
    vec![
        r#"{"choices":[{"delta":{"role":"assistant","content":""},"index":0,"finish_reason":null}]}"#.to_owned(),
        r#"{"choices":[{"delta":{"content":"hello"},"index":0,"finish_reason":null}]}"#.to_owned(),
        r#"{"choices":[{"delta":{},"index":0,"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#.to_owned(),
        "[DONE]".to_owned(),
    ]
}

#[tokio::test]
async fn executes_text_stream_and_emits_usage_finish_and_replay() {
    let (url, captured) = server(200, text_events(), None).await;
    let adapter = adapter(&url, Some("test-key".to_owned()), json!({}));
    let chunks = collect(&adapter, request()).await.unwrap();
    assert_eq!(
        serde_json::to_value(chunks).unwrap(),
        json!([
            {"type":"block-start","index":0,"blockType":"text"},
            {"type":"text-delta","index":0,"text":"hello"},
            {"type":"block-end","index":0,"block":{"type":"text","text":"hello"}},
            {"type":"usage","usage":{"inputTokens":3,"outputTokens":1}},
            {"type":"finish","reason":{"kind":"stop"},"replayState":{"kind":"pi-ai","version":1,"api":"openai-completions","provider":"deepseek","model":"deepseek-v4-flash","stopReason":"stop","blocks":[{"type":"text"}]}}
        ])
    );
    assert!(
        captured
            .await
            .unwrap()
            .request
            .starts_with("POST /chat/completions")
    );
}

#[tokio::test]
async fn forwards_reasoning_sampling_session_and_attribution_headers() {
    let (url, captured) = server(200, text_events(), None).await;
    let adapter = adapter(
        &url,
        Some("test-key".to_owned()),
        json!({"reasoning":"max"}),
    );
    let mut options = request();
    options.temperature = Some(0.2);
    options.max_tokens = Some(77);
    options.session_id = Some(SessionId::new("session-for-pi"));
    collect(&adapter, options).await.unwrap();
    let captured = captured.await.unwrap();
    assert_eq!(captured.body["model"], json!("deepseek-v4-flash"));
    assert_eq!(captured.body["temperature"], json!(0.2));
    assert_eq!(captured.body["max_completion_tokens"], json!(77));
    assert_eq!(captured.body["thinking"], json!({"type":"enabled"}));
    assert_eq!(captured.body["reasoning_effort"], json!("max"));
    let headers = captured.request.to_ascii_lowercase();
    assert!(headers.contains("authorization: bearer test-key"));
    assert!(headers.contains("x-company: private"));
    assert!(headers.contains(&format!("user-agent: {}", user_agent()).to_ascii_lowercase()));
    assert!(!headers.contains("user-agent: wrong"));
}

#[tokio::test]
async fn maps_http_failures_as_terminal_error_events_without_sdk_retry() {
    for (status, expected) in [
        (401, "AUTH"),
        (400, "INVALID_REQUEST"),
        (429, "RATE_LIMIT"),
        (500, "SERVER"),
    ] {
        let (url, captured) = server(
            status,
            vec![],
            Some(format!(r#"{{"error":{{"message":"provider {status}"}}}}"#)),
        )
        .await;
        let adapter = adapter(&url, Some("test-key".to_owned()), json!({}));
        let chunks = collect(&adapter, request()).await.unwrap();
        let finish = chunks.last().unwrap();
        let StreamChunk::Finish { reason, .. } = finish else {
            panic!("finish")
        };
        assert_eq!(reason.kind(), "error");
        let value = serde_json::to_value(reason).unwrap();
        assert_eq!(value["failure"]["code"], json!(expected));
        let _ = captured.await.unwrap();
    }
}

#[tokio::test]
async fn parses_reasoning_and_fragmented_tool_calls() {
    let events = vec![
        r#"{"choices":[{"delta":{"reasoning_content":"hmm"},"finish_reason":null}]}"#.to_owned(),
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"f","arguments":"{\"a\""}}]},"finish_reason":null}]}"#.to_owned(),
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":1}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":2,"completion_tokens":3}}"#.to_owned(),
        "[DONE]".to_owned(),
    ];
    let (url, _captured) = server(200, events, None).await;
    let adapter = adapter(&url, Some("test-key".to_owned()), json!({}));
    let chunks = collect(&adapter, request()).await.unwrap();
    assert!(
        chunks.iter().any(
            |chunk| matches!(chunk, StreamChunk::ReasoningDelta { text, .. } if text == "hmm")
        )
    );
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        StreamChunk::BlockEnd { block: seekdeep_llm::ContentBlock::ToolCall { id, name, arguments }, .. }
            if id.as_str() == "c1" && name == "f" && arguments == "{\"a\":1}"
    )));
    assert!(matches!(
        chunks.last(),
        Some(StreamChunk::Finish {
            reason: seekdeep_llm::FinishReason::ToolCalls,
            ..
        })
    ));
}

#[tokio::test]
async fn refuses_missing_key_before_opening_network_stream() {
    let adapter = adapter("http://127.0.0.1:9", None, json!({}));
    let chunks = collect(&adapter, request()).await.unwrap();
    let value = serde_json::to_value(chunks.last().unwrap()).unwrap();
    assert_eq!(value["reason"]["kind"], json!("error"));
    assert_eq!(
        value["reason"]["failure"]["message"],
        json!("No API key for provider: deepseek")
    );
}

#[tokio::test]
async fn mistral_flavor_uses_v1_endpoint_max_tokens_and_affinity() {
    let (url, captured) = server(200, text_events(), None).await;
    let mut options = GenerateOptions::new(
        ProviderId::new("mistral"),
        ModelId::new("codestral-latest"),
        vec![],
    );
    options.max_tokens = Some(77);
    options.session_id = Some(SessionId::new("mistral-session"));
    collect(&mistral_adapter(&url), options).await.unwrap();
    let captured = captured.await.unwrap();
    assert!(captured.request.starts_with("POST /v1/chat/completions"));
    assert!(
        captured
            .request
            .to_ascii_lowercase()
            .contains("x-affinity: mistral-session")
    );
    assert_eq!(captured.body["max_tokens"], json!(77));
    assert_eq!(captured.body["prompt_cache_key"], json!("mistral-session"));
    assert!(captured.body.get("store").is_none());
}
