//! Local-socket mirror of the transport half of `adapter.spec.ts`.

use std::{
    collections::{BTreeMap, VecDeque},
    error::Error as _,
    fmt::Write as _,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::{FutureExt as _, TryStreamExt as _, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_anonymous_user_id::{ANONYMOUS_USER_ID_FILE_NAME, AnonymousUserId};
use seekdeep_cordis::Context;
use seekdeep_credentials::{CREDENTIALS, CredentialRef, credential_ref};
use seekdeep_credentials_local::{LocalCredentialConfig, install as install_credentials};
use seekdeep_llm::{
    AbortSignal, ContentBlock, GenerateOptions, LLM, LlmAdapter, LlmError, LlmRequestPurpose,
    LlmRuntime, Message, MessageRole, MessageSource, ProviderRequestId, StreamChunk, user_agent,
};
use seekdeep_llm_deepseek::{
    DeepSeekAdapter, DeepSeekAdapterOptions, DeepSeekConfig, DeepSeekConnectionOptions,
    adapter::http_error_code,
    install as install_deepseek, resolve_adapter_options,
    types::{WireErrorDetail, WireFunctionDelta, WireToolCallDelta},
};
use seekdeep_settings_file::{FileSettingsConfig, install as install_settings_file};
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSource, SEEKDEEP_LAUNCH_ENVIRONMENT,
    create_launch_environment_snapshot,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
};

const TEXT_EVENTS: [&str; 4] = [
    r#"{"choices":[{"delta":{"role":"assistant","content":null,"reasoning_content":""}}]}"#,
    r#"{"choices":[{"delta":{"content":"hello"}}]}"#,
    r#"{"choices":[{"delta":{"content":""},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}"#,
    "[DONE]",
];

#[derive(Clone)]
struct Frame {
    before_ms: u64,
    bytes: String,
}

enum Behavior {
    Stream(Vec<Frame>),
    Http {
        status: u16,
        body: String,
        headers: Vec<(String, String)>,
    },
}

#[derive(Clone, Debug)]
struct CapturedRequest {
    headers: BTreeMap<String, String>,
    body: Value,
}

struct MockServer {
    url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    task: tokio::task::AbortHandle,
}

impl MockServer {
    async fn start(script: Vec<Behavior>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let script = Arc::new(Mutex::new(VecDeque::from(script)));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let behavior = script.lock().pop_front();
                let captured = captured.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, behavior, captured).await;
                });
            }
        });
        let handle = task.abort_handle();
        drop(task);
        Self {
            url: format!("http://{address}"),
            requests,
            task: handle,
        }
    }

    fn requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    mut stream: TcpStream,
    behavior: Option<Behavior>,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
) -> anyhow::Result<()> {
    let request = read_request(&mut stream).await?;
    requests.lock().push(request);
    match behavior {
        Some(Behavior::Http {
            status,
            body,
            headers,
        }) => {
            let mut response = format!(
                "HTTP/1.1 {status} Test\r\ncontent-length: {}\r\nconnection: close\r\n",
                body.len()
            );
            for (name, value) in headers {
                write!(response, "{name}: {value}\r\n").unwrap();
            }
            response.push_str("\r\n");
            response.push_str(&body);
            stream.write_all(response.as_bytes()).await?;
        }
        Some(Behavior::Stream(frames)) => {
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
                )
                .await?;
            for frame in frames {
                tokio::time::sleep(Duration::from_millis(frame.before_ms)).await;
                stream.write_all(frame.bytes.as_bytes()).await?;
            }
        }
        None => {
            stream
                .write_all(
                    b"HTTP/1.1 500 Script Exhausted\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await?;
        }
    }
    Ok(())
}

async fn read_request(stream: &mut TcpStream) -> anyhow::Result<CapturedRequest> {
    let mut bytes = Vec::new();
    let boundary = loop {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0, "request closed before headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(boundary) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break boundary + 4;
        }
    };
    let head = std::str::from_utf8(&bytes[..boundary])?;
    let mut headers = BTreeMap::new();
    for line in head.split("\r\n").skip(1).filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("malformed request header"))?;
        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    while bytes.len() < boundary + length {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0, "request closed before body");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(CapturedRequest {
        headers,
        body: serde_json::from_slice(&bytes[boundary..boundary + length])?,
    })
}

fn sse_events() -> Vec<Frame> {
    TEXT_EVENTS
        .iter()
        .map(|event| Frame {
            before_ms: 0,
            bytes: format!("data: {event}\n\n"),
        })
        .collect()
}

