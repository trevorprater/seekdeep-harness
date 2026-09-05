//! OpenAI Responses native protocol engine.

use std::{
    collections::{HashMap, HashSet},
    pin::Pin,
    sync::{Arc, LazyLock},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures::{SinkExt as _, Stream, StreamExt as _};
use parking_lot::Mutex;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use seekdeep_llm::CallId;
use seekdeep_llm_deepseek::sse::{ByteStream, parse_sse};
use serde::Serialize;
use serde_json::{Map, Value, json};
use tokio_tungstenite::tungstenite::{
    Message as WebSocketMessage,
    client::IntoClientRequest as _,
    http::{HeaderName as WsHeaderName, HeaderValue as WsHeaderValue},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::{
    adapter::{BoxPiEventStream, PiExecutionRequest, PiProtocolExecutor},
    catalog::{PiModality, PiModel, PiThinkingLevel},
    config::{PiCacheRetention, PiTransport},
    context::{PiContext, PiMessage, PiToolResultMessage, PiUserContent, PiUserContentBlock},
    json::{stringify, stringify_object},
    provider::{PiProtocol, PiProviderDispatch},
    replay::{
        PiAssistantBlock, PiAssistantMessage, PiAssistantRole, PiCost, PiResponseId, PiStopReason,
        PiUsage,
    },
    stream::{PiAssistantEvent, PiToolCall},
};

const MIN_OUTPUT_TOKENS: u64 = 16;
const DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS: u64 = 15_000;
const CODEX_WEBSOCKET_BETA: &str = "responses_websockets=2026-02-06";
const SESSION_WEBSOCKET_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const SESSION_WEBSOCKET_MAX_AGE: Duration = Duration::from_secs(55 * 60);

/// Session-scoped Codex WebSocket diagnostics matching pi-ai's debug surface.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAiCodexWebSocketDebugStats {
    /// Requests attempted over WebSocket.
    pub requests: u64,
    /// Fresh sockets created.
    pub connections_created: u64,
    /// Cached sockets reused.
    pub connections_reused: u64,
    /// Requests eligible for continuation deltas.
    pub cached_context_requests: u64,
    /// Requests carrying `store: true`.
    pub store_true_requests: u64,
    /// Full-context requests.
    pub full_context_requests: u64,
    /// Delta continuation requests.
    pub delta_requests: u64,
    /// Input item count on the last WebSocket request.
    pub last_input_items: u64,
    /// Input item count on the last delta request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_delta_input_items: Option<u64>,
    /// Last continuation response id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_previous_response_id: Option<String>,
    /// WebSocket transport failures.
    pub websocket_failures: u64,
    /// SSE fallbacks selected.
    pub sse_fallbacks: u64,
    /// Last WebSocket failure diagnostic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_websocket_error: Option<String>,
    /// Whether this session is pinned to SSE.
    pub websocket_fallback_active: bool,
}

static CODEX_WEBSOCKET_STATS: LazyLock<Mutex<HashMap<String, OpenAiCodexWebSocketDebugStats>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static CODEX_WEBSOCKET_SSE_FALLBACKS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));
static CODEX_WEBSOCKET_RUNTIMES: LazyLock<Mutex<Vec<std::sync::Weak<CodexWebSocketRuntime>>>> =
    LazyLock::new(|| Mutex::new(Vec::new()));

/// Returns a copy of one session's Codex WebSocket diagnostic counters.
#[must_use]
pub fn get_openai_codex_websocket_debug_stats(
    session_id: &str,
) -> Option<OpenAiCodexWebSocketDebugStats> {
    CODEX_WEBSOCKET_STATS.lock().get(session_id).cloned()
}

/// Clears Codex WebSocket diagnostics and SSE-fallback state for one or all sessions.
pub fn reset_openai_codex_websocket_debug_stats(session_id: Option<&str>) {
    if let Some(session_id) = session_id {
        CODEX_WEBSOCKET_STATS.lock().remove(session_id);
        CODEX_WEBSOCKET_SSE_FALLBACKS.lock().remove(session_id);
    } else {
        CODEX_WEBSOCKET_STATS.lock().clear();
        CODEX_WEBSOCKET_SSE_FALLBACKS.lock().clear();
    }
}

/// Closes cached Codex `WebSockets` for one session or for every live executor.
pub async fn close_openai_codex_websocket_sessions(session_id: Option<&str>) {
    let runtimes = {
        let mut registered = CODEX_WEBSOCKET_RUNTIMES.lock();
        let runtimes = registered
            .iter()
            .filter_map(std::sync::Weak::upgrade)
            .collect::<Vec<_>>();
        registered.retain(|runtime| runtime.strong_count() > 0);
        runtimes
    };
    for runtime in runtimes {
        if let Some(session_id) = session_id {
            runtime.close_session(session_id).await;
        } else {
            runtime.close_all().await;
        }
    }
}

fn registered_codex_websocket_runtime() -> Arc<CodexWebSocketRuntime> {
    let runtime = Arc::new(CodexWebSocketRuntime::default());
    CODEX_WEBSOCKET_RUNTIMES
        .lock()
        .push(Arc::downgrade(&runtime));
    runtime
}

fn record_codex_websocket_request(
    session_id: Option<&str>,
    reused: bool,
    cached_context: bool,
    body: &Value,
) {
    let Some(session_id) = session_id else { return };
    let mut stats = CODEX_WEBSOCKET_STATS.lock();
    let stats = stats.entry(session_id.to_owned()).or_default();
    stats.requests += 1;
    if reused {
        stats.connections_reused += 1;
    } else {
        stats.connections_created += 1;
    }
    if cached_context {
        stats.cached_context_requests += 1;
    }
    if body.get("store").and_then(Value::as_bool) == Some(true) {
        stats.store_true_requests += 1;
    }
    stats.last_input_items = body
        .get("input")
        .and_then(Value::as_array)
        .map_or(0, |input| u64::try_from(input.len()).unwrap_or(u64::MAX));
    if let Some(previous) = body.get("previous_response_id").and_then(Value::as_str) {
        stats.delta_requests += 1;
        stats.last_delta_input_items = Some(stats.last_input_items);
        stats.last_previous_response_id = Some(previous.to_owned());
    } else {
        stats.full_context_requests += 1;
        stats.last_delta_input_items = None;
        stats.last_previous_response_id = None;
    }
}

type PayloadStream = Pin<Box<dyn Stream<Item = anyhow::Result<String>> + Send + 'static>>;

type CodexSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Clone, Debug)]
struct CodexContinuation {
    request_body: Value,
    response_id: String,
    response_items: Vec<Value>,
}

#[derive(Debug)]
struct CachedCodexSocket {
    socket: CodexSocket,
    created_at: tokio::time::Instant,
    last_used_at: tokio::time::Instant,
    continuation: Option<CodexContinuation>,
}

#[derive(Debug)]
struct CachedCodexEntry {
    connection: Arc<tokio::sync::Mutex<CachedCodexSocket>>,
    abort: seekdeep_llm::AbortSignal,
}

#[derive(Debug, Default)]
struct CodexWebSocketRuntime {
    sessions: Mutex<HashMap<String, Arc<CachedCodexEntry>>>,
}

enum CodexSocketLease {
    Cached(CachedCodexLease),
    Temporary(Box<CodexSocket>),
}

struct CachedCodexLease {
    runtime: Arc<CodexWebSocketRuntime>,
    key: String,
    entry: Arc<CachedCodexEntry>,
    guard: tokio::sync::OwnedMutexGuard<CachedCodexSocket>,
    keep: bool,
    reused: bool,
}

