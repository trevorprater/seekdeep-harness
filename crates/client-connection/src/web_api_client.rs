//! Native Rust web carrier: HTTP upstream plus one WebSocket per downlink.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
    time::Duration,
};

use futures::{SinkExt as _, StreamExt as _, future::BoxFuture, stream::BoxStream};
use parking_lot::Mutex;
use seekdeep_cordis::{Context, fiber::EffectHandle};
use seekdeep_llm::AbortSignal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use url::{Host, Url};
use uuid::Uuid;

use crate::{
    CLIENT_CONNECTION, ClientConnectionHandle, ClientRequest, ConnectionConfig, EventFrame,
    HOST_EVENTS_PATH, HostDescription, HttpMethod, HttpRequest, HttpResponse, HttpTransport,
    HttpTransportFuture, MUX_EVENTS_PATH, RpcId, RpcResult, ServerResponse, StreamApi,
    WebConnectionRpc, is_loopback_hostname,
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const INTERNAL_BASE: &str = "http://seekdeep.internal";

type EnvelopeCallback = Arc<dyn Fn(&[Value]) + Send + Sync>;

/// Deadline policy for one unary HTTP call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryTimeoutPolicy {
    /// Apply the transport-health deadline in addition to caller cancellation.
    Bounded,
    /// Carry caller cancellation only for user-paced native interaction.
    CallerSignalOnly,
}

/// Physical downlink whose second-level payload contract is being applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebApiDownlink {
    /// Session/mux event stream.
    Mux,
    /// Host-wide event stream.
    Host,
}

/// Protocol-contract seam used by the transport-only Connection crate.
///
/// API Proxy owns the concrete method and frame schemas. Keeping this trait in
/// Connection lets that higher layer install its validators without creating a
/// dependency cycle, while generic Connection tests and compositions retain a
/// transport-only identity contract.
pub trait WebApiContract: Send + Sync + 'static {
    /// Parses and normalizes the first-level Host response envelope.
    ///
    /// # Errors
    ///
    /// Returns a contract failure for a malformed envelope or business error.
    fn parse_server_response(&self, value: &Value) -> anyhow::Result<ServerResponse>;

    /// Parses the method-specific value after envelope observation and rpc-id correlation.
    ///
    /// `None` represents an omitted success value. Concrete API methods normally
    /// reject it, while the transport-only identity contract preserves it.
    ///
    /// # Errors
    ///
    /// Returns a method-specific response-value contract failure.
    fn parse_unary_success_value(
        &self,
        method: &str,
        value: Option<&Value>,
    ) -> anyhow::Result<Option<Value>>;

    /// Parses and normalizes a second-level stream frame payload.
    ///
    /// # Errors
    ///
    /// Returns a stream-specific frame contract failure.
    fn parse_downlink_payload(
        &self,
        downlink: WebApiDownlink,
        payload: &Value,
    ) -> anyhow::Result<Value>;
}

#[derive(Debug)]
struct TransportOnlyContract;

impl WebApiContract for TransportOnlyContract {
    fn parse_server_response(&self, value: &Value) -> anyhow::Result<ServerResponse> {
        let response: ServerResponse = serde_json::from_value(value.clone())?;
        anyhow::ensure!(
            response.kind == "server-response",
            "invalid server-response type"
        );
        Ok(response)
    }

    fn parse_unary_success_value(
        &self,
        _method: &str,
        value: Option<&Value>,
    ) -> anyhow::Result<Option<Value>> {
        Ok(value.cloned())
    }

    fn parse_downlink_payload(
        &self,
        _downlink: WebApiDownlink,
        payload: &Value,
    ) -> anyhow::Result<Value> {
        anyhow::ensure!(
            payload
                .as_object()
                .and_then(|payload| payload.get("type"))
                .and_then(Value::as_str)
                .is_some(),
            "downlink payload has no string type"
        );
        Ok(payload.clone())
    }
}

struct EnvelopeListener {
    id: Uuid,
    callback: EnvelopeCallback,
}

#[derive(Default)]
struct EnvelopeState {
    buffer: Vec<Value>,
    flush_scheduled: bool,
    listeners: Vec<EnvelopeListener>,
}

/// Idempotent disposer for one envelope observer.
pub struct EnvelopeSubscription {
    state: std::sync::Weak<Mutex<EnvelopeState>>,
    id: Uuid,
}

impl std::fmt::Debug for EnvelopeSubscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EnvelopeSubscription")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl EnvelopeSubscription {
    /// Removes this exact observer without disturbing later registrations.
    pub fn dispose(&self) {
        if let Some(state) = self.state.upgrade() {
            state
                .lock()
                .listeners
                .retain(|listener| listener.id != self.id);
        }
    }
}

