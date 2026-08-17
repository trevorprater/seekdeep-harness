//! Generic unary RPC contracts and the Host/Client Connection registries.

use std::{
    collections::HashMap,
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Weak},
};

use futures::{Stream, future::BoxFuture, stream::BoxStream};
use parking_lot::Mutex;
use seekdeep_cordis::{Context, ServiceKey, fiber::EffectHandle};
use seekdeep_host_webserver::{WebHandler, WebHandlerFuture, WebRoute, WebRouteKind, WebServer};
use seekdeep_llm::AbortSignal;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::{
    API_PATH, ConnectionConfig, ConnectionController, ConnectionSinks, ConnectionState,
    HostDescription, StreamApi, trust::is_trusted_api_request,
};

/// Typed Cordis slot corresponding to Client `ctx.connection`.
pub const CLIENT_CONNECTION: ServiceKey<ClientConnectionHandle> = ServiceKey::new("connection");
/// Typed Cordis slot corresponding to Host `ctx.connection`.
pub const HOST_CONNECTION: ServiceKey<HostConnectionService> = ServiceKey::new("connection");

/// Opaque correlation identifier minted by the request initiator.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RpcId(String);

impl RpcId {
    /// Brands an arbitrary string without runtime validation, matching the source contract.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the opaque identifier string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RpcId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Structured RPC business error. Unknown codes and details remain lossless.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    /// Closed in the pinned source schema, retained as text for forward-compatible diagnostics.
    pub code: String,
    /// Human-readable seam-owned message.
    pub message: String,
    /// Code-specific details object.
    pub details: Map<String, Value>,
}

/// Business success/failure result, preserving omitted success values distinctly from JSON null.
#[derive(Clone, Debug, PartialEq)]
pub enum RpcResult<T> {
    /// Successful optional business value.
    Success {
        /// `None` serializes without a `value` property; `Some(null)` remains explicit null.
        value: Option<T>,
    },
    /// Structured business failure.
    Failure {
        /// Stable carrier error.
        error: RpcError,
    },
}

impl<T: Serialize> Serialize for RpcResult<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = match self {
            Self::Success { value: Some(value) } => {
                json!({ "ok": true, "value": value })
            }
            Self::Success { value: None } => json!({ "ok": true }),
            Self::Failure { error } => json!({ "ok": false, "error": error }),
        };
        value.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for RpcResult<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut value = Value::deserialize(deserializer)?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| serde::de::Error::custom("RPC result must be an object"))?;
        match object.remove("ok") {
            Some(Value::Bool(true)) => {
                let value = object
                    .remove("value")
                    .map(T::deserialize)
                    .transpose()
                    .map_err(serde::de::Error::custom)?;
                Ok(Self::Success { value })
            }
            Some(Value::Bool(false)) => {
                let error = object
                    .remove("error")
                    .ok_or_else(|| serde::de::Error::missing_field("error"))?;
                Ok(Self::Failure {
                    error: serde_json::from_value(error).map_err(serde::de::Error::custom)?,
                })
            }
            _ => Err(serde::de::Error::custom(
                "RPC result requires boolean ok discriminant",
            )),
        }
    }
}

/// Call initiated by the Client.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClientRequest {
    /// Must be `client-request` on the wire.
    #[serde(rename = "type")]
    pub kind: String,
    /// Client-minted correlation id.
    #[serde(rename = "rpcId")]
    pub rpc_id: RpcId,
    /// Channel-relative endpoint.
    pub method: String,
    /// Method-specific second-level payload.
    pub payload: Value,
}

impl ClientRequest {
    /// Creates the exact full-form wire envelope.
    #[must_use]
    pub fn new(rpc_id: RpcId, method: impl Into<String>, payload: Value) -> Self {
        Self {
            kind: "client-request".to_owned(),
            rpc_id,
            method: method.into(),
            payload,
        }
    }

    fn validate(self) -> anyhow::Result<Self> {
        anyhow::ensure!(self.kind == "client-request", "invalid client-request type");
        Ok(self)
    }
}

