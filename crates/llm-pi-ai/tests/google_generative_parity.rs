//! Google Generative AI executor integration tests.

use std::sync::Arc;

use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use indexmap::IndexMap;
use seekdeep_llm::{
    AdapterRejection, ContentBlock, GenerateOptions, LlmAdapter, Message, MessageRole,
    MessageSource, ModelId, ProviderId, StreamChunk, ToolSchema,
};
use seekdeep_llm_pi_ai::{
    adapter::{PiAiAdapter, PiAiAdapterOptions, PiApiKeyResolver, PiProfileSource, PiResolvedAuth},
    catalog::builtin_catalog,
    config::{ResolvedPiProviderProfile, resolve_profiles},
    google_generative::GoogleGenerativeExecutor,
};
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
};

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
        Ok(PiResolvedAuth::api_key(Some("google-key".to_owned())))
    }
}
struct Captured {
    request: String,
    body: Value,
}

async fn server(events: Vec<Value>) -> (String, oneshot::Receiver<Captured>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut buffer = [0_u8; 4096];
            let read = socket.read(&mut buffer).await.unwrap();
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let request = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
        let length = request
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().ok())
                    .flatten()
            })
            .unwrap_or(0_usize);
        while bytes.len() - header_end < length {
            let mut buffer = vec![0_u8; length - (bytes.len() - header_end)];
            let read = socket.read(&mut buffer).await.unwrap();
            bytes.extend_from_slice(&buffer[..read]);
        }
        let body = serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
        let _ = tx.send(Captured { request, body });
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        for event in events {
            socket
                .write_all(
                    format!("data: {}\n\n", serde_json::to_string(&event).unwrap()).as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    (format!("http://{address}"), rx)
}

fn adapter(base_url: &str) -> PiAiAdapter {
    let raw = json!({"google":{"apiKeyEnv":"GOOGLE_KEY","baseURL":base_url,"models":[{"id":"gemini-2.5-flash"}]}});
    let profiles = Arc::new(resolve_profiles(Some(&raw), builtin_catalog()).unwrap());
    PiAiAdapter::new(PiAiAdapterOptions {
        profiles: Arc::new(Profiles(profiles)),
        api_keys: Arc::new(Key),
        executor: Arc::new(GoogleGenerativeExecutor::new(reqwest::Client::new())),
        attachments: None,
    })
}

fn vertex_adapter(base_url: &str) -> PiAiAdapter {
    let raw = json!({"google-vertex":{
        "apiKeyEnv":"VERTEX_KEY","baseURL":base_url,"models":[{"id":"gemini-2.5-flash"}]
    }});
    let profiles = Arc::new(resolve_profiles(Some(&raw), builtin_catalog()).unwrap());
    PiAiAdapter::new(PiAiAdapterOptions {
        profiles: Arc::new(Profiles(profiles)),
        api_keys: Arc::new(Key),
        executor: Arc::new(GoogleGenerativeExecutor::new_vertex(reqwest::Client::new())),
        attachments: None,
    })
}
fn request_for(provider: &str) -> GenerateOptions {
    let mut options = GenerateOptions::new(
        ProviderId::new(provider),
        ModelId::new("gemini-2.5-flash"),
        vec![Message::new(
            MessageRole::User,
            vec![ContentBlock::Text {
                text: "hi".to_owned(),
            }],
            MessageSource::plugin("test"),
        )],
    );
    options.system = Some("system".to_owned());
    options.tools = Some(vec![ToolSchema {
        name: "lookup".to_owned(),
        description: "Lookup".to_owned(),
        parameters: Map::from_iter([("type".to_owned(), json!("object"))]),
    }]);
    options
}
async fn collect(adapter: &PiAiAdapter) -> anyhow::Result<Vec<StreamChunk>> {
    adapter
        .stream(request_for("google"))
        .map(|result| result.map_err(AdapterRejection::into_anyhow))
        .try_collect()
        .await
}

#[tokio::test]
async fn translates_text_usage_finish_and_public_api_request() {
    let events = vec![json!({
        "responseId":"resp_google",
        "candidates":[{"content":{"parts":[{"text":"hello"}]},"finishReason":"STOP"}],
        "usageMetadata":{"promptTokenCount":4,"cachedContentTokenCount":1,"candidatesTokenCount":2,"thoughtsTokenCount":1,"totalTokenCount":7}
    })];
    let (url, captured) = server(events).await;
    let chunks = collect(&adapter(&url)).await.unwrap();
    assert_eq!(
        serde_json::to_value(chunks).unwrap(),
        json!([
            {"type":"block-start","index":0,"blockType":"text"},
            {"type":"text-delta","index":0,"text":"hello"},
            {"type":"block-end","index":0,"block":{"type":"text","text":"hello"}},
            {"type":"usage","usage":{"inputTokens":3,"outputTokens":3,"cacheReadTokens":1}},
            {"type":"finish","reason":{"kind":"stop"},"replayState":{"kind":"pi-ai","version":1,"api":"google-generative-ai","provider":"google","model":"gemini-2.5-flash","responseId":"resp_google","stopReason":"stop","blocks":[{"type":"text"}]}}
        ])
    );
    let captured = captured.await.unwrap();
    assert!(
        captured
            .request
            .starts_with("POST /models/gemini-2.5-flash:streamGenerateContent?alt=sse")
    );
    assert!(
        captured
            .request
            .to_ascii_lowercase()
            .contains("x-goog-api-key: google-key")
    );
    assert_eq!(
        captured.body["generationConfig"]["thinkingConfig"],
        json!({"thinkingBudget":0})
    );
    assert_eq!(
        captured.body["systemInstruction"],
        json!({"parts":[{"text":"system"}],"role":"user"})
    );
    assert_eq!(
        captured.body["tools"],
        json!([{"functionDeclarations":[{
            "name":"lookup","description":"Lookup","parametersJsonSchema":{"type":"object"}
        }]}])
    );
    assert!(captured.body["generationConfig"].get("tools").is_none());
}

#[tokio::test]
async fn translates_thought_signature_and_function_call() {
    let signature = "YWJjZA==";
    let events = vec![json!({
        "candidates":[{"content":{"parts":[
            {"text":"hmm","thought":true,"thoughtSignature":signature},
            {"functionCall":{"id":"call_1","name":"lookup","args":{"a":1}},"thoughtSignature":signature}
        ]},"finishReason":"STOP"}]
    })];
    let (url, _captured) = server(events).await;
    let chunks = collect(&adapter(&url)).await.unwrap();
    assert!(
        chunks.iter().any(
            |chunk| matches!(chunk, StreamChunk::ReasoningDelta { text, .. } if text == "hmm")
        )
    );
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        StreamChunk::BlockEnd { block: seekdeep_llm::ContentBlock::ToolCall { name, arguments, .. }, .. }
            if name == "lookup" && arguments == "{\"a\":1}"
    )));
    let finish = serde_json::to_value(chunks.last().unwrap()).unwrap();
    assert_eq!(finish["reason"]["kind"], json!("tool-calls"));
    assert_eq!(
        finish["replayState"]["blocks"][0]["thinkingSignature"],
        json!(signature)
    );
}

