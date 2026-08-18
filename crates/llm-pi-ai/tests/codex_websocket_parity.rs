//! Codex Responses WebSocket transport and pre-stream SSE fallback tests.

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures::{SinkExt as _, StreamExt as _, TryStreamExt as _};
use indexmap::IndexMap;
use seekdeep_cordis::{Context, Fiber};
use seekdeep_core::session_store::{CreateSessionOptions, SessionStore};
use seekdeep_llm::{
    AdapterRejection, ContentBlock, GenerateOptions, LlmAdapter, Message, MessageRole,
    MessageSource, ModelId, ProviderId, SessionId, StreamChunk,
};
use seekdeep_llm_pi_ai::{
    adapter::{PiAiAdapter, PiAiAdapterOptions, PiApiKeyResolver, PiProfileSource, PiResolvedAuth},
    catalog::builtin_catalog,
    config::{ResolvedPiProviderProfile, resolve_profiles},
    openai_responses::{
        OpenAiResponsesExecutor, close_openai_codex_websocket_sessions,
        get_openai_codex_websocket_debug_stats, reset_openai_codex_websocket_debug_stats,
    },
    plugin::plugin,
};
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSource, SEEKDEEP_LAUNCH_ENVIRONMENT,
    create_launch_environment_snapshot,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpListener,
    sync::oneshot,
};
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        Message as WsMessage,
        handshake::server::{Request, Response},
    },
};

struct Profiles(Arc<IndexMap<String, ResolvedPiProviderProfile>>);

impl PiProfileSource for Profiles {
    fn profiles(&self) -> Arc<IndexMap<String, ResolvedPiProviderProfile>> {
        self.0.clone()
    }
}

struct Key(String);

#[async_trait]
impl PiApiKeyResolver for Key {
    async fn resolve(
        &self,
        _: &ProviderId,
        _: &ResolvedPiProviderProfile,
    ) -> anyhow::Result<PiResolvedAuth> {
        Ok(PiResolvedAuth::api_key(Some(self.0.clone())))
    }
}

#[derive(Debug)]
struct CapturedWebSocket {
    path: String,
    headers: Vec<(String, String)>,
    body: Value,
}

fn jwt(account: &str) -> String {
    let encode = |value: Value| URL_SAFE_NO_PAD.encode(serde_json::to_vec(&value).unwrap());
    format!(
        "{}.{}.x",
        encode(json!({"alg":"none"})),
        encode(json!({
            "exp":4_102_444_800_u64,
            "https://api.openai.com/auth":{"chatgpt_account_id":account}
        }))
    )
}

fn response_events() -> Vec<Value> {
    let part = json!({"type":"output_text","annotations":[],"text":"hello"});
    let message = json!({
        "id":"msg_fixture","type":"message","status":"completed",
        "role":"assistant","content":[part]
    });
    let completed = json!({
        "id":"resp_fixture","status":"completed","model":"gpt-5.3-codex-spark",
        "output":[message.clone()],
        "usage":{"input_tokens":3,"input_tokens_details":{"cached_tokens":0},
            "output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":4}
    });
    vec![
        json!({"type":"response.created","response":{"id":"resp_fixture","status":"in_progress"}}),
        json!({"type":"response.output_item.added","output_index":0,"item":{
            "id":"msg_fixture","type":"message","status":"in_progress","role":"assistant","content":[]
        }}),
        json!({"type":"response.output_text.delta","output_index":0,"delta":"hello"}),
        json!({"type":"response.output_item.done","output_index":0,"item":message}),
        json!({"type":"response.completed","response":completed}),
    ]
}

async fn websocket_server() -> (String, oneshot::Receiver<CapturedWebSocket>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (captured_tx, captured_rx) = oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (headers_tx, headers_rx) = oneshot::channel();
        let mut headers_tx = Some(headers_tx);
        let mut socket = accept_hdr_async(stream, move |request: &Request, response: Response| {
            let headers = request
                .headers()
                .iter()
                .map(|(name, value)| {
                    (
                        name.as_str().to_ascii_lowercase(),
                        value.to_str().unwrap().to_owned(),
                    )
                })
                .collect();
            let _ = headers_tx
                .take()
                .unwrap()
                .send((request.uri().path().to_owned(), headers));
            Ok(response)
        })
        .await
        .unwrap();
        let (path, headers) = headers_rx.await.unwrap();
        let body = match socket.next().await.unwrap().unwrap() {
            WsMessage::Text(text) => serde_json::from_str(&text).unwrap(),
            other => panic!("expected text request, got {other:?}"),
        };
        let _ = captured_tx.send(CapturedWebSocket {
            path,
            headers,
            body,
        });
        for event in response_events() {
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&event).unwrap().into(),
                ))
                .await
                .unwrap();
        }
    });
    (format!("http://{address}"), captured_rx)
}