/// Response to a [`ClientRequest`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerResponse<T = Value> {
    /// Must be `server-response` on the wire.
    #[serde(rename = "type")]
    pub kind: String,
    /// Echoed request id.
    #[serde(rename = "rpcId")]
    pub rpc_id: RpcId,
    /// Business result.
    pub result: RpcResult<T>,
}

impl<T> ServerResponse<T> {
    /// Creates one correlated full-form response.
    #[must_use]
    pub fn new(rpc_id: RpcId, result: RpcResult<T>) -> Self {
        Self {
            kind: "server-response".to_owned(),
            rpc_id,
            result,
        }
    }
}

/// Unwraps the business result slot from one unary response.
#[must_use]
pub fn result_of<T>(response: ServerResponse<T>) -> RpcResult<T> {
    response.result
}

/// Folds one transport exception/message into the shared internal-error branch.
pub fn transport_error<T>(error: &(impl ToString + ?Sized)) -> RpcResult<T> {
    RpcResult::Failure {
        error: RpcError {
            code: "internal".to_owned(),
            message: error.to_string(),
            details: Map::new(),
        },
    }
}

impl<T> ServerResponse<T> {
    fn validate(self) -> anyhow::Result<Self> {
        anyhow::ensure!(
            self.kind == "server-response",
            "invalid server-response type"
        );
        Ok(self)
    }
}

/// Minimal HTTP method vocabulary used by the transport-independent bridge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HttpMethod {
    /// GET.
    Get,
    /// POST.
    Post,
    /// Any method retained losslessly for a 404 boundary response.
    Other(String),
}

/// Transport-independent request consumed by Connection RPC dispatch.
#[derive(Clone, Debug)]
pub struct HttpRequest {
    /// HTTP method.
    pub method: HttpMethod,
    /// Absolute path without scheme or authority.
    pub path: String,
    /// Raw query string without the leading `?`, retained separately so route
    /// matching remains pathname-only.
    pub query: Option<String>,
    /// Case-insensitive HTTP headers represented as received.
    pub headers: HashMap<String, String>,
    /// Fully buffered request body.
    pub body: Vec<u8>,
    /// Client-disconnect cancellation signal.
    pub signal: AbortSignal,
}

impl HttpRequest {
    /// Creates a request with a fresh non-aborted signal.
    #[must_use]
    pub fn new(method: HttpMethod, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            query: None,
            headers: HashMap::new(),
            body: Vec::new(),
            signal: AbortSignal::default(),
        }
    }

    /// Returns one case-insensitive header value.
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Pull-driven transport-independent response body.
///
/// Dropping a body before it completes aborts `consumer_signal`, allowing a
/// producer to stop compression, storage reads, or other downstream work.
pub struct HttpResponseStream {
    inner: BoxStream<'static, anyhow::Result<Vec<u8>>>,
    consumer_signal: AbortSignal,
    completed: bool,
}

impl std::fmt::Debug for HttpResponseStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpResponseStream")
            .field("completed", &self.completed)
            .finish_non_exhaustive()
    }
}

impl HttpResponseStream {
    /// Wraps one byte stream with a consumer-cancellation signal.
    #[must_use]
    pub fn new(
        inner: BoxStream<'static, anyhow::Result<Vec<u8>>>,
        consumer_signal: AbortSignal,
    ) -> Self {
        Self {
            inner,
            consumer_signal,
            completed: false,
        }
    }

    /// Signal that becomes aborted when this body is dropped before completion.
    #[must_use]
    pub fn consumer_signal(&self) -> AbortSignal {
        self.consumer_signal.clone()
    }

    /// Cancels the producer with the caller-supplied reason.
    pub fn cancel_with_reason(&self, reason: Value) {
        self.consumer_signal.abort_with_reason(reason);
    }
}