impl CodexSocketLease {
    fn socket(&mut self) -> &mut CodexSocket {
        match self {
            Self::Cached(lease) => &mut lease.guard.socket,
            Self::Temporary(socket) => socket,
        }
    }

    fn continuation(&self) -> Option<&CodexContinuation> {
        match self {
            Self::Cached(lease) => lease.guard.continuation.as_ref(),
            Self::Temporary(_) => None,
        }
    }

    fn set_continuation(&mut self, continuation: CodexContinuation) {
        if let Self::Cached(lease) = self {
            lease.guard.continuation = Some(continuation);
        }
    }

    fn clear_continuation(&mut self) {
        if let Self::Cached(lease) = self {
            lease.guard.continuation = None;
        }
    }

    fn keep(&mut self) {
        if let Self::Cached(lease) = self {
            lease.keep = true;
        }
    }

    fn session_abort(&self) -> Option<seekdeep_llm::AbortSignal> {
        match self {
            Self::Cached(lease) => Some(lease.entry.abort.clone()),
            Self::Temporary(_) => None,
        }
    }

    fn reused(&self) -> bool {
        matches!(self, Self::Cached(lease) if lease.reused)
    }
}

impl Drop for CachedCodexLease {
    fn drop(&mut self) {
        if !self.keep {
            self.runtime.remove_if_same(&self.key, &self.entry);
            return;
        }
        self.guard.last_used_at = tokio::time::Instant::now();
        let runtime = self.runtime.clone();
        let key = self.key.clone();
        let entry = self.entry.clone();
        tokio::spawn(async move {
            tokio::time::sleep(SESSION_WEBSOCKET_CACHE_TTL).await;
            let remove = entry
                .connection
                .try_lock()
                .is_ok_and(|cached| cached.last_used_at.elapsed() >= SESSION_WEBSOCKET_CACHE_TTL);
            if remove {
                runtime.remove_if_same(&key, &entry);
            }
        });
    }
}

impl CodexWebSocketRuntime {
    fn remove_if_same(&self, key: &str, entry: &Arc<CachedCodexEntry>) {
        let mut sessions = self.sessions.lock();
        if sessions
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, entry))
        {
            sessions.remove(key);
        }
    }

    fn fallback_active(key: Option<&str>) -> bool {
        key.is_some_and(|key| CODEX_WEBSOCKET_SSE_FALLBACKS.lock().contains(key))
    }

    fn record_failure(key: Option<&str>, error: impl std::fmt::Display) {
        if let Some(key) = key {
            CODEX_WEBSOCKET_SSE_FALLBACKS.lock().insert(key.to_owned());
            let mut stats = CODEX_WEBSOCKET_STATS.lock();
            let stats = stats.entry(key.to_owned()).or_default();
            stats.websocket_failures += 1;
            stats.last_websocket_error = Some(error.to_string());
            stats.websocket_fallback_active = true;
        }
    }

    fn record_sse_fallback(key: Option<&str>) {
        if let Some(key) = key {
            let mut stats = CODEX_WEBSOCKET_STATS.lock();
            let stats = stats.entry(key.to_owned()).or_default();
            stats.sse_fallbacks += 1;
            stats.websocket_fallback_active = Self::fallback_active(Some(key));
        }
    }

    async fn close_session(&self, key: &str) {
        let entry = self.sessions.lock().remove(key);
        let Some(entry) = entry else { return };
        entry.abort.abort_with_reason(json!("session disposed"));
        let mut connection = entry.connection.lock().await;
        let _ = connection.socket.close(None).await;
    }

    async fn close_all(&self) {
        let entries = self
            .sessions
            .lock()
            .drain()
            .map(|(_, entry)| entry)
            .collect::<Vec<_>>();
        for entry in entries {
            entry
                .abort
                .abort_with_reason(json!("Codex WebSocket sessions closed"));
            let mut connection = entry.connection.lock().await;
            let _ = connection.socket.close(None).await;
        }
    }

    async fn acquire(
        self: &Arc<Self>,
        request: tokio_tungstenite::tungstenite::http::Request<()>,
        cache_key: Option<&str>,
        connect_timeout_ms: u64,
        signal: &seekdeep_llm::AbortSignal,
    ) -> anyhow::Result<CodexSocketLease> {
        let Some(key) = cache_key else {
            return Ok(CodexSocketLease::Temporary(Box::new(
                Box::pin(connect_codex_socket(request, connect_timeout_ms, signal)).await?,
            )));
        };
        let existing = self.sessions.lock().get(key).cloned();
        if let Some(entry) = existing {
            match entry.connection.clone().try_lock_owned() {
                Ok(guard) if guard.created_at.elapsed() < SESSION_WEBSOCKET_MAX_AGE => {
                    return Ok(CodexSocketLease::Cached(CachedCodexLease {
                        runtime: self.clone(),
                        key: key.to_owned(),
                        entry,
                        guard,
                        keep: false,
                        reused: true,
                    }));
                }
                Ok(_) => self.remove_if_same(key, &entry),
                Err(_) => {
                    return Ok(CodexSocketLease::Temporary(Box::new(
                        Box::pin(connect_codex_socket(request, connect_timeout_ms, signal)).await?,
                    )));
                }
            }
        }
        let socket = Box::pin(connect_codex_socket(request, connect_timeout_ms, signal)).await?;
        let now = tokio::time::Instant::now();
        let entry = Arc::new(CachedCodexEntry {
            connection: Arc::new(tokio::sync::Mutex::new(CachedCodexSocket {
                socket,
                created_at: now,
                last_used_at: now,
                continuation: None,
            })),
            abort: seekdeep_llm::AbortSignal::default(),
        });
        self.sessions.lock().insert(key.to_owned(), entry.clone());
        let guard = entry.connection.clone().lock_owned().await;
        Ok(CodexSocketLease::Cached(CachedCodexLease {
            runtime: self.clone(),
            key: key.to_owned(),
            entry,
            guard,
            keep: false,
            reused: false,
        }))
    }
}

/// Reqwest-backed `OpenAI` Responses engine.
#[derive(Clone, Debug)]
pub struct OpenAiResponsesExecutor {
    http: reqwest::Client,
    flavor: ResponsesFlavor,
    codex_websockets: Arc<CodexWebSocketRuntime>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResponsesFlavor {
    OpenAi,
    Azure,
    Codex,
}

impl OpenAiResponsesExecutor {
    /// Creates an executor using one reusable HTTP client.
    #[must_use]
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            flavor: ResponsesFlavor::OpenAi,
            codex_websockets: Arc::new(CodexWebSocketRuntime::default()),
        }
    }

    /// Creates the official `ChatGPT` Codex SSE flavor.
    #[must_use]
    pub fn new_codex(http: reqwest::Client) -> Self {
        Self {
            http,
            flavor: ResponsesFlavor::Codex,
            codex_websockets: registered_codex_websocket_runtime(),
        }
    }

    /// Creates the Azure `OpenAI` Responses flavor.
    #[must_use]
    pub fn new_azure(http: reqwest::Client) -> Self {
        Self {
            http,
            flavor: ResponsesFlavor::Azure,
            codex_websockets: Arc::new(CodexWebSocketRuntime::default()),
        }
    }

    /// Closes and forgets a cached Codex socket for one disposed session.
    pub(crate) async fn close_codex_session(&self, session: &str) {
        if self.flavor == ResponsesFlavor::Codex {
            self.codex_websockets.close_session(session).await;
        }
    }
}