fn request() -> GenerateOptions {
    GenerateOptions::new(
        seekdeep_llm::ProviderId::new("deepseek-official"),
        seekdeep_llm::ModelId::new("deepseek-v4-flash"),
        vec![Message::new(
            MessageRole::User,
            vec![ContentBlock::Text {
                text: "hi".to_owned(),
            }],
            MessageSource::plugin("test"),
        )],
    )
}

fn adapter_of(
    connection: DeepSeekConnectionOptions,
    key: &str,
) -> (
    DeepSeekAdapter,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let connection = Arc::new(connection);
    let option_calls = Arc::new(AtomicUsize::new(0));
    let key_calls = Arc::new(AtomicUsize::new(0));
    let user_calls = Arc::new(AtomicUsize::new(0));
    let options = {
        let calls = option_calls.clone();
        Arc::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            connection.clone()
        })
    };
    let key = key.to_owned();
    let resolve_api_key = {
        let calls = key_calls.clone();
        Arc::new(
            move |_connection: Arc<DeepSeekConnectionOptions>| -> BoxFuture<'static, anyhow::Result<String>> {
                calls.fetch_add(1, Ordering::SeqCst);
                futures::future::ready(Ok(key.clone())).boxed()
            },
        )
    };
    let resolve_user_id = {
        let calls = user_calls.clone();
        Arc::new(move || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(AnonymousUserId::new("00000000-0000-4000-8000-000000000001"))
        })
    };
    (
        DeepSeekAdapter::new(DeepSeekAdapterOptions {
            options,
            resolve_api_key,
            resolve_user_id,
            http: reqwest::Client::new(),
        }),
        option_calls,
        key_calls,
        user_calls,
    )
}

fn connection(base_url: &str) -> DeepSeekConnectionOptions {
    resolve_adapter_options(
        &DeepSeekConfig {
            base_url: Some(base_url.to_owned()),
            ..DeepSeekConfig::default()
        },
        None,
    )
    .unwrap()
}

async fn drain(
    adapter: &DeepSeekAdapter,
    request: GenerateOptions,
) -> anyhow::Result<Vec<StreamChunk>> {
    adapter
        .stream(request)
        .map_err(seekdeep_llm::AdapterRejection::into_anyhow)
        .try_collect()
        .await
}