impl Stream for HttpResponseStream {
    type Item = anyhow::Result<Vec<u8>>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.inner.as_mut().poll_next(context) {
            std::task::Poll::Ready(item) => {
                if item.as_ref().is_none_or(Result::is_err) {
                    self.completed = true;
                }
                std::task::Poll::Ready(item)
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

impl Drop for HttpResponseStream {
    fn drop(&mut self) {
        if !self.completed && !self.consumer_signal.is_aborted() {
            self.consumer_signal.abort_with_reason(Value::String(
                "session log export stream cancelled".to_owned(),
            ));
        }
    }
}

/// Complete transport-independent HTTP response.
pub struct HttpResponse {
    /// HTTP status.
    pub status: u16,
    /// Response headers.
    pub headers: HashMap<String, String>,
    /// Complete response body.
    pub body: Vec<u8>,
    /// Optional pull-driven body. When present, `body` is an empty prefix.
    pub body_stream: Option<HttpResponseStream>,
}

impl std::fmt::Debug for HttpResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("body", &self.body)
            .field("streaming", &self.body_stream.is_some())
            .finish()
    }
}

impl PartialEq for HttpResponse {
    fn eq(&self, other: &Self) -> bool {
        self.status == other.status
            && self.headers == other.headers
            && self.body == other.body
            && self.body_stream.is_none()
            && other.body_stream.is_none()
    }
}

impl Eq for HttpResponse {}

impl HttpResponse {
    /// Creates a UTF-8 text response.
    #[must_use]
    pub fn text(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: HashMap::new(),
            body: body.into().into_bytes(),
            body_stream: None,
        }
    }

    /// Creates a JSON response.
    fn json<T: Serialize>(status: u16, body: &T) -> Self {
        let mut headers = HashMap::new();
        headers.insert("content-type".to_owned(), "application/json".to_owned());
        Self {
            status,
            headers,
            body: serde_json::to_vec(body).expect("wire values must serialize"),
            body_stream: None,
        }
    }
}

/// Browser authority accepted by one logical RPC channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionRpcAuthority {
    /// Loopback or a configured deployment authority.
    TrustedHost,
    /// Loopback only.
    Loopback,
}

/// Future returned by one Host RPC handler.
pub type RpcHandlerFuture = BoxFuture<'static, anyhow::Result<RpcResult<Value>>>;
/// Decoded unary handler after Connection validates the carrier envelope.
pub type RpcHandler =
    Arc<dyn Fn(String, Value, AbortSignal) -> RpcHandlerFuture + Send + Sync + 'static>;
/// Synchronous ownership test for one endpoint on a shared channel.
pub type EndpointMatcher = Arc<dyn Fn(&str) -> bool + Send + Sync + 'static>;

#[derive(Clone)]
struct Registration {
    id: Uuid,
    handler: RpcHandler,
    authority: ConnectionRpcAuthority,
    matcher: Option<EndpointMatcher>,
}

#[derive(Default)]
struct HostState {
    dedicated: HashMap<String, Registration>,
    interceptors: HashMap<String, Registration>,
}

/// Host registry for dedicated and shared logical RPC channels.
pub struct HostConnectionService {
    trusted_hosts: Vec<String>,
    state: Arc<Mutex<HostState>>,
    self_weak: Weak<Self>,
    webserver: Mutex<Option<Arc<WebServer>>>,
}

/// Lifecycle lease for the singleton shared-channel interceptor.
///
/// Withdrawal is synchronous so dependency reconciliation can make the old
/// Connection instance unreachable before a replacement is installed; the
/// asynchronous disposer remains idempotent and joins ordinary fiber cleanup.
#[derive(Clone)]
pub struct SharedRpcRegistration {
    effect: EffectHandle,
    state: Arc<Mutex<HostState>>,
    channel: String,
    id: Uuid,
}

impl fmt::Debug for SharedRpcRegistration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SharedRpcRegistration")
            .field("channel", &self.channel)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl SharedRpcRegistration {
    /// Immediately withdraws this exact claim without touching a replacement.
    pub fn withdraw(&self) {
        let mut state = self.state.lock();
        if state
            .interceptors
            .get(&self.channel)
            .is_some_and(|registration| registration.id == self.id)
        {
            state.interceptors.remove(&self.channel);
        }
    }

    /// Withdraws and joins the fiber-owned disposer.
    ///
    /// # Errors
    ///
    /// Returns any effect disposal failure.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        self.withdraw();
        self.effect.dispose().await
    }
}