impl PiProtocolExecutor for OpenAiResponsesExecutor {
    fn stream(&self, request: PiExecutionRequest) -> anyhow::Result<BoxPiEventStream> {
        let expected_api = match self.flavor {
            ResponsesFlavor::OpenAi => "openai-responses",
            ResponsesFlavor::Azure => "azure-openai-responses",
            ResponsesFlavor::Codex => "openai-codex-responses",
        };
        if request.model.api.as_str() != expected_api
            || matches!(
                request.provider.dispatch,
                PiProviderDispatch::Protocol(protocol)
                    if matches!(self.flavor, ResponsesFlavor::OpenAi | ResponsesFlavor::Azure)
                        && protocol != PiProtocol::OpenAiResponses
            )
        {
            anyhow::bail!(
                "native OpenAI Responses executor cannot dispatch api \"{}\"",
                request.model.api.as_str()
            );
        }
        let has_auth_header = has_auth_header(&request);
        if request.options.api_key.as_deref().is_none_or(str::is_empty)
            && (matches!(self.flavor, ResponsesFlavor::Azure | ResponsesFlavor::Codex)
                || !has_auth_header)
        {
            anyhow::bail!(
                "No API key for provider: {}",
                request.model.provider.as_str()
            );
        }
        let output = Arc::new(Mutex::new(empty_assistant(&request.model)));
        let signal = request.options.signal.clone();
        let native = native_events(
            self.http.clone(),
            request,
            output.clone(),
            self.flavor,
            self.codex_websockets.clone(),
        );
        Ok(Box::pin(async_stream::stream! {
            futures::pin_mut!(native);
            while let Some(event) = native.next().await {
                match event {
                    Ok(event) => yield Ok(event),
                    Err(error) => {
                        let mut failed = output.lock().clone();
                        failed.stop_reason = if signal.is_aborted() {
                            PiStopReason::Aborted
                        } else {
                            PiStopReason::Error
                        };
                        failed.error_message = Some(error.to_string());
                        yield Ok(PiAssistantEvent::Error { reason: failed.stop_reason, error: failed });
                        return;
                    }
                }
            }
        }))
    }
}

fn has_auth_header(request: &PiExecutionRequest) -> bool {
    let eligible = |name: &str| {
        name.eq_ignore_ascii_case("authorization")
            || name.eq_ignore_ascii_case("cf-aig-authorization")
    };
    request
        .model
        .extra
        .get("headers")
        .and_then(Value::as_object)
        .is_some_and(|headers| {
            headers.iter().any(|(name, value)| {
                eligible(name) && value.as_str().is_some_and(|value| !value.trim().is_empty())
            })
        })
        || request
            .options
            .headers
            .iter()
            .any(|(name, value)| eligible(name) && !value.trim().is_empty())
}

#[derive(Clone, Debug)]
enum Slot {
    Thinking {
        content_index: usize,
    },
    Text {
        content_index: usize,
    },
    ToolCall {
        content_index: usize,
        partial: String,
    },
}

struct OpenedPayloads {
    stream: PayloadStream,
    start_immediately: bool,
}

enum CodexWebSocketPreflightError {
    ConnectionLimit,
    Transport(anyhow::Error),
}

async fn open_response_payloads(
    http: reqwest::Client,
    request: &PiExecutionRequest,
    flavor: ResponsesFlavor,
    body: Value,
    codex_websockets: Arc<CodexWebSocketRuntime>,
) -> anyhow::Result<OpenedPayloads> {
    let transport = request.options.transport.unwrap_or(PiTransport::Auto);
    let cache_key = codex_cache_key(request);
    let fallback_active = CodexWebSocketRuntime::fallback_active(cache_key.as_deref());
    if flavor == ResponsesFlavor::Codex && transport != PiTransport::Sse && !fallback_active {
        let mut retried_connection_limit = false;
        loop {
            match Box::pin(open_codex_websocket_payloads(
                request,
                body.clone(),
                codex_websockets.clone(),
            ))
            .await
            {
                Ok(stream) => match preflight_codex_websocket(stream).await {
                    Ok(stream) => {
                        return Ok(OpenedPayloads {
                            stream,
                            start_immediately: false,
                        });
                    }
                    Err(CodexWebSocketPreflightError::ConnectionLimit)
                        if !retried_connection_limit =>
                    {
                        retried_connection_limit = true;
                    }
                    Err(CodexWebSocketPreflightError::ConnectionLimit) => {
                        CodexWebSocketRuntime::record_failure(
                            cache_key.as_deref(),
                            "websocket_connection_limit_reached",
                        );
                        break;
                    }
                    Err(CodexWebSocketPreflightError::Transport(error)) => {
                        if request.options.signal.is_aborted() {
                            return Err(error);
                        }
                        CodexWebSocketRuntime::record_failure(cache_key.as_deref(), &error);
                        break;
                    }
                },
                Err(error) if request.options.signal.is_aborted() => return Err(error),
                Err(error) => {
                    CodexWebSocketRuntime::record_failure(cache_key.as_deref(), &error);
                    break;
                }
            }
        }
    }
    if flavor == ResponsesFlavor::Codex && transport != PiTransport::Sse {
        CodexWebSocketRuntime::record_sse_fallback(cache_key.as_deref());
    }
    Ok(OpenedPayloads {
        stream: open_sse_payloads(http, request, flavor, &body).await?,
        start_immediately: true,
    })
}

async fn preflight_codex_websocket(
    mut stream: PayloadStream,
) -> Result<PayloadStream, CodexWebSocketPreflightError> {
    let first = stream
        .next()
        .await
        .ok_or_else(|| {
            CodexWebSocketPreflightError::Transport(anyhow::anyhow!(
                "WebSocket stream closed before response.completed"
            ))
        })?
        .map_err(CodexWebSocketPreflightError::Transport)?;
    let event: Value = serde_json::from_str(&first).map_err(|error| {
        CodexWebSocketPreflightError::Transport(anyhow::anyhow!(
            "Invalid Codex WebSocket JSON: {error}"
        ))
    })?;
    if event.get("type").and_then(Value::as_str) == Some("error")
        && codex_error_code(&event) == Some("websocket_connection_limit_reached")
    {
        return Err(CodexWebSocketPreflightError::ConnectionLimit);
    }
    Ok(Box::pin(
        futures::stream::once(async move { Ok(first) }).chain(stream),
    ))
}

async fn open_sse_payloads(
    http: reqwest::Client,
    request: &PiExecutionRequest,
    flavor: ResponsesFlavor,
    body: &Value,
) -> anyhow::Result<PayloadStream> {
    let url = response_url(&request.model.base_url, flavor);
    let headers = request_headers(request, flavor)?;
    let uncompressed = serde_json::to_vec(body)?;
    let (wire_body, compressed) = if flavor == ResponsesFlavor::Codex {
        zstd::stream::encode_all(uncompressed.as_slice(), 3)
            .map_or((uncompressed, false), |compressed| (compressed, true))
    } else {
        (uncompressed, false)
    };
    let mut builder = http
        .post(url)
        .headers(headers)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(wire_body);
    if compressed {
        builder = builder.header(reqwest::header::CONTENT_ENCODING, "zstd");
    }
    if let Some(timeout_ms) = request.options.timeout_ms {
        builder = builder.timeout(Duration::from_millis(timeout_ms));
    }
    let response_result: anyhow::Result<reqwest::Response> = tokio::select! {
        biased;
        () = request.options.signal.cancelled() => Err(anyhow::anyhow!("Request was aborted")),
        response = builder.send() => response.map_err(anyhow::Error::from),
    };
    let response = response_result?;
    let response = if response.status().is_success() {
        response
    } else {
        let status = response.status().as_u16();
        let text_result: anyhow::Result<String> = tokio::select! {
            biased;
            () = request.options.signal.cancelled() => Err(anyhow::anyhow!("Request was aborted")),
            body = response.text() => body.map_err(anyhow::Error::from),
        };
        let text = text_result?;
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|body| {
                body.get("error")?
                    .get("message")?
                    .as_str()
                    .map(str::to_owned)
            })
            .filter(|message| !message.is_empty())
            .unwrap_or(text);
        anyhow::bail!("OpenAI API error ({status}): {detail}");
    };
    let bytes: ByteStream = Box::pin(
        response
            .bytes_stream()
            .map(|result| result.map_err(anyhow::Error::from)),
    );
    Ok(Box::pin(parse_sse(bytes, None)))
}