/// Real Rust Client transport corresponding to the browser `WebApiClient`.
#[derive(Clone)]
pub struct WebApiClient {
    base: Url,
    http: reqwest::Client,
    timeout: Duration,
    contract: Arc<dyn WebApiContract>,
    envelopes: Arc<Mutex<EnvelopeState>>,
}

impl std::fmt::Debug for WebApiClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebApiClient")
            .field("base", &self.base)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl WebApiClient {
    /// Creates a same-origin-style carrier for one HTTP(S) base URL.
    ///
    /// `None` selects the renamed non-network internal authority used by
    /// non-browser/in-process compositions.
    ///
    /// # Errors
    ///
    /// Rejects malformed bases and non-HTTP schemes.
    pub fn new(base: Option<&str>) -> anyhow::Result<Arc<Self>> {
        Self::with_timeout(base, DEFAULT_TIMEOUT)
    }

    /// Creates a carrier with an explicit bounded-unary timeout.
    ///
    /// # Errors
    ///
    /// Rejects malformed bases and non-HTTP schemes.
    pub fn with_timeout(base: Option<&str>, timeout: Duration) -> anyhow::Result<Arc<Self>> {
        Self::with_timeout_and_contract(base, timeout, Arc::new(TransportOnlyContract))
    }

    /// Creates a carrier whose two-level protocol parsing is owned by `contract`.
    ///
    /// # Errors
    ///
    /// Rejects malformed bases and non-HTTP schemes.
    pub fn with_contract(
        base: Option<&str>,
        contract: Arc<dyn WebApiContract>,
    ) -> anyhow::Result<Arc<Self>> {
        Self::with_timeout_and_contract(base, DEFAULT_TIMEOUT, contract)
    }

    /// Creates a contracted carrier with an explicit bounded-unary timeout.
    ///
    /// # Errors
    ///
    /// Rejects malformed bases and non-HTTP schemes.
    pub fn with_timeout_and_contract(
        base: Option<&str>,
        timeout: Duration,
        contract: Arc<dyn WebApiContract>,
    ) -> anyhow::Result<Arc<Self>> {
        let mut base = Url::parse(base.unwrap_or(INTERNAL_BASE))?;
        anyhow::ensure!(
            matches!(base.scheme(), "http" | "https"),
            "client-connection: web base must use HTTP or HTTPS"
        );
        anyhow::ensure!(
            base.host().is_some(),
            "client-connection: web base has no host"
        );
        base.set_path("/");
        base.set_query(None);
        base.set_fragment(None);
        Ok(Arc::new(Self {
            base,
            http: reqwest::Client::new(),
            timeout,
            contract,
            envelopes: Arc::new(Mutex::new(EnvelopeState::default())),
        }))
    }

    /// Builds the public Connection handle over this physical client.
    #[must_use]
    pub fn connection_handle(self: &Arc<Self>) -> Arc<ClientConnectionHandle> {
        let transport: Arc<dyn HttpTransport> = self.clone();
        let caller = WebConnectionRpc::new(transport);
        let streams: Arc<dyn StreamApi> = self.clone();
        ClientConnectionHandle::with_streams(caller, streams, self.is_loopback())
    }

    /// Provides a complete Client Connection in the calling Cordis fiber.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> anyhow::Result<EffectHandle> {
        Ok(context.provide(CLIENT_CONNECTION, self.connection_handle())?)
    }

    /// Whether the configured page/base authority is loopback.
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        self.base.host().is_some_and(|host| {
            let hostname = match host {
                Host::Domain(domain) => domain.to_ascii_lowercase(),
                Host::Ipv4(address) => address.to_string(),
                Host::Ipv6(address) => format!("[{address}]"),
            };
            is_loopback_hostname(&hostname)
        })
    }

    /// Resolves one HTTP API path to its `ws:` or `wss:` downlink URL.
    ///
    /// # Errors
    ///
    /// Returns URL resolution or scheme-mapping failures.
    pub fn downlink_url(&self, path: &str) -> anyhow::Result<Url> {
        self.socket_url(path)
    }

    /// Subscribes to microtask-like batches of complete carrier envelopes.
    #[must_use]
    pub fn subscribe_envelopes(&self, callback: EnvelopeCallback) -> EnvelopeSubscription {
        let id = Uuid::now_v7();
        self.envelopes
            .lock()
            .listeners
            .push(EnvelopeListener { id, callback });
        EnvelopeSubscription {
            state: Arc::downgrade(&self.envelopes),
            id,
        }
    }

    /// Calls one API Proxy unary method with the carrier-owned full envelope.
    pub fn call_unary(
        &self,
        method: impl Into<String>,
        payload: Value,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<ServerResponse>> {
        self.call_unary_with_policy(method, payload, signal, UnaryTimeoutPolicy::Bounded)
    }

    /// Calls one API Proxy unary method with an explicit deadline policy.
    pub fn call_unary_with_policy(
        &self,
        method: impl Into<String>,
        payload: Value,
        signal: AbortSignal,
        timeout_policy: UnaryTimeoutPolicy,
    ) -> BoxFuture<'static, anyhow::Result<ServerResponse>> {
        let method = method.into();
        let client = self.clone();
        Box::pin(async move {
            let rpc_id = RpcId::new(Uuid::new_v4().to_string());
            let message = ClientRequest::new(rpc_id.clone(), method.clone(), payload);
            client.observe(serde_json::to_value(&message)?);
            let path = format!("/api/{method}");
            let response = client
                .post_json(&path, serde_json::to_vec(&message)?, signal, timeout_policy)
                .await?;
            let wire: Value = serde_json::from_slice(&response.body)?;
            let mut full = client.contract.parse_server_response(&wire)?;
            client.observe(serde_json::to_value(&full)?);
            anyhow::ensure!(
                full.rpc_id == rpc_id,
                "rpcId mismatch for {method}: sent {rpc_id}, got {}",
                full.rpc_id
            );
            if let RpcResult::Success { value } = &full.result {
                full.result = RpcResult::Success {
                    value: client
                        .contract
                        .parse_unary_success_value(&method, value.as_ref())?,
                };
            }
            Ok(full)
        })
    }

    /// Sends one Client response without minting a correlation id.
    ///
    /// The caller owns the full-form response schema and receipt parsing;
    /// Connection owns only JSON POST, timeout, and outgoing observation.
    pub fn respond_raw(
        &self,
        message: Value,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<Value>> {
        let client = self.clone();
        Box::pin(async move {
            client.observe(message.clone());
            let response = client
                .post_json(
                    "/api/respond",
                    serde_json::to_vec(&message)?,
                    signal,
                    UnaryTimeoutPolicy::Bounded,
                )
                .await?;
            Ok(serde_json::from_slice(&response.body)?)
        })
    }

    async fn post_json(
        &self,
        path: &str,
        body: Vec<u8>,
        signal: AbortSignal,
        timeout_policy: UnaryTimeoutPolicy,
    ) -> anyhow::Result<HttpResponse> {
        let mut request = HttpRequest::new(HttpMethod::Post, path);
        request
            .headers
            .insert("content-type".to_owned(), "application/json".to_owned());
        request.body = body;
        let response = match timeout_policy {
            UnaryTimeoutPolicy::Bounded => {
                let timeout_signal = AbortSignal::default();
                request.signal = AbortSignal::fuse(&signal, &timeout_signal);
                tokio::select! {
                    () = tokio::time::sleep(self.timeout) => {
                        timeout_signal.abort();
                        anyhow::bail!("transport failure for {path}: timed out");
                    }
                    response = self.fetch(request) => response?,
                }
            }
            UnaryTimeoutPolicy::CallerSignalOnly => {
                request.signal = signal;
                self.fetch(request).await?
            }
        };
        anyhow::ensure!(
            (200..300).contains(&response.status),
            "transport failure for {path}: HTTP {}",
            response.status
        );
        Ok(response)
    }

    fn socket_url(&self, path: &str) -> anyhow::Result<Url> {
        let mut url = self.base.join(path)?;
        let websocket_scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(websocket_scheme)
            .map_err(|()| anyhow::anyhow!("cannot map web base to WebSocket scheme"))?;
        Ok(url)
    }

    fn open_downlink(
        &self,
        path: &'static str,
        downlink: WebApiDownlink,
        signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>> {
        let url = self.socket_url(path);
        let observer = self.clone();
        let contract = self.contract.clone();
        Box::pin(async_stream::try_stream! {
            let url = url?;
            if signal.is_aborted() {
                return;
            }
            let connecting = connect_async(url.as_str());
            let connected = tokio::select! {
                () = signal.cancelled() => None,
                connected = connecting => Some(connected),
            };
            let Some(connected) = connected else { return };
            let (mut socket, _) = connected?;
            let _ = catch_unwind(AssertUnwindSafe(|| on_open()));
            loop {
                let message = tokio::select! {
                    () = signal.cancelled() => {
                        let _ = socket.send(Message::Close(None)).await;
                        return;
                    }
                    message = socket.next() => message,
                };
                let Some(message) = message else {
                    return;
                };
                match message {
                    Ok(Message::Text(text)) => {
                        match parse_server_request(text.as_str(), downlink, contract.as_ref()) {
                            Ok((full, frame)) => {
                                observer.observe(full);
                                yield frame;
                            }
                            Err(error) => {
                                tracing::error!(%error, path, "client-connection: dropping malformed WebSocket frame");
                            }
                        }
                    }
                    Ok(Message::Binary(_)) => {
                        tracing::error!(path, "client-connection: dropping binary WebSocket frame");
                    }
                    Ok(Message::Ping(payload)) => {
                        socket.send(Message::Pong(payload)).await?;
                    }
                    Ok(Message::Pong(_) | Message::Frame(_)) => {}
                    Ok(Message::Close(_)) | Err(_) => return,
                }
            }
        })
    }

    fn observe(&self, envelope: Value) {
        let schedule = {
            let mut state = self.envelopes.lock();
            if state.listeners.is_empty() {
                return;
            }
            state.buffer.push(envelope);
            if state.flush_scheduled {
                false
            } else {
                state.flush_scheduled = true;
                true
            }
        };
        if !schedule {
            return;
        }
        let state = self.envelopes.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            let (batch, listeners) = {
                let mut state = state.lock();
                state.flush_scheduled = false;
                let batch = std::mem::take(&mut state.buffer);
                let listeners = state
                    .listeners
                    .iter()
                    .map(|listener| listener.callback.clone())
                    .collect::<Vec<_>>();
                (batch, listeners)
            };
            for listener in listeners {
                let _ = catch_unwind(AssertUnwindSafe(|| listener(&batch)));
            }
        });
    }
}