impl fmt::Debug for HostConnectionService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.lock();
        formatter
            .debug_struct("HostConnectionService")
            .field("trusted_hosts", &self.trusted_hosts)
            .field("dedicated", &state.dedicated.keys().collect::<Vec<_>>())
            .field(
                "interceptors",
                &state.interceptors.keys().collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl HostConnectionService {
    /// Creates a Host registry after validating every configured authority.
    ///
    /// # Errors
    ///
    /// Returns the canonical load-time trusted-host validation error.
    pub fn new(trusted_hosts: Vec<String>) -> anyhow::Result<Arc<Self>> {
        for entry in &trusted_hosts {
            crate::assert_trusted_authority(entry)?;
        }
        Ok(Arc::new_cyclic(|self_weak| Self {
            trusted_hosts,
            state: Arc::new(Mutex::new(HostState::default())),
            self_weak: self_weak.clone(),
            webserver: Mutex::new(None),
        }))
    }

    /// Attaches the physical Webserver used by dedicated channel registrations.
    ///
    /// # Errors
    ///
    /// Rejects a second, different physical server.
    pub(crate) fn attach_webserver(&self, webserver: Arc<WebServer>) -> anyhow::Result<()> {
        let mut slot = self.webserver.lock();
        anyhow::ensure!(slot.is_none(), "connection: Webserver is already attached");
        *slot = Some(webserver);
        Ok(())
    }

    /// Provides this registry as Host `ctx.connection`.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-owner failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> anyhow::Result<EffectHandle> {
        Ok(context.provide(HOST_CONNECTION, self.clone())?)
    }

    /// Registers one dedicated absolute channel in the calling fiber.
    ///
    /// # Errors
    ///
    /// Rejects malformed/reserved or duplicate channels and inactive owners.
    pub fn handle(
        &self,
        owner: &Context,
        channel: &str,
        handler: RpcHandler,
        authority: ConnectionRpcAuthority,
    ) -> anyhow::Result<EffectHandle> {
        assert_channel(channel)?;
        let id = Uuid::now_v7();
        let web_registration = if let Some(webserver) = self.webserver.lock().clone() {
            let connection = self
                .self_weak
                .upgrade()
                .ok_or_else(|| anyhow::anyhow!("connection service is no longer live"))?;
            let channel_owned = channel.to_owned();
            let route_handler: WebHandler = Arc::new(move |request| {
                let connection = connection.clone();
                let channel = channel_owned.clone();
                Box::pin(async move {
                    crate::host::dispatch_dedicated(connection, channel, request, authority).await
                }) as WebHandlerFuture
            });
            Some(webserver.register(WebRoute {
                kind: WebRouteKind::Prefix,
                path: channel.to_owned(),
                handler: route_handler,
            })?)
        } else {
            None
        };
        {
            let mut state = self.state.lock();
            if state.dedicated.contains_key(channel) {
                if let Some(registration) = &web_registration {
                    registration.dispose();
                }
                anyhow::bail!("connection: RPC channel {channel:?} is already registered");
            }
            state.dedicated.insert(
                channel.to_owned(),
                Registration {
                    id,
                    handler,
                    authority,
                    matcher: None,
                },
            );
        }
        let state = self.state.clone();
        let channel_owned = channel.to_owned();
        let web_registration = Arc::new(Mutex::new(web_registration));
        let cleanup_registration = web_registration.clone();
        let effect = EffectHandle::synchronous(
            format!("client-connection: {channel} rpc channel"),
            move || {
                let mut state = state.lock();
                if state
                    .dedicated
                    .get(&channel_owned)
                    .is_some_and(|registration| registration.id == id)
                {
                    state.dedicated.remove(&channel_owned);
                }
                if let Some(registration) = cleanup_registration.lock().take() {
                    registration.dispose();
                }
                Ok(())
            },
        );
        if let Err(error) = owner.own(effect.clone()) {
            self.state.lock().dedicated.remove(channel);
            if let Some(registration) = web_registration.lock().take() {
                registration.dispose();
            }
            return Err(error.into());
        }
        Ok(effect)
    }

    /// Intercepts owned endpoints on the shared `/api` channel.
    ///
    /// # Errors
    ///
    /// Rejects non-API or duplicate shared registrations and inactive owners.
    pub fn intercept(
        &self,
        owner: &Context,
        channel: &str,
        matcher: EndpointMatcher,
        handler: RpcHandler,
        authority: ConnectionRpcAuthority,
    ) -> anyhow::Result<SharedRpcRegistration> {
        anyhow::ensure!(
            channel == API_PATH,
            "connection: invalid shared RPC channel {channel:?}"
        );
        let id = Uuid::now_v7();
        {
            let mut state = self.state.lock();
            anyhow::ensure!(
                !state.interceptors.contains_key(channel),
                "connection: shared RPC channel {channel:?} already has an interceptor"
            );
            state.interceptors.insert(
                channel.to_owned(),
                Registration {
                    id,
                    handler,
                    authority,
                    matcher: Some(matcher),
                },
            );
        }
        let state = self.state.clone();
        let channel_owned = channel.to_owned();
        let effect = EffectHandle::synchronous(
            format!("client-connection: {channel} rpc interceptor"),
            move || {
                let mut state = state.lock();
                if state
                    .interceptors
                    .get(&channel_owned)
                    .is_some_and(|registration| registration.id == id)
                {
                    state.interceptors.remove(&channel_owned);
                }
                Ok(())
            },
        );
        if let Err(error) = owner.own(effect.clone()) {
            self.state.lock().interceptors.remove(channel);
            return Err(error.into());
        }
        Ok(SharedRpcRegistration {
            effect,
            state: self.state.clone(),
            channel: channel.to_owned(),
            id,
        })
    }

    /// Dispatches a request to one registered dedicated channel.
    pub async fn dispatch(&self, channel: &str, request: HttpRequest) -> HttpResponse {
        let registration = self.state.lock().dedicated.get(channel).cloned();
        let Some(registration) = registration else {
            return HttpResponse::text(404, "not found");
        };
        if !self.trusted(&request, registration.authority) {
            return HttpResponse::text(403, "forbidden");
        }
        rpc_fetch(channel, registration.handler, request).await
    }

    /// Dispatches a shared-channel request to its claim or the supplied fallback.
    pub async fn dispatch_shared<F, Fut>(
        &self,
        channel: &str,
        request: HttpRequest,
        fallback: F,
    ) -> HttpResponse
    where
        F: FnOnce(HttpRequest) -> Fut,
        Fut: Future<Output = HttpResponse>,
    {
        let endpoint = endpoint_from_path(channel, &request.path);
        let registration = self.state.lock().interceptors.get(channel).cloned();
        let claimed = endpoint.as_deref().is_some_and(|endpoint| {
            registration
                .as_ref()
                .and_then(|registration| registration.matcher.as_ref())
                .is_some_and(|matcher| matcher(endpoint))
        });
        if !claimed {
            return fallback(request).await;
        }
        let Some(registration) = registration else {
            return fallback(request).await;
        };
        if !self.trusted(&request, registration.authority) {
            return HttpResponse::text(403, "forbidden");
        }
        rpc_fetch(channel, registration.handler, request).await
    }

    fn trusted(&self, request: &HttpRequest, authority: ConnectionRpcAuthority) -> bool {
        let trusted_hosts: &[String] = match authority {
            ConnectionRpcAuthority::TrustedHost => &self.trusted_hosts,
            ConnectionRpcAuthority::Loopback => &[],
        };
        is_trusted_api_request(&request.headers, trusted_hosts)
    }

    pub(crate) fn trusted_headers(
        &self,
        headers: &HashMap<String, String>,
        authority: ConnectionRpcAuthority,
    ) -> bool {
        let trusted_hosts: &[String] = match authority {
            ConnectionRpcAuthority::TrustedHost => &self.trusted_hosts,
            ConnectionRpcAuthority::Loopback => &[],
        };
        is_trusted_api_request(headers, trusted_hosts)
    }
}