fn adapter(base_url: &str, transport: &str) -> PiAiAdapter {
    adapter_with_timeout(base_url, transport, None)
}

fn adapter_with_timeout(
    base_url: &str,
    transport: &str,
    websocket_connect_timeout_ms: Option<u64>,
) -> PiAiAdapter {
    let mut profile = json!({
        "baseURL":base_url,"transport":transport,
        "models":[{"id":"gpt-5.3-codex-spark"}]
    });
    if let Some(timeout) = websocket_connect_timeout_ms {
        profile["websocketConnectTimeoutMs"] = json!(timeout);
    }
    let raw = json!({"openai-codex":profile});
    let profiles = Arc::new(resolve_profiles(Some(&raw), builtin_catalog()).unwrap());
    PiAiAdapter::new(PiAiAdapterOptions {
        profiles: Arc::new(Profiles(profiles)),
        api_keys: Arc::new(Key(jwt("account-one"))),
        executor: Arc::new(OpenAiResponsesExecutor::new_codex(reqwest::Client::new())),
        attachments: None,
    })
}

fn request() -> GenerateOptions {
    request_for_session("session-one")
}

fn request_for_session(session: &str) -> GenerateOptions {
    let mut request = GenerateOptions::new(
        ProviderId::new("openai-codex"),
        ModelId::new("gpt-5.3-codex-spark"),
        vec![],
    );
    request.session_id = Some(SessionId::new(session));
    request
}

async fn collect(adapter: &PiAiAdapter) -> Vec<StreamChunk> {
    collect_request(adapter, request()).await
}

async fn collect_request(adapter: &PiAiAdapter, request: GenerateOptions) -> Vec<StreamChunk> {
    adapter
        .stream(request)
        .map(|result| result.map_err(AdapterRejection::into_anyhow))
        .try_collect()
        .await
        .unwrap()
}

async fn websocket_cache_server() -> (String, oneshot::Receiver<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (captured_tx, captured_rx) = oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_hdr_async(stream, |_: &Request, response: Response| Ok(response))
            .await
            .unwrap();
        let mut bodies = Vec::new();
        for turn in 1..=2 {
            let body = match socket.next().await.unwrap().unwrap() {
                WsMessage::Text(text) => serde_json::from_str::<Value>(&text).unwrap(),
                other => panic!("expected text request, got {other:?}"),
            };
            bodies.push(body);
            let response_id = format!("resp_{turn}");
            let message_id = format!("msg_{turn}");
            let text = format!("answer-{turn}");
            let part = json!({"type":"output_text","annotations":[],"text":text});
            let message = json!({
                "id":message_id,"type":"message","status":"completed",
                "role":"assistant","content":[part]
            });
            let completed = json!({
                "id":response_id,"status":"completed","model":"gpt-5.3-codex-spark",
                "output":[message.clone()],
                "usage":{"input_tokens":1,"input_tokens_details":{"cached_tokens":0},
                    "output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":2}
            });
            for event in [
                json!({"type":"response.created","response":{"id":response_id,"status":"in_progress"}}),
                json!({"type":"response.output_item.added","output_index":0,"item":{
                    "id":message_id,"type":"message","status":"in_progress","role":"assistant","content":[]
                }}),
                json!({"type":"response.output_text.delta","output_index":0,"delta":text}),
                json!({"type":"response.output_item.done","output_index":0,"item":message}),
                json!({"type":"response.completed","response":completed}),
            ] {
                socket
                    .send(WsMessage::Text(
                        serde_json::to_string(&event).unwrap().into(),
                    ))
                    .await
                    .unwrap();
            }
        }
        let _ = captured_tx.send(bodies);
    });
    (format!("http://{address}"), captured_rx)
}