async fn write_deepseek_settings(path: &Path, base_url: &str, model: &str) {
    tokio::fs::write(
        path,
        format!("llm-deepseek:\n  baseURL: {base_url}\n  models:\n    - id: {model}\n"),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn streams_over_http_with_exact_request_headers_and_one_snapshot() {
    let server = MockServer::start(vec![Behavior::Stream(sse_events())]).await;
    let (adapter, option_calls, key_calls, user_calls) =
        adapter_of(connection(&server.url), "test-key");
    let mut input = request();
    input.session_id = Some(seekdeep_llm::SessionId::new("child-session"));
    input.purpose = Some(LlmRequestPurpose::Compaction);
    input.max_tokens = Some(8_192);
    let chunks = drain(&adapter, input).await.unwrap();
    assert_eq!(
        chunks
            .iter()
            .map(|chunk| match chunk {
                StreamChunk::BlockStart { .. } => "block-start",
                StreamChunk::TextDelta { .. } => "text-delta",
                StreamChunk::BlockEnd { .. } => "block-end",
                StreamChunk::Usage { .. } => "usage",
                StreamChunk::Finish { .. } => "finish",
                StreamChunk::ReasoningDelta { .. } => "reasoning-delta",
                StreamChunk::ToolCallDelta { .. } => "tool-call-delta",
            })
            .collect::<Vec<_>>(),
        ["block-start", "text-delta", "block-end", "usage", "finish"]
    );
    assert_eq!(option_calls.load(Ordering::SeqCst), 1);
    assert_eq!(key_calls.load(Ordering::SeqCst), 1);
    assert_eq!(user_calls.load(Ordering::SeqCst), 1);

    let request = &server.requests()[0];
    assert_eq!(request.headers["authorization"], "Bearer test-key");
    assert_eq!(request.headers["user-agent"], user_agent());
    assert_eq!(
        request.headers["x-seekdeep-harness-user-id"],
        "00000000-0000-4000-8000-000000000001"
    );
    assert_eq!(
        request.headers["x-seekdeep-harness-session-id"],
        "child-session"
    );
    assert_eq!(request.headers["x-seekdeep-harness-compact"], "1");
    assert_eq!(request.body["model"], "deepseek-v4-flash");
    assert_eq!(request.body["max_tokens"], 8_192);
    assert_eq!(request.body["stream"], true);
    assert_eq!(
        request.body["stream_options"],
        json!({"include_usage":true})
    );
}

#[tokio::test]
async fn maps_http_status_body_retry_and_request_identity_to_llm_error() {
    let server = MockServer::start(vec![
        Behavior::Http {
            status: 429,
            body: json!({"error":{"message":"slow down"}}).to_string(),
            headers: vec![
                ("retry-after".to_owned(), "2".to_owned()),
                ("x-request-id".to_owned(), "req-429".to_owned()),
            ],
        },
        Behavior::Http {
            status: 503,
            body: json!({"error":{"message":"later"}}).to_string(),
            headers: vec![(
                "x-deepseek-request-id".to_owned(),
                "deepseek-503".to_owned(),
            )],
        },
        Behavior::Http {
            status: 502,
            body: "Bad Gateway".to_owned(),
            headers: vec![],
        },
    ])
    .await;
    let (adapter, _, _, _) = adapter_of(connection(&server.url), "k");
    let error = drain(&adapter, request()).await.unwrap_err();
    let failure = error.downcast_ref::<LlmError>().unwrap().failure();
    assert_eq!(failure.message, "slow down");
    assert_eq!(failure.code, "RATE_LIMIT");
    assert_eq!(failure.status, Some(429));
    assert_eq!(failure.provider_retry_after_ms, Some(2_000.0));
    assert_eq!(failure.request_id, Some(ProviderRequestId::new("req-429")));

    let error = drain(&adapter, request()).await.unwrap_err();
    let failure = error.downcast_ref::<LlmError>().unwrap().failure();
    assert_eq!(failure.code, "SERVER");
    assert_eq!(
        failure.request_id,
        Some(ProviderRequestId::new("deepseek-503"))
    );

    let error = drain(&adapter, request()).await.unwrap_err();
    let failure = error.downcast_ref::<LlmError>().unwrap().failure();
    assert_eq!(failure.message, "DeepSeek API error (HTTP 502)");
    assert_eq!(failure.code, "SERVER");
}

#[test]
fn http_code_classification_preserves_context_quota_and_unknown_statuses() {
    assert_eq!(http_error_code(401, None), "AUTH");
    assert_eq!(http_error_code(403, None), "AUTH");
    assert_eq!(http_error_code(429, None), "RATE_LIMIT");
    assert_eq!(http_error_code(500, None), "SERVER");
    assert_eq!(http_error_code(418, None), "HTTP_418");
    assert_eq!(
        http_error_code(
            400,
            Some(&WireErrorDetail {
                message: Some("request too large for model context".to_owned()),
                ..WireErrorDetail::default()
            })
        ),
        "CONTEXT_WINDOW_EXCEEDED"
    );
    assert_eq!(
        http_error_code(
            429,
            Some(&WireErrorDetail {
                code: Some("insufficient_quota".to_owned()),
                message: Some("account credits exhausted".to_owned()),
                ..WireErrorDetail::default()
            })
        ),
        "QUOTA"
    );
    assert_eq!(
        http_error_code(
            413,
            Some(&WireErrorDetail {
                code: Some("context_length_exceeded".to_owned()),
                ..WireErrorDetail::default()
            })
        ),
        "HTTP_413"
    );
}

#[tokio::test]
async fn connection_failure_and_caller_abort_have_distinct_codes_and_causes() {
    let (adapter, _, _, _) = adapter_of(connection("http://127.0.0.1:1"), "k");
    let error = drain(&adapter, request()).await.unwrap_err();
    let llm = error.downcast_ref::<LlmError>().unwrap();
    assert_eq!(llm.code(), "TRANSPORT");
    assert_eq!(
        llm.failure().message,
        "DeepSeek API request to http://127.0.0.1:1 failed"
    );
    assert!(llm.source().is_some());

    let signal = AbortSignal::default();
    signal.abort();
    let mut input = request();
    input.signal = Some(signal);
    let error = drain(&adapter, input).await.unwrap_err();
    assert_eq!(error.downcast_ref::<LlmError>().unwrap().code(), "ABORTED");
}

#[tokio::test]
async fn midstream_abort_and_idle_timeout_terminate_pending_body_reads() {
    let delayed = MockServer::start(vec![Behavior::Stream(vec![Frame {
        before_ms: 200,
        bytes: "data: [DONE]\n\n".to_owned(),
    }])])
    .await;
    let (adapter, _, _, _) = adapter_of(connection(&delayed.url), "k");
    let signal = AbortSignal::default();
    let mut input = request();
    input.signal = Some(signal.clone());
    let pending = tokio::spawn({
        let adapter = adapter.clone();
        async move { drain(&adapter, input).await }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    signal.abort();
    let error = pending.await.unwrap().unwrap_err();
    assert_eq!(error.downcast_ref::<LlmError>().unwrap().code(), "ABORTED");

    let idle = MockServer::start(vec![Behavior::Stream(vec![Frame {
        before_ms: 200,
        bytes: "data: [DONE]\n\n".to_owned(),
    }])])
    .await;
    let mut timed_connection = connection(&idle.url);
    timed_connection.stream_idle_timeout_ms = 25.0;
    let (adapter, _, _, _) = adapter_of(timed_connection, "k");
    let error = drain(&adapter, request()).await.unwrap_err();
    let failure = error.downcast_ref::<LlmError>().unwrap();
    assert_eq!(failure.code(), "TIMEOUT");
    assert_eq!(
        failure.failure().message,
        "DeepSeek stream idle timeout after 25ms"
    );
}

#[tokio::test]
async fn sse_comments_keep_an_idle_read_alive_without_entering_payloads() {
    let mut frames = Vec::new();
    for _ in 0..5 {
        frames.push(Frame {
            before_ms: 15,
            bytes: ": keep-alive\n\n".to_owned(),
        });
    }
    frames.extend(sse_events().into_iter().map(|mut frame| {
        frame.before_ms = 10;
        frame
    }));
    let server = MockServer::start(vec![Behavior::Stream(frames)]).await;
    let mut live = connection(&server.url);
    live.stream_idle_timeout_ms = 40.0;
    let (adapter, _, _, _) = adapter_of(live, "k");
    let chunks = drain(&adapter, request()).await.unwrap();
    assert!(matches!(chunks.last(), Some(StreamChunk::Finish { .. })));
}

#[tokio::test]
async fn real_credentials_seam_starts_keyless_then_rotates_on_the_next_request() {
    let home = tempfile::tempdir().unwrap();
    let snapshot = Arc::new(create_launch_environment_snapshot(&[
        LaunchEnvironmentLayerInput {
            source: LaunchEnvironmentSource::Process,
            path: None,
            values: BTreeMap::from([
                (
                    "SEEKDEEP_HOME".to_owned(),
                    home.path().to_string_lossy().into_owned(),
                ),
                ("DEEPSEEK_API_KEY".to_owned(), String::new()),
            ]),
        },
    ]));
    let context = Context::new();
    context
        .provide(SEEKDEEP_LAUNCH_ENVIRONMENT, snapshot)
        .unwrap();
    let runtime = LlmRuntime::install(&context).unwrap();
    let credential_fiber = install_credentials(
        &context,
        LocalCredentialConfig {
            path: Some(home.path().join(".credentials.yaml")),
            seekdeep_home: None,
            watch: false,
            debounce_ms: 0.0,
        },
    )
    .unwrap();
    credential_fiber.await_settled().await.unwrap();
    let server = MockServer::start(vec![
        Behavior::Stream(sse_events()),
        Behavior::Stream(sse_events()),
    ])
    .await;
    let provider_fiber = install_deepseek(
        &context,
        DeepSeekConfig {
            base_url: Some(server.url.clone()),
            ..DeepSeekConfig::default()
        },
    )
    .unwrap();
    provider_fiber.await_settled().await.unwrap();

    let keyless = runtime
        .stream(request())
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    let Some(StreamChunk::Finish {
        reason: seekdeep_llm::FinishReason::Error { failure },
        ..
    }) = keyless.last()
    else {
        panic!("expected keyless finish")
    };
    assert_eq!(failure.code, "MISSING_CREDENTIAL");
    assert!(!home.path().join(ANONYMOUS_USER_ID_FILE_NAME).exists());
    assert!(server.requests().is_empty());

    let credentials = context.get(CREDENTIALS).unwrap();
    let reference = credential_ref("DEEPSEEK_API_KEY").unwrap();
    credentials.set(&reference, "first-key").await.unwrap();
    runtime
        .stream(request())
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    credentials.set(&reference, "rotated-key").await.unwrap();
    runtime
        .stream(request())
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    let requests = server.requests();
    assert_eq!(requests[0].headers["authorization"], "Bearer first-key");
    assert_eq!(requests[1].headers["authorization"], "Bearer rotated-key");
    assert!(home.path().join(ANONYMOUS_USER_ID_FILE_NAME).exists());

    provider_fiber.dispose().await.unwrap();
    assert!(runtime.list_providers().is_empty());
    credential_fiber.dispose().await.unwrap();
    assert!(context.get(CREDENTIALS).is_none());
    assert!(context.get(LLM).is_some());
}

#[tokio::test]
async fn file_settings_hot_reload_changes_the_next_real_request_route_and_catalog() {
    let home = tempfile::tempdir().unwrap();
    let settings_path = home.path().join("settings.yaml");
    let snapshot = Arc::new(create_launch_environment_snapshot(&[
        LaunchEnvironmentLayerInput {
            source: LaunchEnvironmentSource::Process,
            path: None,
            values: BTreeMap::from([
                (
                    "SEEKDEEP_HOME".to_owned(),
                    home.path().to_string_lossy().into_owned(),
                ),
                ("DEEPSEEK_API_KEY".to_owned(), String::new()),
            ]),
        },
    ]));
    let first = MockServer::start(vec![Behavior::Stream(sse_events())]).await;
    let second = MockServer::start(vec![Behavior::Stream(sse_events())]).await;
    write_deepseek_settings(&settings_path, &first.url, "first-model").await;

    let context = Context::new();
    context
        .provide(SEEKDEEP_LAUNCH_ENVIRONMENT, snapshot)
        .unwrap();
    let runtime = LlmRuntime::install(&context).unwrap();
    let credential_fiber = install_credentials(
        &context,
        LocalCredentialConfig {
            path: Some(home.path().join(".credentials.yaml")),
            seekdeep_home: None,
            watch: false,
            debounce_ms: 0.0,
        },
    )
    .unwrap();
    credential_fiber.await_settled().await.unwrap();
    context
        .get(CREDENTIALS)
        .unwrap()
        .set(&credential_ref("DEEPSEEK_API_KEY").unwrap(), "file-key")
        .await
        .unwrap();
    let settings_fiber = install_settings_file(
        &context,
        FileSettingsConfig {
            path: Some(settings_path.clone()),
            watch: true,
            debounce_ms: 5.0,
            ..FileSettingsConfig::default()
        },
    )
    .unwrap();
    settings_fiber.await_settled().await.unwrap();
    let provider_fiber = install_deepseek(&context, DeepSeekConfig::default()).unwrap();
    provider_fiber.await_settled().await.unwrap();

    let mut first_request = request();
    first_request.model = "first-model".into();
    runtime
        .stream(first_request)
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    assert_eq!(
        first.requests()[0].headers["authorization"],
        "Bearer file-key"
    );
    assert_eq!(first.requests()[0].body["model"], "first-model");

    write_deepseek_settings(&settings_path, &second.url, "second-model").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let models = runtime.list_models("deepseek-official").await.unwrap();
            if models.len() == 1 && models[0].id.as_str() == "second-model" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let mut second_request = request();
    second_request.model = "second-model".into();
    runtime
        .stream(second_request)
        .try_collect::<Vec<_>>()
        .await
        .unwrap();
    assert_eq!(
        second.requests()[0].headers["authorization"],
        "Bearer file-key"
    );
    assert_eq!(second.requests()[0].body["model"], "second-model");
    assert_eq!(first.requests().len(), 1);

    provider_fiber.dispose().await.unwrap();
    settings_fiber.dispose().await.unwrap();
    credential_fiber.dispose().await.unwrap();
    assert!(runtime.list_providers().is_empty());
}

#[test]
fn wire_delta_types_keep_parallel_index_and_optional_fragments() {
    let delta = WireToolCallDelta {
        index: 7,
        id: None,
        kind: None,
        function: Some(WireFunctionDelta {
            name: None,
            arguments: Some("{}".to_owned()),
        }),
    };
    assert_eq!(delta.index, 7);
    assert_eq!(delta.function.unwrap().arguments.as_deref(), Some("{}"));
}

#[test]
fn default_retry_policy_is_captured_as_normal() {
    let resolved = connection("http://127.0.0.1:1");
    assert_eq!(resolved.retry_policy.max_retries(), Some(2));
    assert_eq!(resolved.api_key_env, CredentialRef::new("DEEPSEEK_API_KEY"));
}