async fn rpc_fetch(channel: &str, handler: RpcHandler, request: HttpRequest) -> HttpResponse {
    let Some(endpoint) = endpoint_from_path(channel, &request.path) else {
        return HttpResponse::text(404, "not found");
    };
    if request.method != HttpMethod::Post {
        return HttpResponse::text(404, "not found");
    }
    let media_type = request
        .header("content-type")
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    if media_type.as_deref() != Some("application/json") {
        return HttpResponse::text(415, "content type must be application/json");
    }
    let body: Value = match serde_json::from_slice(&request.body) {
        Ok(body) => body,
        Err(_) => return HttpResponse::text(400, "body is not JSON"),
    };
    let raw_id = body
        .as_object()
        .and_then(|object| object.get("rpcId"))
        .and_then(Value::as_str)
        .map_or_else(|| RpcId::new("invalid-request"), RpcId::new);
    let Some(message) = serde_json::from_value::<ClientRequest>(body)
        .ok()
        .and_then(|value| value.validate().ok())
    else {
        return error_response(
            raw_id,
            RpcError {
                code: "bad-request".to_owned(),
                message: "invalid client-request message".to_owned(),
                details: json!({ "issues": [] }).as_object().expect("object").clone(),
            },
        );
    };
    if message.method != endpoint {
        return error_response(
            message.rpc_id,
            RpcError {
                code: "bad-request".to_owned(),
                message: format!(
                    "method {:?} does not match endpoint {:?}",
                    message.method, endpoint
                ),
                details: json!({ "issues": [] }).as_object().expect("object").clone(),
            },
        );
    }
    let rpc_id = message.rpc_id;
    match handler(endpoint, message.payload, request.signal).await {
        Ok(result) => HttpResponse::json(200, &ServerResponse::new(rpc_id, result)),
        Err(error) => HttpResponse::text(500, format!("handler failure: {error}")),
    }
}