#[allow(clippy::too_many_lines)] // One owned WebSocket request/continuation lifecycle.
async fn open_codex_websocket_payloads(
    request: &PiExecutionRequest,
    body: Value,
    runtime: Arc<CodexWebSocketRuntime>,
) -> anyhow::Result<PayloadStream> {
    let url = codex_websocket_url(&request.model.base_url)?;
    let request_id = request.options.session_id.as_ref().map_or_else(
        || uuid::Uuid::now_v7().to_string(),
        |session| clamp_cache_key(session.as_str()),
    );
    let mut headers = request_headers(request, ResponsesFlavor::Codex)?;
    headers.remove(reqwest::header::ACCEPT);
    headers.remove(reqwest::header::CONTENT_TYPE);
    headers.remove("openai-beta");
    headers.insert(
        HeaderName::from_static("openai-beta"),
        HeaderValue::from_static(CODEX_WEBSOCKET_BETA),
    );
    headers.insert(
        HeaderName::from_static("x-client-request-id"),
        HeaderValue::from_str(&request_id)?,
    );
    headers.insert(
        HeaderName::from_static("session-id"),
        HeaderValue::from_str(&request_id)?,
    );
    let mut websocket_request = url.into_client_request()?;
    for (name, value) in &headers {
        websocket_request.headers_mut().insert(
            name.as_str().parse::<WsHeaderName>()?,
            WsHeaderValue::from_bytes(value.as_bytes())?,
        );
    }
    let connect_timeout = request
        .options
        .websocket_connect_timeout_ms
        .unwrap_or(DEFAULT_WEBSOCKET_CONNECT_TIMEOUT_MS);
    let signal = request.options.signal.clone();
    let cache_key = codex_cache_key(request);
    let mut lease = Box::pin(runtime.acquire(
        websocket_request,
        cache_key.as_deref(),
        connect_timeout,
        &signal,
    ))
    .await?;
    let signal = lease.session_abort().map_or(signal.clone(), |session| {
        seekdeep_llm::AbortSignal::fuse(&signal, &session)
    });
    let use_cached_context = matches!(
        request.options.transport.unwrap_or(PiTransport::Auto),
        PiTransport::WebsocketCached | PiTransport::Auto
    );
    let continuation = lease.continuation().cloned();
    let (mut request_body, used_delta) = if use_cached_context {
        continuation.as_ref().map_or_else(
            || (body.clone(), false),
            |continuation| cached_websocket_body(&body, continuation),
        )
    } else {
        (body.clone(), false)
    };
    if use_cached_context && continuation.is_some() && !used_delta {
        lease.clear_continuation();
    }
    record_codex_websocket_request(
        cache_key.as_deref(),
        lease.reused(),
        use_cached_context,
        &request_body,
    );
    insert_response_create_type(&mut request_body)?;
    lease
        .socket()
        .send(WebSocketMessage::Text(
            serde_json::to_string(&request_body)?.into(),
        ))
        .await?;
    let idle_timeout_ms = request.options.timeout_ms;
    let full_body = body;
    let failure_key = cache_key;
    Ok(Box::pin(async_stream::try_stream! {
        let mut saw_terminal = false;
        let mut retried_missing_continuation = false;
        loop {
            let next = async {
                match idle_timeout_ms {
                    Some(timeout_ms) if timeout_ms > 0 => {
                        tokio::time::timeout(Duration::from_millis(timeout_ms), lease.socket().next())
                            .await
                            .map_err(|_| {
                                let error = format!("WebSocket idle timeout after {timeout_ms}ms");
                                CodexWebSocketRuntime::record_failure(failure_key.as_deref(), &error);
                                anyhow::anyhow!(error)
                            })
                    }
                    _ => Ok(lease.socket().next().await),
                }
            };
            let message_result: anyhow::Result<_> = tokio::select! {
                biased;
                () = signal.cancelled() => Err(anyhow::anyhow!("Request was aborted")),
                message = next => message,
            };
            let message = message_result?;
            let Some(message) = message else { break };
            let message = match message {
                Ok(message) => message,
                Err(error) => {
                    CodexWebSocketRuntime::record_failure(failure_key.as_deref(), &error);
                    Err::<WebSocketMessage, _>(error)?
                }
            };
            let text = match message {
                WebSocketMessage::Text(text) => text.to_string(),
                WebSocketMessage::Binary(bytes) => String::from_utf8(bytes.to_vec())?,
                WebSocketMessage::Ping(payload) => {
                    lease.socket().send(WebSocketMessage::Pong(payload)).await?;
                    continue;
                }
                WebSocketMessage::Pong(_) => continue,
                WebSocketMessage::Close(frame) => {
                    let detail = frame.map_or_else(String::new, |frame| {
                        format!(" ({}) {}", u16::from(frame.code), frame.reason)
                    });
                    let error = format!("WebSocket closed{detail}");
                    CodexWebSocketRuntime::record_failure(failure_key.as_deref(), &error);
                    Err::<String, _>(anyhow::anyhow!(error))?
                }
                WebSocketMessage::Frame(_) => continue,
            };
            let event: Value = serde_json::from_str(&text)
                .map_err(|error| anyhow::anyhow!("Invalid Codex WebSocket JSON: {error}"))?;
            let kind = event.get("type").and_then(Value::as_str).unwrap_or_default();
            if kind == "error"
                && used_delta
                && !retried_missing_continuation
                && codex_error_code(&event) == Some("previous_response_not_found")
            {
                retried_missing_continuation = true;
                lease.clear_continuation();
                let mut retry_body = full_body.clone();
                insert_response_create_type(&mut retry_body)?;
                lease.socket().send(WebSocketMessage::Text(
                    serde_json::to_string(&retry_body)?.into(),
                )).await?;
                continue;
            }
            saw_terminal = matches!(kind, "response.completed" | "response.done" | "response.incomplete");
            if saw_terminal && use_cached_context
                && let Some(response) = event.get("response") {
                    let response_id = response.get("id").and_then(Value::as_str);
                    let response_items = response.get("output").and_then(Value::as_array);
                    if let (Some(response_id), Some(response_items)) = (response_id, response_items) {
                        lease.set_continuation(CodexContinuation {
                            request_body: full_body.clone(),
                            response_id: response_id.to_owned(),
                            response_items: response_items.clone(),
                        });
                    }
            }
            if saw_terminal {
                lease.keep();
            }
            yield text;
            if saw_terminal {
                return;
            }
        }
        if !saw_terminal {
            let error = "WebSocket stream closed before response.completed";
            CodexWebSocketRuntime::record_failure(failure_key.as_deref(), error);
            Err::<(), _>(anyhow::anyhow!(error))?;
        }
    }))
}