impl HttpTransport for WebApiClient {
    fn fetch(&self, request: HttpRequest) -> HttpTransportFuture {
        let client = self.clone();
        Box::pin(async move {
            let mut url = client.base.join(&request.path)?;
            url.set_query(request.query.as_deref());
            let method = match request.method {
                HttpMethod::Get => reqwest::Method::GET,
                HttpMethod::Post => reqwest::Method::POST,
                HttpMethod::Other(method) => reqwest::Method::from_bytes(method.as_bytes())?,
            };
            let mut builder = client.http.request(method, url);
            for (name, value) in request.headers {
                builder = builder.header(name, value);
            }
            if !request.body.is_empty() {
                builder = builder.body(request.body);
            }
            if request.signal.is_aborted() {
                anyhow::bail!("This operation was aborted");
            }
            let response = tokio::select! {
                () = request.signal.cancelled() => anyhow::bail!("This operation was aborted"),
                response = builder.send() => response?,
            };
            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .filter_map(|(name, value)| {
                    value
                        .to_str()
                        .ok()
                        .map(|value| (name.as_str().to_owned(), value.to_owned()))
                })
                .collect();
            let body = tokio::select! {
                () = request.signal.cancelled() => anyhow::bail!("This operation was aborted"),
                body = response.bytes() => body?.to_vec(),
            };
            Ok(HttpResponse {
                status,
                headers,
                body,
                body_stream: None,
            })
        })
    }
}