fn error_response(rpc_id: RpcId, error: RpcError) -> HttpResponse {
    HttpResponse::json(
        200,
        &ServerResponse::<Value>::new(rpc_id, RpcResult::Failure { error }),
    )
}

/// Extracts and validates a channel-relative endpoint from an absolute path.
#[must_use]
pub fn endpoint_from_path(channel: &str, pathname: &str) -> Option<String> {
    let endpoint = pathname.strip_prefix(&format!("{channel}/"))?;
    validate_endpoint(endpoint).then(|| endpoint.to_owned())
}

fn validate_endpoint(endpoint: &str) -> bool {
    !endpoint.is_empty()
        && endpoint.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$' | b'.' | b'-')
                })
        })
}

fn valid_channel(channel: &str) -> bool {
    channel.strip_prefix('/').is_some_and(|body| {
        !body.is_empty()
            && body.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'~' | b'-')
            })
    })
}

fn assert_channel(channel: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        valid_channel(channel) && channel != API_PATH,
        "connection: invalid or reserved RPC channel {channel:?}"
    );
    Ok(())
}

/// Validates one Client logical channel and endpoint pair.
///
/// # Errors
///
/// Returns the exact invalid-target diagnostic used by the browser caller.
pub fn validate_rpc_target(channel: &str, endpoint: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        valid_channel(channel) && validate_endpoint(endpoint),
        "connection: invalid RPC target {:?}",
        format!("{channel}/{endpoint}")
    );
    Ok(())
}

/// Future returned by one Client Connection RPC call.
pub type ClientConnectionFuture = BoxFuture<'static, anyhow::Result<RpcResult<Value>>>;

/// Client caller for logical RPC channels carried by the current transport.
pub trait ClientConnection: Send + Sync + 'static {
    /// Calls one channel-relative endpoint with correlation and envelope validation owned by the carrier.
    fn call(
        &self,
        channel: &str,
        endpoint: &str,
        payload: Value,
        signal: AbortSignal,
    ) -> ClientConnectionFuture;
}

struct HostDescriptionSnapshot {
    revision: u64,
    value: Option<HostDescription>,
}

struct DescriptionListener {
    id: Uuid,
    callback: Arc<dyn Fn() + Send + Sync>,
}