async fn connect_codex_socket(
    request: tokio_tungstenite::tungstenite::http::Request<()>,
    connect_timeout_ms: u64,
    signal: &seekdeep_llm::AbortSignal,
) -> anyhow::Result<CodexSocket> {
    let connect = tokio_tungstenite::connect_async(request);
    let connected = async {
        if connect_timeout_ms == 0 {
            connect.await.map_err(anyhow::Error::from)
        } else {
            tokio::time::timeout(Duration::from_millis(connect_timeout_ms), connect)
                .await
                .map_err(|_| {
                    anyhow::anyhow!("WebSocket connect timeout after {connect_timeout_ms}ms")
                })?
                .map_err(anyhow::Error::from)
        }
    };
    let (socket, _) = tokio::select! {
        biased;
        () = signal.cancelled() => Err(anyhow::anyhow!("Request was aborted"))?,
        connected = connected => connected?,
    };
    Ok(socket)
}

fn codex_cache_key(request: &PiExecutionRequest) -> Option<String> {
    if request.options.cache_retention == Some(PiCacheRetention::None) {
        return None;
    }
    request
        .options
        .session_id
        .as_ref()
        .map(|session| session.as_str().to_owned())
}

fn insert_response_create_type(body: &mut Value) -> anyhow::Result<()> {
    body.as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Codex request body is not an object"))?
        .insert(
            "type".to_owned(),
            Value::String("response.create".to_owned()),
        );
    Ok(())
}

fn cached_websocket_body(body: &Value, continuation: &CodexContinuation) -> (Value, bool) {
    if request_body_without_input(body) != request_body_without_input(&continuation.request_body) {
        return (body.clone(), false);
    }
    let current = body
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut baseline = continuation
        .request_body
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    baseline.extend(continuation.response_items.clone());
    if current.len() < baseline.len() || current[..baseline.len()] != baseline {
        return (body.clone(), false);
    }
    let mut delta = body.clone();
    let object = delta.as_object_mut().expect("Codex body was validated");
    object.insert(
        "previous_response_id".to_owned(),
        Value::String(continuation.response_id.clone()),
    );
    object.insert(
        "input".to_owned(),
        Value::Array(current[baseline.len()..].to_vec()),
    );
    (delta, true)
}

fn request_body_without_input(body: &Value) -> Value {
    let mut body = body.clone();
    if let Some(object) = body.as_object_mut() {
        object.remove("input");
        object.remove("previous_response_id");
    }
    body
}

fn codex_error_code(event: &Value) -> Option<&str> {
    event.get("code").and_then(Value::as_str).or_else(|| {
        event
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str)
    })
}

fn codex_websocket_url(base_url: &str) -> anyhow::Result<String> {
    let mut url = reqwest::Url::parse(&response_url(base_url, ResponsesFlavor::Codex))?;
    match url.scheme() {
        "https" => url.set_scheme("wss").expect("wss is a valid URL scheme"),
        "http" => url.set_scheme("ws").expect("ws is a valid URL scheme"),
        "ws" | "wss" => {}
        scheme => anyhow::bail!("Codex WebSocket URL uses unsupported scheme {scheme}"),
    }
    Ok(url.into())
}