impl StreamApi for WebApiClient {
    fn describe(&self) -> BoxFuture<'static, anyhow::Result<RpcResult<HostDescription>>> {
        let call = self.call_unary(
            "host.describe",
            serde_json::json!({}),
            AbortSignal::default(),
        );
        Box::pin(async move { Ok(call.await?.result) })
    }

    fn mux(
        &self,
        signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>> {
        self.open_downlink(MUX_EVENTS_PATH, WebApiDownlink::Mux, signal, on_open)
    }

    fn host(
        &self,
        signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>> {
        self.open_downlink(HOST_EVENTS_PATH, WebApiDownlink::Host, signal, on_open)
    }
}

#[derive(Deserialize, Serialize)]
struct WireServerRequest {
    #[serde(rename = "type")]
    kind: String,
    #[serde(rename = "rpcId")]
    rpc_id: RpcId,
    method: String,
    payload: Value,
}

fn parse_server_request(
    text: &str,
    downlink: WebApiDownlink,
    contract: &dyn WebApiContract,
) -> anyhow::Result<(Value, EventFrame)> {
    let full: WireServerRequest = serde_json::from_str(text)?;
    anyhow::ensure!(full.kind == "server-request", "invalid server-request type");
    let payload = contract.parse_downlink_payload(downlink, &full.payload)?;
    let envelope = serde_json::to_value(&full)?;
    Ok((
        envelope,
        EventFrame {
            rpc_id: full.rpc_id,
            payload,
        },
    ))
}

/// Installs the real Client transport at a supplied page/base URL.
///
/// # Errors
///
/// Rejects an invalid base or Cordis service activation failure.
pub fn install_client(context: &Context, base: Option<&str>) -> anyhow::Result<Arc<WebApiClient>> {
    let client = WebApiClient::new(base)?;
    client.provide(context)?;
    Ok(client)
}

/// Default reconnect configuration exported beside the concrete client.
#[must_use]
pub fn default_connection_config() -> ConnectionConfig {
    ConnectionConfig::default()
}