struct HostDescriptionState {
    snapshot: Mutex<HostDescriptionSnapshot>,
    listeners: Mutex<Vec<DescriptionListener>>,
}

impl HostDescriptionState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            snapshot: Mutex::new(HostDescriptionSnapshot {
                revision: 0,
                value: None,
            }),
            listeners: Mutex::new(Vec::new()),
        })
    }

    fn publish(&self, value: Option<HostDescription>, force: bool) -> u64 {
        let revision = {
            let mut snapshot = self.snapshot.lock();
            if !force && snapshot.value == value {
                return snapshot.revision;
            }
            snapshot.revision = snapshot.revision.wrapping_add(1);
            snapshot.value = value;
            snapshot.revision
        };
        let listeners = self
            .listeners
            .lock()
            .iter()
            .map(|listener| listener.callback.clone())
            .collect::<Vec<_>>();
        for listener in listeners {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener()));
        }
        revision
    }

    fn is_revision(&self, revision: u64) -> bool {
        self.snapshot.lock().revision == revision
    }
}

/// Type-erased live Client Connection plus one-shot downstream ownership.
pub struct ClientConnectionHandle {
    caller: Arc<dyn ClientConnection>,
    stream_api: Option<Arc<dyn StreamApi>>,
    is_loopback: bool,
    description: Arc<HostDescriptionState>,
    started: std::sync::atomic::AtomicBool,
}

impl fmt::Debug for ClientConnectionHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientConnectionHandle")
            .field("is_loopback", &self.is_loopback)
            .field(
                "started",
                &self.started.load(std::sync::atomic::Ordering::Acquire),
            )
            .finish_non_exhaustive()
    }
}

impl ClientConnectionHandle {
    /// Wraps one live caller.
    #[must_use]
    pub fn new(caller: Arc<dyn ClientConnection>) -> Arc<Self> {
        Arc::new(Self {
            caller,
            stream_api: None,
            is_loopback: true,
            description: HostDescriptionState::new(),
            started: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Wraps the complete Client transport and its derived page-authority state.
    #[must_use]
    pub fn with_streams(
        caller: Arc<dyn ClientConnection>,
        stream_api: Arc<dyn StreamApi>,
        is_loopback: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            caller,
            stream_api: Some(stream_api),
            is_loopback,
            description: HostDescriptionState::new(),
            started: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Whether the current page authority is loopback.
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        self.is_loopback
    }

    /// Latest connected-generation Host facts, absent before connect and while reconnecting.
    #[must_use]
    pub fn host_description(&self) -> Option<HostDescription> {
        self.description.snapshot.lock().value.clone()
    }

    /// Subscribes to Host description replacement and connection loss.
    #[must_use]
    pub fn subscribe_host_description(
        &self,
        callback: Arc<dyn Fn() + Send + Sync>,
    ) -> HostDescriptionSubscription {
        let id = Uuid::now_v7();
        self.description
            .listeners
            .lock()
            .push(DescriptionListener { id, callback });
        HostDescriptionSubscription {
            state: Arc::downgrade(&self.description),
            id,
        }
    }

    /// Starts the dual stream loop for its one allowed consumer.
    ///
    /// # Errors
    ///
    /// Rejects a second consumer or a transport-only handle without downstream streams.
    pub fn start(
        self: &Arc<Self>,
        sinks: ConnectionSinks,
        config: ConnectionConfig,
    ) -> anyhow::Result<ConnectionStopHandle> {
        anyhow::ensure!(
            !self.started.swap(true, std::sync::atomic::Ordering::AcqRel),
            "connection: the stream loop is already owned by another consumer"
        );
        let Some(api) = self.stream_api.clone() else {
            self.started
                .store(false, std::sync::atomic::Ordering::Release);
            anyhow::bail!("connection: this transport has no downstream stream API");
        };
        let description = self.description.clone();
        let consumer_connected = sinks.on_connected.clone();
        let consumer_state = sinks.on_state_change.clone();
        let controller = ConnectionController::new(
            api,
            ConnectionSinks {
                on_connected: Some(Arc::new(move |next| {
                    let revision = description.publish(Some(next.clone()), true);
                    if description.is_revision(revision)
                        && let Some(sink) = &consumer_connected
                    {
                        sink(next);
                    }
                })),
                on_state_change: Some({
                    let description = self.description.clone();
                    Arc::new(move |state| {
                        if state == ConnectionState::Reconnecting {
                            description.publish(None, false);
                        }
                        if let Some(sink) = &consumer_state {
                            sink(state);
                        }
                    })
                }),
                ..sinks
            },
            config,
        );
        controller.start();
        Ok(ConnectionStopHandle {
            controller,
            description: self.description.clone(),
        })
    }

    /// Calls one carrier channel and endpoint.
    pub fn call(
        &self,
        channel: &str,
        endpoint: &str,
        payload: Value,
        signal: AbortSignal,
    ) -> ClientConnectionFuture {
        self.caller.call(channel, endpoint, payload, signal)
    }
}

/// Idempotent stop handle returned to the sole downstream consumer.
pub struct ConnectionStopHandle {
    controller: Arc<ConnectionController>,
    description: Arc<HostDescriptionState>,
}

impl fmt::Debug for ConnectionStopHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectionStopHandle")
            .finish_non_exhaustive()
    }
}

impl ConnectionStopHandle {
    /// Stops physical streams and retracts the connected-generation description.
    pub fn stop(&self) {
        self.controller.stop();
        self.description.publish(None, false);
    }
}

/// Idempotent Host-description subscription disposer.
pub struct HostDescriptionSubscription {
    state: std::sync::Weak<HostDescriptionState>,
    id: Uuid,
}

impl fmt::Debug for HostDescriptionSubscription {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostDescriptionSubscription")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl HostDescriptionSubscription {
    /// Removes this exact listener. Repeated disposal is harmless.
    pub fn dispose(&self) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        state
            .listeners
            .lock()
            .retain(|listener| listener.id != self.id);
    }
}