fn followup_request(first: &[StreamChunk], assistant_text: &str, session: &str) -> GenerateOptions {
    let replay_state = match first.last().unwrap() {
        StreamChunk::Finish {
            replay_state: Some(replay_state),
            ..
        } => replay_state.clone(),
        other => panic!("expected replay finish, got {other:?}"),
    };
    let mut source = MessageSource::model("openai-codex", "gpt-5.3-codex-spark");
    source.fields.insert("replayState".to_owned(), replay_state);
    let assistant = Message::new(
        MessageRole::Assistant,
        vec![ContentBlock::Text {
            text: assistant_text.to_owned(),
        }],
        source,
    );
    let user = Message::new(
        MessageRole::User,
        vec![ContentBlock::Text {
            text: "next".to_owned(),
        }],
        MessageSource::plugin("test"),
    );
    let mut request = request_for_session(session);
    request.messages = vec![assistant, user];
    request
}

#[tokio::test]
async fn websocket_cached_reuses_connection_and_sends_only_continuation_delta() {
    reset_openai_codex_websocket_debug_stats(Some("cached-session"));
    let (url, captured) = websocket_cache_server().await;
    let adapter = adapter(&url, "websocket-cached");
    let first = collect_request(&adapter, request_for_session("cached-session")).await;
    let second_request = followup_request(&first, "answer-1", "cached-session");
    let second = collect_request(&adapter, second_request).await;
    assert!(matches!(
        second.last(),
        Some(StreamChunk::Finish {
            reason: seekdeep_llm::FinishReason::Stop,
            ..
        })
    ));
    let bodies = tokio::time::timeout(std::time::Duration::from_secs(2), captured)
        .await
        .unwrap()
        .unwrap();
    assert!(bodies[0].get("previous_response_id").is_none());
    assert_eq!(bodies[1]["previous_response_id"], json!("resp_1"));
    assert_eq!(bodies[1]["input"].as_array().unwrap().len(), 1);
    assert_eq!(bodies[1]["input"][0]["role"], json!("user"));
    let stats = get_openai_codex_websocket_debug_stats("cached-session").unwrap();
    assert_eq!(
        serde_json::to_value(stats).unwrap(),
        json!({
            "requests":2,"connectionsCreated":1,"connectionsReused":1,
            "cachedContextRequests":2,"storeTrueRequests":0,
            "fullContextRequests":1,"deltaRequests":1,"lastInputItems":1,
            "lastDeltaInputItems":1,"lastPreviousResponseId":"resp_1",
            "websocketFailures":0,"sseFallbacks":0,"websocketFallbackActive":false
        })
    );
    reset_openai_codex_websocket_debug_stats(Some("cached-session"));
    assert!(get_openai_codex_websocket_debug_stats("cached-session").is_none());
}

fn header<'a>(captured: &'a CapturedWebSocket, name: &str) -> Option<&'a str> {
    captured
        .headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[tokio::test]
async fn websocket_transport_sends_response_create_and_translates_native_events() {
    let (url, captured) = websocket_server().await;
    let chunks = collect(&adapter(&url, "websocket")).await;
    assert!(matches!(
        chunks.last(),
        Some(StreamChunk::Finish {
            reason: seekdeep_llm::FinishReason::Stop,
            ..
        })
    ));
    let captured = captured.await.unwrap();
    assert_eq!(captured.path, "/codex/responses");
    assert_eq!(
        header(&captured, "openai-beta"),
        Some("responses_websockets=2026-02-06")
    );
    assert_eq!(header(&captured, "session-id"), Some("session-one"));
    assert_eq!(
        header(&captured, "x-client-request-id"),
        Some("session-one")
    );
    assert_eq!(header(&captured, "chatgpt-account-id"), Some("account-one"));
    assert_eq!(captured.body["type"], json!("response.create"));
    assert_eq!(captured.body["stream"], json!(true));
    assert_eq!(captured.body["store"], json!(false));
}

async fn read_head(socket: &mut tokio::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    loop {
        let mut buffer = [0_u8; 4096];
        let read = socket.read(&mut buffer).await.unwrap();
        if read == 0 {
            return String::from_utf8_lossy(&bytes).into_owned();
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            return String::from_utf8_lossy(&bytes).into_owned();
        }
    }
}