#[allow(clippy::too_many_lines)] // Closed source event machine in wire order.
fn native_events(
    http: reqwest::Client,
    request: PiExecutionRequest,
    shared: Arc<Mutex<PiAssistantMessage>>,
    flavor: ResponsesFlavor,
    codex_websockets: Arc<CodexWebSocketRuntime>,
) -> BoxPiEventStream {
    Box::pin(async_stream::try_stream! {
        let body = build_request(&request.model, &request.context, &request.options, flavor)?;
        let opened = Box::pin(open_response_payloads(
            http,
            &request,
            flavor,
            body,
            codex_websockets,
        )).await?;
        let mut payloads = opened.stream;
        let mut output = shared.lock().clone();
        let mut slots = HashMap::<u64, Slot>::new();
        let mut started = opened.start_immediately;
        if started {
            yield PiAssistantEvent::Start { partial: output.clone() };
        }
        while let Some(payload) = payloads.next().await {
            let payload = payload?;
            let event: Value = serde_json::from_str(&payload)?;
            let kind = event.get("type").and_then(Value::as_str).unwrap_or_default();
            if !started && !matches!(kind, "error" | "response.failed") {
                started = true;
                yield PiAssistantEvent::Start { partial: output.clone() };
            }
            match kind {
                "response.created" => {
                    output.response_id = event.get("response").and_then(|value| value.get("id"))
                        .and_then(Value::as_str).map(PiResponseId::new);
                }
                "response.output_item.added" => {
                    let output_index = number(event.get("output_index"));
                    if let Some(item) = event.get("item")
                        && let Some((slot, opened)) = create_slot(&mut output, output_index, item)
                    {
                            slots.insert(output_index, slot.clone());
                            match slot {
                                Slot::Thinking { content_index } => yield PiAssistantEvent::ThinkingStart {
                                    content_index: index_u64(content_index), partial: output.clone(),
                                },
                                Slot::Text { content_index } => yield PiAssistantEvent::TextStart {
                                    content_index: index_u64(content_index), partial: output.clone(),
                                },
                                Slot::ToolCall { content_index, .. } => yield PiAssistantEvent::ToolCallStart {
                                    content_index: index_u64(content_index), partial: output.clone(),
                                },
                            }
                            debug_assert!(opened);
                    }
                }
                "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                    let output_index = number(event.get("output_index"));
                    if let Some(Slot::Thinking { content_index }) = slots.get(&output_index) {
                        let index = *content_index;
                        let delta = event.get("delta").and_then(Value::as_str).unwrap_or_default();
                        if let PiAssistantBlock::Thinking { thinking, .. } = &mut output.content[index] {
                            thinking.push_str(delta);
                        }
                        publish(&shared, &output);
                        yield PiAssistantEvent::ThinkingDelta {
                            content_index: index_u64(index), delta: delta.to_owned(), partial: output.clone(),
                        };
                    }
                }
                "response.reasoning_summary_part.done" => {
                    let output_index = number(event.get("output_index"));
                    if let Some(Slot::Thinking { content_index }) = slots.get(&output_index) {
                        let index = *content_index;
                        if let PiAssistantBlock::Thinking { thinking, .. } = &mut output.content[index] {
                            thinking.push_str("\n\n");
                        }
                        yield PiAssistantEvent::ThinkingDelta {
                            content_index: index_u64(index), delta: "\n\n".to_owned(), partial: output.clone(),
                        };
                    }
                }
                "response.output_text.delta" | "response.refusal.delta" => {
                    let output_index = number(event.get("output_index"));
                    if let Some(Slot::Text { content_index }) = slots.get(&output_index) {
                        let index = *content_index;
                        let delta = event.get("delta").and_then(Value::as_str).unwrap_or_default();
                        if let PiAssistantBlock::Text { text, .. } = &mut output.content[index] {
                            text.push_str(delta);
                        }
                        publish(&shared, &output);
                        yield PiAssistantEvent::TextDelta {
                            content_index: index_u64(index), delta: delta.to_owned(), partial: output.clone(),
                        };
                    }
                }
                "response.function_call_arguments.delta" => {
                    let output_index = number(event.get("output_index"));
                    if let Some(Slot::ToolCall { content_index, partial }) = slots.get_mut(&output_index) {
                        let index = *content_index;
                        let delta = event.get("delta").and_then(Value::as_str).unwrap_or_default();
                        partial.push_str(delta);
                        set_tool_arguments(&mut output.content[index], partial);
                        publish(&shared, &output);
                        yield PiAssistantEvent::ToolCallDelta {
                            content_index: index_u64(index), delta: delta.to_owned(), partial: output.clone(),
                        };
                    }
                }
                "response.function_call_arguments.done" => {
                    let output_index = number(event.get("output_index"));
                    if let Some(Slot::ToolCall { content_index, partial }) = slots.get_mut(&output_index) {
                        let arguments = event.get("arguments").and_then(Value::as_str).unwrap_or_default();
                        let delta = arguments.strip_prefix(partial.as_str()).unwrap_or_default().to_owned();
                        arguments.clone_into(partial);
                        set_tool_arguments(&mut output.content[*content_index], partial);
                        if !delta.is_empty() {
                            yield PiAssistantEvent::ToolCallDelta {
                                content_index: index_u64(*content_index), delta, partial: output.clone(),
                            };
                        }
                    }
                }
                "response.output_item.done" => {
                    let output_index = number(event.get("output_index"));
                    let item = event.get("item").unwrap_or(&Value::Null);
                    if !slots.contains_key(&output_index)
                        && let Some((slot, _)) = create_slot(&mut output, output_index, item)
                    {
                        slots.insert(output_index, slot);
                    }
                    if let Some(slot) = slots.remove(&output_index) {
                        match slot {
                            Slot::Thinking { content_index } => {
                                let text = reasoning_text(item).unwrap_or_else(|| thinking_at(&output, content_index));
                                let signature = stringify(item)?;
                                output.content[content_index] = PiAssistantBlock::Thinking {
                                    thinking: text.clone(), thinking_signature: Some(signature), redacted: None,
                                };
                                publish(&shared, &output);
                                yield PiAssistantEvent::ThinkingEnd {
                                    content_index: index_u64(content_index), content: text, partial: output.clone(),
                                };
                            }
                            Slot::Text { content_index } => {
                                let text = response_message_text(item);
                                let signature = text_signature(item);
                                output.content[content_index] = PiAssistantBlock::Text {
                                    text: text.clone(), text_signature: signature,
                                };
                                publish(&shared, &output);
                                yield PiAssistantEvent::TextEnd {
                                    content_index: index_u64(content_index), content: text, partial: output.clone(),
                                };
                            }
                            Slot::ToolCall { content_index, partial } => {
                                let raw = item.get("arguments").and_then(Value::as_str).unwrap_or(&partial);
                                let arguments = parse_arguments(raw);
                                let PiAssistantBlock::ToolCall { id, name, thought_signature, .. } = output.content[content_index].clone() else {
                                    unreachable!("tool slot points at tool block")
                                };
                                output.content[content_index] = PiAssistantBlock::ToolCall {
                                    id: id.clone(), name: name.clone(), arguments: arguments.clone(),
                                    thought_signature: thought_signature.clone(),
                                };
                                publish(&shared, &output);
                                yield PiAssistantEvent::ToolCallEnd {
                                    content_index: index_u64(content_index),
                                    tool_call: PiToolCall { id, name, arguments, thought_signature },
                                    partial: output.clone(),
                                };
                            }
                        }
                    }
                }
                "response.completed" | "response.incomplete" | "response.done" => {
                    let response = event.get("response").unwrap_or(&Value::Null);
                    finalize_response(&mut output, response)?;
                    publish(&shared, &output);
                    if request.options.signal.is_aborted() {
                        Err::<(), _>(anyhow::anyhow!("Request was aborted"))?;
                    }
                    yield PiAssistantEvent::Done { reason: output.stop_reason, message: output };
                    return;
                }
                "error" => {
                    let code = event.get("code").and_then(Value::as_str).unwrap_or_default();
                    let message = event.get("message").and_then(Value::as_str).unwrap_or_default();
                    Err::<(), _>(anyhow::anyhow!("Error Code {code}: {message}"))?;
                }
                "response.failed" => {
                    let response = event.get("response").unwrap_or(&Value::Null);
                    let code = response.get("error").and_then(|value| value.get("code")).and_then(Value::as_str).unwrap_or("unknown");
                    let message = response.get("error").and_then(|value| value.get("message")).and_then(Value::as_str).unwrap_or("no message");
                    Err::<(), _>(anyhow::anyhow!("{code}: {message}"))?;
                }
                _ => {}
            }
            publish(&shared, &output);
        }
        Err::<(), _>(anyhow::anyhow!("OpenAI Responses stream ended before a terminal response event"))?;
    })
}

#[allow(clippy::too_many_lines)] // Standard and Codex envelopes share one auditable field order.
fn build_request(
    model: &PiModel,
    context: &PiContext,
    options: &crate::adapter::PiStreamOptions,
    flavor: ResponsesFlavor,
) -> anyhow::Result<Value> {
    let include_system = flavor != ResponsesFlavor::Codex;
    let mut root = Map::from_iter([
        (
            "model".to_owned(),
            Value::String(model.id.as_str().to_owned()),
        ),
        (
            "input".to_owned(),
            Value::Array(convert_messages(model, context, include_system)?),
        ),
        ("stream".to_owned(), Value::Bool(true)),
        ("store".to_owned(), Value::Bool(false)),
    ]);
    if flavor == ResponsesFlavor::Codex {
        root.insert(
            "instructions".to_owned(),
            Value::String(
                context
                    .system_prompt
                    .clone()
                    .unwrap_or_else(|| "You are a helpful assistant.".to_owned()),
            ),
        );
        root.insert("text".to_owned(), json!({"verbosity":"low"}));
        root.insert("include".to_owned(), json!(["reasoning.encrypted_content"]));
        root.insert("tool_choice".to_owned(), Value::String("auto".to_owned()));
        root.insert("parallel_tool_calls".to_owned(), Value::Bool(true));
    }
    if (flavor == ResponsesFlavor::Azure || options.cache_retention != Some(PiCacheRetention::None))
        && let Some(session) = &options.session_id
    {
        root.insert(
            "prompt_cache_key".to_owned(),
            Value::String(clamp_cache_key(session.as_str())),
        );
    }
    if flavor == ResponsesFlavor::OpenAi && options.cache_retention == Some(PiCacheRetention::Long)
    {
        root.insert(
            "prompt_cache_retention".to_owned(),
            Value::String("24h".to_owned()),
        );
    }
    if let Some(max_tokens) = options.max_tokens {
        root.insert(
            "max_output_tokens".to_owned(),
            Value::from(max_tokens.max(MIN_OUTPUT_TOKENS)),
        );
    }
    if let Some(temperature) = options.temperature {
        root.insert("temperature".to_owned(), json!(temperature));
    }
    if let Some(tools) = &context.tools
        && !tools.is_empty()
    {
        let strict = model
            .compat
            .as_ref()
            .and_then(|compat| compat.get("supportsStrictMode"))
            .and_then(Value::as_bool)
            .unwrap_or(flavor == ResponsesFlavor::Azure);
        root.insert(
            "tools".to_owned(),
            Value::Array(
                tools
                    .iter()
                    .map(|tool| {
                        let mut value = Map::from_iter([
                            ("type".to_owned(), Value::String("function".to_owned())),
                            ("name".to_owned(), Value::String(tool.name.clone())),
                            (
                                "description".to_owned(),
                                Value::String(tool.description.clone()),
                            ),
                            (
                                "parameters".to_owned(),
                                Value::Object(tool.parameters.clone()),
                            ),
                        ]);
                        if strict {
                            value.insert("strict".to_owned(), Value::Bool(false));
                        }
                        Value::Object(value)
                    })
                    .collect(),
            ),
        );
    }
    if model.reasoning {
        if let Some(reasoning) = options.reasoning {
            root.insert(
                "reasoning".to_owned(),
                json!({
                    "effort":mapped_effort(model, reasoning),
                    "summary":"auto"
                }),
            );
            root.insert("include".to_owned(), json!(["reasoning.encrypted_content"]));
        } else if model.provider.as_str() != "github-copilot"
            && model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get("off"))
                != Some(&Value::Null)
        {
            let off = model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get("off"))
                .and_then(Value::as_str)
                .unwrap_or("none");
            root.insert("reasoning".to_owned(), json!({"effort":off}));
        }
    }
    Ok(Value::Object(root))
}