/// Future returned by an HTTP transport implementation.
pub type HttpTransportFuture = Pin<Box<dyn Future<Output = anyhow::Result<HttpResponse>> + Send>>;

/// Physical HTTP carrier used by the generic web-style RPC caller.
pub trait HttpTransport: Send + Sync + 'static {
    /// Sends one complete request.
    fn fetch(&self, request: HttpRequest) -> HttpTransportFuture;
}

/// Correlating Client caller over an injected physical HTTP carrier.
pub struct WebConnectionRpc {
    transport: Arc<dyn HttpTransport>,
}

impl WebConnectionRpc {
    /// Creates a web-style caller.
    #[must_use]
    pub fn new(transport: Arc<dyn HttpTransport>) -> Arc<Self> {
        Arc::new(Self { transport })
    }
}

impl ClientConnection for WebConnectionRpc {
    fn call(
        &self,
        channel: &str,
        endpoint: &str,
        payload: Value,
        signal: AbortSignal,
    ) -> ClientConnectionFuture {
        let channel = channel.to_owned();
        let endpoint = endpoint.to_owned();
        let transport = self.transport.clone();
        Box::pin(async move {
            validate_rpc_target(&channel, &endpoint)?;
            let rpc_id = RpcId::new(Uuid::new_v4().to_string());
            let message = ClientRequest::new(rpc_id.clone(), endpoint.clone(), payload);
            let mut request = HttpRequest::new(HttpMethod::Post, format!("{channel}/{endpoint}"));
            request
                .headers
                .insert("content-type".to_owned(), "application/json".to_owned());
            request.body = serde_json::to_vec(&message)?;
            request.signal = signal;
            let response = transport.fetch(request).await?;
            anyhow::ensure!(
                (200..300).contains(&response.status),
                "transport failure for {channel}/{endpoint}: HTTP {}",
                response.status
            );
            let full = serde_json::from_slice::<ServerResponse>(&response.body)?.validate()?;
            anyhow::ensure!(
                full.rpc_id == rpc_id,
                "rpcId mismatch for {endpoint}: sent {rpc_id}, got {}",
                full.rpc_id
            );
            Ok(full.result)
        })
    }
}