async fn fallback_server() -> (String, oneshot::Receiver<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut websocket, _) = listener.accept().await.unwrap();
        let first = read_head(&mut websocket).await;
        websocket
            .write_all(
                b"HTTP/1.1 503 Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        let (mut sse, _) = listener.accept().await.unwrap();
        let second = read_head(&mut sse).await;
        let body = response_events()
            .into_iter()
            .fold(String::new(), |mut output, event| {
                use std::fmt::Write as _;
                writeln!(output, "data: {event}\n").unwrap();
                output
            });
        sse.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let _ = tx.send(vec![first, second]);
    });
    (format!("http://{address}"), rx)
}

async fn connection_limit_server() -> (String, oneshot::Receiver<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let mut requests = Vec::new();
        for _ in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            let (head_tx, head_rx) = oneshot::channel();
            let mut head_tx = Some(head_tx);
            let mut socket =
                accept_hdr_async(stream, move |request: &Request, response: Response| {
                    let _ = head_tx
                        .take()
                        .unwrap()
                        .send(format!("GET {} HTTP/1.1", request.uri().path()));
                    Ok(response)
                })
                .await
                .unwrap();
            requests.push(head_rx.await.unwrap());
            let _ = socket.next().await.unwrap().unwrap();
            socket
                .send(WsMessage::Text(
                    json!({
                        "type":"error",
                        "code":"websocket_connection_limit_reached",
                        "message":"too many sockets"
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        }
        let (mut sse, _) = listener.accept().await.unwrap();
        let head = read_head(&mut sse).await;
        requests.push(head.lines().next().unwrap_or_default().to_owned());
        let body = response_events()
            .into_iter()
            .fold(String::new(), |mut output, event| {
                use std::fmt::Write as _;
                writeln!(output, "data: {event}\n").unwrap();
                output
            });
        sse.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let _ = tx.send(requests);
    });
    (format!("http://{address}"), rx)
}

#[tokio::test]
async fn websocket_connection_limit_retries_once_then_falls_back_to_sse() {
    let (url, captured) = connection_limit_server().await;
    let adapter = adapter(&url, "auto");
    let chunks = collect_request(&adapter, request_for_session("connection-limit")).await;
    assert!(matches!(
        chunks.last(),
        Some(StreamChunk::Finish {
            reason: seekdeep_llm::FinishReason::Stop,
            ..
        })
    ));
    let requests = captured.await.unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].starts_with("GET /codex/responses"));
    assert!(requests[1].starts_with("GET /codex/responses"));
    assert!(requests[2].starts_with("POST /codex/responses"));
}