fn convert_messages(
    model: &PiModel,
    context: &PiContext,
    include_system: bool,
) -> anyhow::Result<Vec<Value>> {
    let mut messages = Vec::new();
    if include_system
        && let Some(system) = context
            .system_prompt
            .as_deref()
            .filter(|value| !value.is_empty())
    {
        messages.push(json!({
            "role":if model.reasoning {"developer"}else{"system"},
            "content":system
        }));
    }
    for (message_index, message) in context.messages.iter().enumerate() {
        match message {
            PiMessage::User(message) => {
                let wire_content = match &message.content {
                    PiUserContent::Text(text) => vec![json!({"type":"input_text","text":text})],
                    PiUserContent::Blocks(blocks) => blocks.iter().map(input_block).collect(),
                };
                if !wire_content.is_empty() {
                    messages.push(json!({"role":"user","content":wire_content}));
                }
            }
            PiMessage::Assistant(message) => {
                let mut text_index = 0_usize;
                for block in &message.content {
                    match block {
                        PiAssistantBlock::Thinking {
                            thinking_signature: Some(signature),
                            ..
                        } => {
                            if let Ok(value) = serde_json::from_str(signature) {
                                messages.push(value);
                            }
                        }
                        PiAssistantBlock::Text {
                            text,
                            text_signature,
                        } => {
                            let id =
                                parse_text_id(text_signature.as_deref()).unwrap_or_else(|| {
                                    if text_index == 0 {
                                        format!("msg_pi_{message_index}")
                                    } else {
                                        format!("msg_pi_{message_index}_{text_index}")
                                    }
                                });
                            text_index += 1;
                            messages.push(json!({
                                "type":"message","role":"assistant","status":"completed","id":id,
                                "content":[{"type":"output_text","text":text,"annotations":[]}]
                            }));
                        }
                        PiAssistantBlock::ToolCall {
                            id,
                            name,
                            arguments,
                            ..
                        } => {
                            let (call_id, item_id) =
                                id.as_str().split_once('|').unwrap_or((id.as_str(), ""));
                            let mut value = Map::from_iter([
                                ("type".to_owned(), Value::String("function_call".to_owned())),
                                ("call_id".to_owned(), Value::String(call_id.to_owned())),
                                ("name".to_owned(), Value::String(name.clone())),
                                (
                                    "arguments".to_owned(),
                                    Value::String(stringify_object(arguments)?),
                                ),
                            ]);
                            if item_id.starts_with("fc_") {
                                value.insert("id".to_owned(), Value::String(item_id.to_owned()));
                            }
                            messages.push(Value::Object(value));
                        }
                        PiAssistantBlock::Thinking { .. } => {}
                    }
                }
            }
            PiMessage::ToolResult(message) => messages.push(tool_result(model, message)),
        }
    }
    Ok(messages)
}

fn input_block(block: &PiUserContentBlock) -> Value {
    match block {
        PiUserContentBlock::Text { text } => json!({"type":"input_text","text":text}),
        PiUserContentBlock::Image { data, mime_type } => json!({
            "type":"input_image","detail":"auto","image_url":format!("data:{mime_type};base64,{data}")
        }),
    }
}

