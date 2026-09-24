//! Anthropic Messages executor integration tests.

use std::sync::Arc;

use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use indexmap::IndexMap;
use seekdeep_llm::{
    AdapterRejection, GenerateOptions, LlmAdapter, ModelId, ProviderId, StreamChunk,
};
use seekdeep_llm_pi_ai::{
    adapter::{PiAiAdapter, PiAiAdapterOptions, PiApiKeyResolver, PiProfileSource, PiResolvedAuth},
    anthropic_messages::AnthropicMessagesExecutor,
    catalog::builtin_catalog,
    config::{ResolvedPiProviderProfile, resolve_profiles},
};
use serde_json::{Value, json};
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
        Ok(PiResolvedAuth::api_key(Some("anthropic-key".to_owned())))
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
                    format!(
                        "event: {}\ndata: {}\n\n",
                        event["type"].as_str().unwrap(),
                        serde_json::to_string(&event).unwrap()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    (format!("http://{address}"), rx)
}

fn adapter(base_url: &str, reasoning: Option<&str>) -> PiAiAdapter {
    let mut profile = json!({"apiKeyEnv":"ANTHROPIC_KEY","baseURL":base_url,"models":[{"id":"claude-sonnet-4-5"}]});
    if let Some(reasoning) = reasoning {
        profile["reasoning"] = json!(reasoning);
    }
    let raw = json!({"anthropic":profile});
    let profiles = Arc::new(resolve_profiles(Some(&raw), builtin_catalog()).unwrap());
    PiAiAdapter::new(PiAiAdapterOptions {
        profiles: Arc::new(Profiles(profiles)),
        api_keys: Arc::new(Key),
        executor: Arc::new(AnthropicMessagesExecutor::new(reqwest::Client::new())),
        attachments: None,
    })
}
fn request() -> GenerateOptions {
    GenerateOptions::new(
        ProviderId::new("anthropic"),
        ModelId::new("claude-sonnet-4-5"),
        vec![],
    )
}
async fn collect(adapter: &PiAiAdapter) -> anyhow::Result<Vec<StreamChunk>> {
    adapter
        .stream(request())
        .map(|result| result.map_err(AdapterRejection::into_anyhow))
        .try_collect()
        .await
}

fn text_events(include_stop: bool) -> Vec<Value> {
    let mut events = vec![
        json!({"type":"message_start","message":{"id":"msg_1","usage":{"input_tokens":3,"output_tokens":0,"cache_read_input_tokens":1,"cache_creation_input_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hello"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}),
    ];
    if include_stop {
        events.push(json!({"type":"message_stop"}));
    }
    events
}

#[tokio::test]
async fn translates_text_usage_stop_and_request_headers() {
    let (url, captured) = server(text_events(true)).await;
    let chunks = collect(&adapter(&url, None)).await.unwrap();
    assert_eq!(
        serde_json::to_value(chunks).unwrap(),
        json!([
            {"type":"block-start","index":0,"blockType":"text"},
            {"type":"text-delta","index":0,"text":"hello"},
            {"type":"block-end","index":0,"block":{"type":"text","text":"hello"}},
            {"type":"usage","usage":{"inputTokens":3,"outputTokens":1,"cacheReadTokens":1}},
            {"type":"finish","reason":{"kind":"stop"},"replayState":{"kind":"pi-ai","version":1,"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","responseId":"msg_1","stopReason":"stop","blocks":[{"type":"text"}]}}
        ])
    );
    let captured = captured.await.unwrap();
    assert!(captured.request.starts_with("POST /v1/messages"));
    assert!(
        captured
            .request
            .to_ascii_lowercase()
            .contains("x-api-key: anthropic-key")
    );
    assert_eq!(captured.body["thinking"], json!({"type":"disabled"}));
}

#[tokio::test]
async fn translates_thinking_signature_and_fragmented_tool_input() {
    let events = vec![
        json!({"type":"message_start","message":{"id":"msg_2","usage":{"input_tokens":1,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hmm"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tool_1","name":"lookup","input":{}}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"a\":1}"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":2}}),
        json!({"type":"message_stop"}),
    ];
    let (url, _captured) = server(events).await;
    let chunks = collect(&adapter(&url, Some("high"))).await.unwrap();
    assert!(
        chunks.iter().any(
            |chunk| matches!(chunk, StreamChunk::ReasoningDelta { text, .. } if text == "hmm")
        )
    );
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        StreamChunk::BlockEnd { block: seekdeep_llm::ContentBlock::ToolCall { id, name, arguments }, .. }
            if id.as_str() == "tool_1" && name == "lookup" && arguments == "{\"a\":1}"
    )));
    let replay = serde_json::to_value(chunks.last().unwrap()).unwrap();
    assert_eq!(
        replay["replayState"]["blocks"][0]["thinkingSignature"],
        json!("sig")
    );
    assert_eq!(replay["reason"]["kind"], json!("tool-calls"));
}

#[tokio::test]
async fn missing_message_stop_is_transport_failure() {
    let (url, _captured) = server(text_events(false)).await;
    let chunks = collect(&adapter(&url, None)).await.unwrap();
    let value = serde_json::to_value(chunks.last().unwrap()).unwrap();
    assert_eq!(value["reason"]["failure"]["code"], json!("TRANSPORT"));
    assert!(
        value["reason"]["failure"]["message"]
            .as_str()
            .unwrap()
            .contains("before message_stop")
    );
}