#[tokio::test]
async fn vertex_api_key_flavor_uses_global_collection_without_project_location() {
    let events = vec![json!({
        "candidates":[{"content":{"parts":[{"text":"vertex"}]},"finishReason":"STOP"}]
    })];
    let (url, captured) = server(events).await;
    let options = request_for("google-vertex");
    let chunks: Vec<StreamChunk> = vertex_adapter(&url)
        .stream(options)
        .map(|result| result.map_err(AdapterRejection::into_anyhow))
        .try_collect()
        .await
        .unwrap();
    assert!(matches!(
        chunks.last(),
        Some(StreamChunk::Finish {
            reason: seekdeep_llm::FinishReason::Stop,
            ..
        })
    ));
    let captured = captured.await.unwrap();
    assert!(captured.request.starts_with(
        "POST /v1/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
    ));
    assert!(
        captured
            .request
            .to_ascii_lowercase()
            .contains("x-goog-api-key: google-key")
    );
}

#[tokio::test]
async fn vertex_custom_base_with_api_version_does_not_duplicate_version_segment() {
    let events = vec![json!({
        "candidates":[{"content":{"parts":[{"text":"vertex"}]},"finishReason":"STOP"}]
    })];
    let (url, captured) = server(events).await;
    let chunks: Vec<StreamChunk> = vertex_adapter(&format!("{url}/v1"))
        .stream(request_for("google-vertex"))
        .map(|result| result.map_err(AdapterRejection::into_anyhow))
        .try_collect()
        .await
        .unwrap();
    assert!(matches!(
        chunks.last(),
        Some(StreamChunk::Finish {
            reason: seekdeep_llm::FinishReason::Stop,
            ..
        })
    ));
    let captured = captured.await.unwrap();
    assert!(captured.request.starts_with(
        "POST /v1/publishers/google/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
    ));
    assert!(!captured.request.contains("/v1/v1/"));
}