fn tool_result(model: &PiModel, message: &PiToolResultMessage) -> Value {
    let call_id = message
        .tool_call_id
        .as_str()
        .split_once('|')
        .map_or(message.tool_call_id.as_str(), |pair| pair.0);
    let text = message
        .content
        .iter()
        .filter_map(|block| match block {
            PiUserContentBlock::Text { text } => Some(text.as_str()),
            PiUserContentBlock::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let images = message
        .content
        .iter()
        .filter(|block| matches!(block, PiUserContentBlock::Image { .. }))
        .collect::<Vec<_>>();
    let output = if images.is_empty() || !model.input.contains(&PiModality::Image) {
        let output_text = if text.is_empty() {
            if images.is_empty() {
                "(no tool output)".to_owned()
            } else {
                "(see attached image)".to_owned()
            }
        } else {
            text
        };
        Value::String(output_text)
    } else {
        let mut blocks = Vec::new();
        if !text.is_empty() {
            blocks.push(json!({"type":"input_text","text":text}));
        }
        blocks.extend(images.into_iter().map(input_block));
        Value::Array(blocks)
    };
    json!({"type":"function_call_output","call_id":call_id,"output":output})
}

fn request_headers(
    request: &PiExecutionRequest,
    flavor: ResponsesFlavor,
) -> anyhow::Result<HeaderMap> {
    let mut values = Map::<String, Value>::new();
    if let Some(headers) = request
        .model
        .extra
        .get("headers")
        .and_then(Value::as_object)
    {
        values.extend(headers.clone());
    }
    if request.options.cache_retention != Some(PiCacheRetention::None)
        && let Some(session) = &request.options.session_id
    {
        if request.model.provider.as_str() == "openrouter"
            || request.model.base_url.contains("openrouter.ai")
        {
            values.insert(
                "x-session-id".to_owned(),
                Value::String(session.as_str().to_owned()),
            );
        } else {
            values.insert(
                "session_id".to_owned(),
                Value::String(session.as_str().to_owned()),
            );
            values.insert(
                "x-client-request-id".to_owned(),
                Value::String(session.as_str().to_owned()),
            );
        }
    }
    for (name, value) in &request.options.headers {
        values.insert(name.clone(), Value::String(value.clone()));
    }
    if flavor == ResponsesFlavor::Codex {
        let token = request.options.api_key.as_deref().unwrap_or_default();
        values.insert(
            "chatgpt-account-id".to_owned(),
            Value::String(extract_account_id(token)?),
        );
        values.insert("originator".to_owned(), Value::String("pi".to_owned()));
        values.insert(
            "user-agent".to_owned(),
            Value::String(format!(
                "pi ({} {}; {})",
                std::env::consts::OS,
                "rust",
                std::env::consts::ARCH
            )),
        );
        values.insert(
            "openai-beta".to_owned(),
            Value::String("responses=experimental".to_owned()),
        );
        values.insert(
            "accept".to_owned(),
            Value::String("text/event-stream".to_owned()),
        );
        if let Some(session) = &request.options.session_id {
            let session = clamp_cache_key(session.as_str());
            values.insert("session-id".to_owned(), Value::String(session.clone()));
            values.insert("x-client-request-id".to_owned(), Value::String(session));
        }
    }
    let mut headers = HeaderMap::new();
    if let Some(key) = &request.options.api_key {
        if flavor == ResponsesFlavor::Azure {
            headers.insert(
                HeaderName::from_static("api-key"),
                HeaderValue::from_str(key)?,
            );
        } else {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {key}"))?,
            );
        }
    }
    for (name, value) in values {
        let Some(value) = value.as_str() else {
            continue;
        };
        headers.insert(
            HeaderName::from_bytes(name.as_bytes())?,
            HeaderValue::from_str(value)?,
        );
    }
    Ok(headers)
}

fn response_url(base_url: &str, flavor: ResponsesFlavor) -> String {
    let normalized = base_url.trim().trim_end_matches('/');
    match flavor {
        ResponsesFlavor::OpenAi => format!("{normalized}/responses"),
        ResponsesFlavor::Azure => format!("{normalized}/responses?api-version=v1"),
        ResponsesFlavor::Codex if normalized.ends_with("/codex/responses") => normalized.to_owned(),
        ResponsesFlavor::Codex if normalized.ends_with("/codex") => {
            format!("{normalized}/responses")
        }
        ResponsesFlavor::Codex => format!("{normalized}/codex/responses"),
    }
}

fn extract_account_id(token: &str) -> anyhow::Result<String> {
    let mut parts = token.split('.');
    let (_header, payload, _signature) = (parts.next(), parts.next(), parts.next());
    anyhow::ensure!(
        parts.next().is_none(),
        "Failed to extract accountId from token"
    );
    let payload =
        payload.ok_or_else(|| anyhow::anyhow!("Failed to extract accountId from token"))?;
    let padding = match payload.len() % 4 {
        0 => "",
        2 => "==",
        3 => "=",
        _ => anyhow::bail!("Failed to extract accountId from token"),
    };
    let value: Value = serde_json::from_slice(
        &URL_SAFE_NO_PAD
            .decode(format!("{payload}{padding}").trim_end_matches('='))
            .map_err(|_| anyhow::anyhow!("Failed to extract accountId from token"))?,
    )
    .map_err(|_| anyhow::anyhow!("Failed to extract accountId from token"))?;
    value
        .get("https://api.openai.com/auth")
        .and_then(|value| value.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Failed to extract accountId from token"))
}

fn create_slot(
    output: &mut PiAssistantMessage,
    _output_index: u64,
    item: &Value,
) -> Option<(Slot, bool)> {
    let item_type = item.get("type").and_then(Value::as_str)?;
    let content_index = output.content.len();
    let slot = match item_type {
        "reasoning" => {
            output.content.push(PiAssistantBlock::Thinking {
                thinking: String::new(),
                thinking_signature: None,
                redacted: None,
            });
            Slot::Thinking { content_index }
        }
        "message" => {
            output.content.push(PiAssistantBlock::Text {
                text: String::new(),
                text_signature: None,
            });
            Slot::Text { content_index }
        }
        "function_call" => {
            let call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
            output.content.push(PiAssistantBlock::ToolCall {
                id: CallId::new(format!("{call_id}|{item_id}")),
                name: item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                arguments: Map::new(),
                thought_signature: None,
            });
            Slot::ToolCall {
                content_index,
                partial: item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            }
        }
        _ => return None,
    };
    Some((slot, true))
}

fn finalize_response(output: &mut PiAssistantMessage, response: &Value) -> anyhow::Result<()> {
    output.response_id = response
        .get("id")
        .and_then(Value::as_str)
        .map(PiResponseId::new);
    if let Some(usage) = response.get("usage") {
        let input_total = number(usage.get("input_tokens"));
        let cached = number(
            usage
                .get("input_tokens_details")
                .and_then(|value| value.get("cached_tokens")),
        );
        let cache_write = number(
            usage
                .get("input_tokens_details")
                .and_then(|value| value.get("cache_write_tokens")),
        );
        output.usage = PiUsage {
            input: input_total.saturating_sub(cached.saturating_add(cache_write)),
            output: number(usage.get("output_tokens")),
            cache_read: cached,
            cache_write,
            total_tokens: number(usage.get("total_tokens")),
            cost: PiCost::default(),
        };
    }
    output.stop_reason = match response.get("status").and_then(Value::as_str) {
        None | Some("completed" | "in_progress" | "queued") => PiStopReason::Stop,
        Some("incomplete") => PiStopReason::Length,
        Some("failed" | "cancelled") => PiStopReason::Error,
        Some(other) => anyhow::bail!("Unhandled stop reason: {other}"),
    };
    if output.stop_reason == PiStopReason::Stop
        && output
            .content
            .iter()
            .any(|block| matches!(block, PiAssistantBlock::ToolCall { .. }))
    {
        output.stop_reason = PiStopReason::ToolUse;
    }
    Ok(())
}

fn response_message_text(item: &Value) -> String {
    item.get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| {
            part.get("text")
                .or_else(|| part.get("refusal"))
                .and_then(Value::as_str)
        })
        .collect()
}

fn reasoning_text(item: &Value) -> Option<String> {
    for field in ["summary", "content"] {
        let text = item
            .get(field)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n");
        if !text.is_empty() {
            return Some(text);
        }
    }
    None
}

fn thinking_at(output: &PiAssistantMessage, index: usize) -> String {
    match &output.content[index] {
        PiAssistantBlock::Thinking { thinking, .. } => thinking.clone(),
        _ => String::new(),
    }
}

fn text_signature(item: &Value) -> Option<String> {
    let id = item.get("id").and_then(Value::as_str)?;
    let mut value = Map::from_iter([
        ("v".to_owned(), Value::from(1)),
        ("id".to_owned(), Value::String(id.to_owned())),
    ]);
    if let Some(phase @ ("commentary" | "final_answer")) = item.get("phase").and_then(Value::as_str)
    {
        value.insert("phase".to_owned(), Value::String(phase.to_owned()));
    }
    stringify(&Value::Object(value)).ok()
}

fn parse_text_id(signature: Option<&str>) -> Option<String> {
    let signature = signature?;
    if signature.starts_with('{')
        && let Ok(value) = serde_json::from_str::<Value>(signature)
        && value.get("v").and_then(Value::as_u64) == Some(1)
        && let Some(id) = value.get("id").and_then(Value::as_str)
    {
        return Some(id.chars().take(64).collect());
    }
    Some(signature.chars().take(64).collect())
}

fn mapped_effort(model: &PiModel, level: PiThinkingLevel) -> String {
    model
        .thinking_level_map
        .as_ref()
        .and_then(|map| map.get(level.as_str()))
        .and_then(Value::as_str)
        .unwrap_or(level.as_str())
        .to_owned()
}

fn clamp_cache_key(value: &str) -> String {
    value.chars().take(64).collect()
}
fn parse_arguments(raw: &str) -> Map<String, Value> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}
fn set_tool_arguments(block: &mut PiAssistantBlock, raw: &str) {
    if let PiAssistantBlock::ToolCall { arguments, .. } = block {
        *arguments = parse_arguments(raw);
    }
}
fn empty_assistant(model: &PiModel) -> PiAssistantMessage {
    PiAssistantMessage {
        role: PiAssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        usage: PiUsage::default(),
        stop_reason: PiStopReason::Stop,
        error_message: None,
        timestamp: u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default(),
    }
}
fn publish(shared: &Mutex<PiAssistantMessage>, output: &PiAssistantMessage) {
    *shared.lock() = output.clone();
}
fn number(value: Option<&Value>) -> u64 {
    value.and_then(Value::as_u64).unwrap_or_default()
}
fn index_u64(index: usize) -> u64 {
    u64::try_from(index).unwrap_or(u64::MAX)
}