async fn missing_continuation_server() -> (String, oneshot::Receiver<Vec<Value>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_hdr_async(stream, |_: &Request, response: Response| Ok(response))
            .await
            .unwrap();
        let mut bodies = Vec::new();
        let first = match socket.next().await.unwrap().unwrap() {
            WsMessage::Text(text) => serde_json::from_str::<Value>(&text).unwrap(),
            other => panic!("expected text request, got {other:?}"),
        };
        bodies.push(first);
        for event in response_events() {
            socket
                .send(WsMessage::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        let delta = match socket.next().await.unwrap().unwrap() {
            WsMessage::Text(text) => serde_json::from_str::<Value>(&text).unwrap(),
            other => panic!("expected delta request, got {other:?}"),
        };
        bodies.push(delta);
        socket
            .send(WsMessage::Text(
                json!({
                    "type":"error","code":"previous_response_not_found",
                    "message":"missing continuation"
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        let retried = match socket.next().await.unwrap().unwrap() {
            WsMessage::Text(text) => serde_json::from_str::<Value>(&text).unwrap(),
            other => panic!("expected full retry request, got {other:?}"),
        };
        bodies.push(retried);
        for event in response_events() {
            socket
                .send(WsMessage::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        let _ = tx.send(bodies);
    });
    (format!("http://{address}"), rx)
}

async fn started_failure_server() -> (String, oneshot::Receiver<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (head_tx, head_rx) = oneshot::channel();
        let mut head_tx = Some(head_tx);
        let mut socket = accept_hdr_async(stream, move |request: &Request, response: Response| {
            let _ = head_tx
                .take()
                .unwrap()
                .send(format!("GET {} HTTP/1.1", request.uri().path()));
            Ok(response)
        })
        .await
        .unwrap();
        let first_head = head_rx.await.unwrap();
        let _ = socket.next().await.unwrap().unwrap();
        socket
            .send(WsMessage::Text(
                json!({
                    "type":"response.created",
                    "response":{"id":"started","status":"in_progress"}
                })
                .to_string()
                .into(),
            ))
            .await
            .unwrap();
        socket.close(None).await.unwrap();

        let (mut sse, _) = listener.accept().await.unwrap();
        let second_head = read_head(&mut sse).await;
        let body = response_events()
            .into_iter()
            .fold(String::new(), |mut output, event| {
                use std::fmt::Write as _;
                writeln!(output, "data: {event}\n").unwrap();
                output
            });
        sse.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let _ = tx.send(vec![
            first_head,
            second_head.lines().next().unwrap_or_default().to_owned(),
        ]);
    });
    (format!("http://{address}"), rx)
}

async fn connect_timeout_server() -> (String, oneshot::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (stalled, _) = listener.accept().await.unwrap();
        tokio::spawn(async move {
            let _stalled = stalled;
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });
        let (mut sse, _) = listener.accept().await.unwrap();
        let head = read_head(&mut sse).await;
        let body = response_events()
            .into_iter()
            .fold(String::new(), |mut output, event| {
                use std::fmt::Write as _;
                writeln!(output, "data: {event}\n").unwrap();
                output
            });
        sse.write_all(
            format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let _ = tx.send(head.lines().next().unwrap_or_default().to_owned());
    });
    (format!("http://{address}"), rx)
}

#[tokio::test]
async fn websocket_connect_timeout_before_start_falls_back_to_sse() {
    let (url, captured) = connect_timeout_server().await;
    let adapter = adapter_with_timeout(&url, "websocket", Some(20));
    let chunks = collect_request(&adapter, request_for_session("connect-timeout")).await;
    assert!(matches!(
        chunks.last(),
        Some(StreamChunk::Finish {
            reason: seekdeep_llm::FinishReason::Stop,
            ..
        })
    ));
    assert!(captured.await.unwrap().starts_with("POST /codex/responses"));
}

async fn close_observing_server() -> (String, oneshot::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (closed_tx, closed_rx) = oneshot::channel();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = accept_hdr_async(stream, |_: &Request, response: Response| Ok(response))
            .await
            .unwrap();
        let _ = socket.next().await.unwrap().unwrap();
        for event in response_events() {
            socket
                .send(WsMessage::Text(event.to_string().into()))
                .await
                .unwrap();
        }
        while let Some(message) = socket.next().await {
            match message {
                Ok(WsMessage::Close(_)) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let _ = closed_tx.send(());
    });
    (format!("http://{address}"), closed_rx)
}

#[tokio::test]
async fn disposed_harness_session_closes_its_cached_websocket() {
    let home = tempfile::tempdir().unwrap();
    let access = jwt("account-one");
    let auth_path = home.path().join("auth.json");
    tokio::fs::write(
        &auth_path,
        serde_json::to_vec(&json!({
            "auth_mode":"chatgpt",
            "tokens":{"id_token":"id","access_token":access,
                "refresh_token":"refresh","account_id":"account-one"}
        }))
        .unwrap(),
    )
    .await
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        tokio::fs::set_permissions(&auth_path, std::fs::Permissions::from_mode(0o600))
            .await
            .unwrap();
    }
    let (url, closed) = close_observing_server().await;
    let context = Context::new();
    let runtime = seekdeep_llm::LlmRuntime::install(&context).unwrap();
    let sessions = SessionStore::install(&context).unwrap();
    let environment = create_launch_environment_snapshot(&[LaunchEnvironmentLayerInput {
        source: LaunchEnvironmentSource::Process,
        path: None,
        values: BTreeMap::from([("CODEX_HOME".to_owned(), home.path().display().to_string())]),
    }]);
    context
        .provide(SEEKDEEP_LAUNCH_ENVIRONMENT, Arc::new(environment))
        .unwrap();
    let plugin_fiber = context
        .plugin(
            plugin(),
            json!({"providers":{"openai-codex":{
                "baseURL":url,"transport":"websocket-cached",
                "models":[{"id":"gpt-5.3-codex-spark"}]
            }}}),
        )
        .unwrap();
    plugin_fiber.await_settled().await.unwrap();
    let owner_fiber = Fiber::active_child("session-owner");
    let owner = context.with_fiber(owner_fiber.clone());
    let _session = sessions
        .create(
            &owner,
            Some(SessionId::new("session-one")),
            CreateSessionOptions::default(),
        )
        .unwrap();
    let chunks: Vec<StreamChunk> = runtime.stream(request()).try_collect().await.unwrap();
    assert!(matches!(
        chunks.last(),
        Some(StreamChunk::Finish {
            reason: seekdeep_llm::FinishReason::Stop,
            ..
        })
    ));
    owner_fiber.dispose().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), closed)
        .await
        .unwrap()
        .unwrap();
    plugin_fiber.dispose().await.unwrap();
}

