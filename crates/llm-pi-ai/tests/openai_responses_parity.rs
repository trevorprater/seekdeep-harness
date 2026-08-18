//! `OpenAI` Responses executor integration tests.

use std::sync::Arc;

use async_trait::async_trait;
use futures::{StreamExt, TryStreamExt};
use indexmap::IndexMap;
use seekdeep_llm::{
    AdapterRejection, GenerateOptions, LlmAdapter, ModelId, ProviderId, SessionId, StreamChunk,
};
use seekdeep_llm_pi_ai::{
    adapter::{PiAiAdapter, PiAiAdapterOptions, PiApiKeyResolver, PiProfileSource, PiResolvedAuth},
    catalog::builtin_catalog,
    config::{ResolvedPiProviderProfile, resolve_profiles},
    openai_responses::OpenAiResponsesExecutor,
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
struct StaticKey;
#[async_trait]
impl PiApiKeyResolver for StaticKey {
    async fn resolve(
        &self,
        _: &ProviderId,
        _: &ResolvedPiProviderProfile,
    ) -> anyhow::Result<PiResolvedAuth> {
        Ok(PiResolvedAuth::api_key(Some("test-key".to_owned())))
    }
}

struct Captured {
    request: String,
    body: Value,
}

async fn server(status: u16, events: Vec<Value>) -> (String, oneshot::Receiver<Captured>) {
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
        let headers = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
        let length = headers
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
        let _ = tx.send(Captured {
            request: headers,
            body,
        });
        if status != 200 {
            let body = format!(r#"{{"error":{{"message":"provider {status}"}}}}"#);
            socket.write_all(format!(
                "HTTP/1.1 {status} Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()
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
    let raw =
        json!({"openai":{"apiKeyEnv":"OPENAI_KEY","baseURL":base_url,"models":[{"id":"gpt-4.1"}]}});
    let profiles = Arc::new(resolve_profiles(Some(&raw), builtin_catalog()).unwrap());
    PiAiAdapter::new(PiAiAdapterOptions {
        profiles: Arc::new(StaticProfiles(profiles)),
        api_keys: Arc::new(StaticKey),
        executor: Arc::new(OpenAiResponsesExecutor::new(reqwest::Client::new())),
        attachments: None,
    })
}

fn azure_adapter(base_url: &str) -> PiAiAdapter {
    let raw = json!({"azure-openai-responses":{
        "apiKeyEnv":"AZURE_KEY","baseURL":base_url,"models":[{"id":"gpt-4"}]
    }});
    let profiles = Arc::new(resolve_profiles(Some(&raw), builtin_catalog()).unwrap());
    PiAiAdapter::new(PiAiAdapterOptions {
        profiles: Arc::new(StaticProfiles(profiles)),
        api_keys: Arc::new(StaticKey),
        executor: Arc::new(OpenAiResponsesExecutor::new_azure(reqwest::Client::new())),
        attachments: None,
    })
}

fn request() -> GenerateOptions {
    GenerateOptions::new(ProviderId::new("openai"), ModelId::new("gpt-4.1"), vec![])
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

fn completed(output: &[Value]) -> Value {
    json!({
        "id":"resp_fixture","status":"completed","model":"gpt-4.1","output":output,
        "usage":{"input_tokens":3,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":4}
    })
}

fn text_events() -> Vec<Value> {
    let part = json!({"type":"output_text","annotations":[],"text":"hello"});
    let message = json!({"id":"msg_fixture","type":"message","status":"completed","role":"assistant","content":[part.clone()]});
    vec![
        json!({"type":"response.created","response":{"id":"resp_fixture","status":"in_progress"}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{"id":"msg_fixture","type":"message","status":"in_progress","role":"assistant","content":[]}}),
        json!({"type":"response.output_text.delta","output_index":0,"delta":"hello"}),
        json!({"type":"response.output_item.done","output_index":0,"item":message.clone()}),
        json!({"type":"response.completed","response":completed(&[message])}),
    ]
}

#[tokio::test]
async fn translates_text_usage_and_native_message_signature() {
    let (url, captured) = server(200, text_events()).await;
    let chunks = collect(&adapter(&url), request()).await.unwrap();
    assert_eq!(
        serde_json::to_value(chunks).unwrap(),
        json!([
            {"type":"block-start","index":0,"blockType":"text"},
            {"type":"text-delta","index":0,"text":"hello"},
            {"type":"block-end","index":0,"block":{"type":"text","text":"hello"}},
            {"type":"usage","usage":{"inputTokens":3,"outputTokens":1}},
            {"type":"finish","reason":{"kind":"stop"},"replayState":{"kind":"pi-ai","version":1,"api":"openai-responses","provider":"openai","model":"gpt-4.1","responseId":"resp_fixture","stopReason":"stop","blocks":[{"type":"text","textSignature":"{\"v\":1,\"id\":\"msg_fixture\"}"}]}}
        ])
    );
    assert!(
        captured
            .await
            .unwrap()
            .request
            .starts_with("POST /responses")
    );
}

#[tokio::test]
async fn request_clamps_output_tokens_and_sets_cache_session_fields() {
    let (url, captured) = server(200, text_events()).await;
    let mut options = request();
    options.max_tokens = Some(1);
    options.temperature = Some(0.3);
    options.session_id = Some(SessionId::new("session-for-responses"));
    collect(&adapter(&url), options).await.unwrap();
    let captured = captured.await.unwrap();
    assert_eq!(captured.body["max_output_tokens"], json!(16));
    assert_eq!(captured.body["temperature"], json!(0.3));
    assert_eq!(
        captured.body["prompt_cache_key"],
        json!("session-for-responses")
    );
    assert_eq!(captured.body["store"], json!(false));
    let headers = captured.request.to_ascii_lowercase();
    assert!(headers.contains("session_id: session-for-responses"));
    assert!(headers.contains("x-client-request-id: session-for-responses"));
}

#[tokio::test]
async fn translates_function_call_identity_arguments_and_tool_stop() {
    let added = json!({"type":"function_call","id":"fc_1","call_id":"call_1","name":"lookup","arguments":""});
    let done = json!({"type":"function_call","id":"fc_1","call_id":"call_1","name":"lookup","arguments":"{\"a\":1}"});
    let events = vec![
        json!({"type":"response.output_item.added","output_index":0,"item":added}),
        json!({"type":"response.function_call_arguments.delta","output_index":0,"delta":"{\"a\""}),
        json!({"type":"response.function_call_arguments.done","output_index":0,"arguments":"{\"a\":1}"}),
        json!({"type":"response.output_item.done","output_index":0,"item":done.clone()}),
        json!({"type":"response.completed","response":completed(&[done])}),
    ];
    let (url, _captured) = server(200, events).await;
    let chunks = collect(&adapter(&url), request()).await.unwrap();
    assert!(chunks.iter().any(|chunk| matches!(
        chunk,
        StreamChunk::BlockEnd { block: seekdeep_llm::ContentBlock::ToolCall { id, name, arguments }, .. }
            if id.as_str() == "call_1|fc_1" && name == "lookup" && arguments == "{\"a\":1}"
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
async fn http_and_missing_terminal_failures_become_error_finishes() {
    let (url, _captured) = server(401, vec![]).await;
    let chunks = collect(&adapter(&url), request()).await.unwrap();
    let value = serde_json::to_value(chunks.last().unwrap()).unwrap();
    assert_eq!(value["reason"]["failure"]["code"], json!("AUTH"));

    let (url, _captured) = server(
        200,
        vec![json!({"type":"response.created","response":{"id":"r"}})],
    )
    .await;
    let chunks = collect(&adapter(&url), request()).await.unwrap();
    let value = serde_json::to_value(chunks.last().unwrap()).unwrap();
    assert_eq!(value["reason"]["failure"]["code"], json!("TRANSPORT"));
}

#[tokio::test]
async fn azure_flavor_uses_api_key_header_versioned_endpoint_and_deployment_model() {
    let (url, captured) = server(200, text_events()).await;
    let mut options = GenerateOptions::new(
        ProviderId::new("azure-openai-responses"),
        ModelId::new("gpt-4"),
        vec![],
    );
    options.max_tokens = Some(1);
    collect(&azure_adapter(&url), options).await.unwrap();
    let captured = captured.await.unwrap();
    assert!(
        captured
            .request
            .starts_with("POST /responses?api-version=v1")
    );
    let headers = captured.request.to_ascii_lowercase();
    assert!(headers.contains("api-key: test-key"));
    assert!(!headers.contains("authorization:"));
    assert_eq!(captured.body["model"], json!("gpt-4"));
    assert_eq!(captured.body["max_output_tokens"], json!(16));
}