#[tokio::test]
async fn public_close_control_releases_selected_cached_session() {
    let (url, closed) = close_observing_server().await;
    let adapter = adapter(&url, "websocket-cached");
    let chunks = collect_request(&adapter, request_for_session("debug-close")).await;
    assert!(matches!(
        chunks.last(),
        Some(StreamChunk::Finish {
            reason: seekdeep_llm::FinishReason::Stop,
            ..
        })
    ));
    close_openai_codex_websocket_sessions(Some("debug-close")).await;
    tokio::time::timeout(std::time::Duration::from_secs(2), closed)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn websocket_failure_after_start_does_not_fallback_midstream_but_pins_next_call_to_sse() {
    reset_openai_codex_websocket_debug_stats(Some("started-failure"));
    let (url, captured) = started_failure_server().await;
    let adapter = adapter(&url, "auto");
    let first = collect_request(&adapter, request_for_session("started-failure")).await;
    assert!(matches!(
        first.last(),
        Some(StreamChunk::Finish {
            reason: seekdeep_llm::FinishReason::Error { .. },
            ..
        })
    ));
    let second = collect_request(&adapter, request_for_session("started-failure")).await;
    assert!(matches!(
        second.last(),
        Some(StreamChunk::Finish {
            reason: seekdeep_llm::FinishReason::Stop,
            ..
        })
    ));
    let requests = captured.await.unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /codex/responses"));
    assert!(requests[1].starts_with("POST /codex/responses"));
    let stats = get_openai_codex_websocket_debug_stats("started-failure").unwrap();
    assert_eq!(stats.requests, 1);
    assert_eq!(stats.connections_created, 1);
    assert_eq!(stats.websocket_failures, 1);
    assert_eq!(stats.sse_fallbacks, 1);
    assert!(stats.websocket_fallback_active);
    assert!(
        stats
            .last_websocket_error
            .unwrap()
            .contains("WebSocket closed")
    );
    reset_openai_codex_websocket_debug_stats(Some("started-failure"));
}

#[tokio::test]
async fn missing_cached_continuation_retries_full_context_once_on_same_socket() {
    let (url, captured) = missing_continuation_server().await;
    let adapter = adapter(&url, "websocket-cached");
    let first = collect_request(&adapter, request_for_session("missing-continuation")).await;
    let second = collect_request(
        &adapter,
        followup_request(&first, "hello", "missing-continuation"),
    )
    .await;
    assert!(matches!(
        second.last(),
        Some(StreamChunk::Finish {
            reason: seekdeep_llm::FinishReason::Stop,
            ..
        })
    ));
    let bodies = captured.await.unwrap();
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[1]["previous_response_id"], json!("resp_fixture"));
    assert!(bodies[2].get("previous_response_id").is_none());
    assert_eq!(bodies[2]["input"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn websocket_handshake_failure_before_events_falls_back_to_sse() {
    let (url, captured) = fallback_server().await;
    let adapter = adapter(&url, "websocket");
    let chunks = collect_request(&adapter, request_for_session("handshake-fallback")).await;
    assert!(matches!(
        chunks.last(),
        Some(StreamChunk::Finish {
            reason: seekdeep_llm::FinishReason::Stop,
            ..
        })
    ));
    let requests = captured.await.unwrap();
    assert!(requests[0].starts_with("GET /codex/responses"));
    assert!(requests[1].starts_with("POST /codex/responses"));
}
