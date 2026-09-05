//! Host-side HTTP/WebSocket carrier adapter for [`ApiProxyRuntime`].

use std::{
    collections::HashMap,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

use futures::{FutureExt as _, StreamExt as _, future::BoxFuture, stream::BoxStream};
use seekdeep_client_connection::{
    ConnectionApiProxy, ConnectionFallback, DownlinkApi, DownlinkStream, EventFrame, HttpMethod,
    HttpRequest, HttpResponse, RpcError, RpcId, RpcResult, ServerResponse,
};
use seekdeep_llm::AbortSignal;
use serde::Serialize;
use serde_json::{Map, Value, json};
use url::form_urlencoded;
use uuid::Uuid;

use crate::api::{
    downloads::SessionLogQuery,
    events::{HostFrame, MuxFrame},
    method::{RpcMethod, parse_unary_request},
    rpc::{
        ClientResponse, RpcReceipt, RpcRequest, RpcResponse, ServerRequest, parse_client_request,
        parse_client_response,
    },
};

/// Typed downstream returned by the transport-agnostic API implementation.
pub type ApiDownlinkStream<F> = BoxStream<'static, anyhow::Result<RpcRequest<F>>>;

/// Streaming body returned by an injected fetch-shaped carrier.
pub type FetchBodyStream = BoxStream<'static, anyhow::Result<Vec<u8>>>;

/// Complete or streaming response produced by a fetch-shaped carrier.
pub enum FetchBody {
    /// Fully buffered response body used by unary calls and downloads.
    Complete(Vec<u8>),
    /// Incremental body used by the in-process SSE downlinks.
    Stream(FetchBodyStream),
}

/// Fetch-shaped response used at the isomorphic in-process seam.
pub struct FetchResponse {
    /// HTTP carrier status.
    pub status: u16,
    /// HTTP carrier headers.
    pub headers: HashMap<String, String>,
    /// Complete or streaming body.
    pub body: FetchBody,
}

impl FetchResponse {
    fn complete(mut response: HttpResponse) -> Self {
        let body = response.body_stream.take().map_or_else(
            || FetchBody::Complete(response.body),
            |stream| FetchBody::Stream(stream.boxed()),
        );
        Self {
            status: response.status,
            headers: response.headers,
            body,
        }
    }
}

/// Injected Request-to-Response aspect consumed by the in-process API client.
///
/// Implementations may be the real [`ApiProxyHandler`] or a protocol fixture.
/// Keeping the seam fetch-shaped exercises the same carrier boundary without
/// requiring a socket or DNS lookup.
pub trait FetchHandler: Send + Sync + 'static {
    /// Executes one complete HTTP-shaped request.
    fn fetch_request(
        &self,
        request: HttpRequest,
    ) -> BoxFuture<'static, anyhow::Result<FetchResponse>>;
}

/// Transport-agnostic implementation face consumed by the Host carrier.
///
/// The exhaustive [`RpcMethod`] registry and second-level parsers own wire
/// dispatch. Concrete gateway composition may fan this method back into typed
/// domain services without coupling those services to HTTP or WebSocket code.
pub trait ApiProxyRuntime: Send + Sync + 'static {
    /// Invokes one validated unary method.
    fn unary(
        &self,
        method: RpcMethod,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcResponse<Value>>>;

    /// Delivers one structurally valid Client response.
    fn respond(
        &self,
        message: ClientResponse,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcReceipt>>;

    /// Opens the mux event source.
    fn mux(&self, request: RpcRequest<Value>, signal: AbortSignal) -> ApiDownlinkStream<MuxFrame>;

    /// Opens the Host event source.
    fn host(&self, request: RpcRequest<Value>, signal: AbortSignal)
    -> ApiDownlinkStream<HostFrame>;

    /// Produces one Session-log download response.
    fn session_log(
        &self,
        query: SessionLogQuery,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<HttpResponse>>;
}

/// Pure carrier adapter corresponding to source `toFetchHandler` plus the
/// production WebSocket downlink projection.
pub struct ApiProxyHandler {
    api: Arc<dyn ApiProxyRuntime>,
}

impl std::fmt::Debug for ApiProxyHandler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApiProxyHandler")
            .finish_non_exhaustive()
    }
}

impl ApiProxyHandler {
    /// Wraps one transport-independent API implementation.
    #[must_use]
    pub fn new(api: Arc<dyn ApiProxyRuntime>) -> Arc<Self> {
        Arc::new(Self { api })
    }

    /// Builds Connection's optional API Proxy seat from this handler.
    #[must_use]
    pub fn connection_proxy(self: &Arc<Self>) -> Arc<ConnectionApiProxy> {
        let handler = self.clone();
        let fallback: ConnectionFallback = Arc::new(move |request| {
            let handler = handler.clone();
            Box::pin(async move { handler.fetch(request).await })
        });
        let downlinks: Arc<dyn DownlinkApi> = Arc::new(RuntimeDownlinks {
            api: self.api.clone(),
        });
        ConnectionApiProxy::new(fallback, downlinks)
    }

    /// Handles one already-trusted, bounded Connection HTTP request.
    pub async fn fetch(&self, request: HttpRequest) -> HttpResponse {
        let path = request.path.as_str();
        let is_head = matches!(&request.method, HttpMethod::Other(method) if method == "HEAD");
        if path == "/api/session.export" && (request.method == HttpMethod::Get || is_head) {
            return self.download(request).await;
        }
        if request.method != HttpMethod::Post || !path.starts_with("/api/") {
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
        if path == "/api/respond" {
            return self.respond(body, request.signal).await;
        }
        let Some(segment) = path.strip_prefix("/api/") else {
            return HttpResponse::text(404, "not found");
        };
        let Ok(method) = segment.parse::<RpcMethod>() else {
            return HttpResponse::text(404, "not found");
        };
        let raw_id = salvage_rpc_id(&body);
        let message = match parse_client_request(&body) {
            Ok(message) => message,
            Err(error) => {
                return bad_request(
                    raw_id,
                    "invalid client-request message".to_owned(),
                    error_issue(&error),
                );
            }
        };
        if message.method != method.as_str() {
            return bad_request(
                message.rpc_id,
                format!(
                    "method \"{}\" does not match path \"{}\"",
                    message.method,
                    method.as_str()
                ),
                Vec::new(),
            );
        }
        let payload = match parse_unary_request(method.as_str(), &message.payload) {
            Ok(payload) => payload,
            Err(error) => {
                return bad_request(
                    message.rpc_id,
                    format!("invalid payload for {}", method.as_str()),
                    vec![json!({
                        "code": "custom",
                        "path": [],
                        "message": error.to_string(),
                    })],
                );
            }
        };
        let signal = request.signal;
        let narrow = RpcRequest::new(message.rpc_id, payload);
        let future = match catch_unwind(AssertUnwindSafe(|| self.api.unary(method, narrow, signal)))
        {
            Ok(future) => future,
            Err(panic) => return handler_panic(&*panic),
        };
        match AssertUnwindSafe(future).catch_unwind().await {
            Ok(Ok(response)) => {
                json_response(&ServerResponse::new(response.rpc_id, response.result))
            }
            Ok(Err(error)) => HttpResponse::text(500, format!("handler failure: {error}")),
            Err(panic) => handler_panic(&*panic),
        }
    }

    /// Handles the complete fetch-shaped surface, including lazy SSE bodies.
    ///
    /// This is the Rust isomorphic point corresponding to source
    /// `toFetchHandler(api).fetch`: an [`crate::client::InProcessApiClient`]
    /// can consume it without opening a network connection.
    pub async fn fetch_request(&self, request: HttpRequest) -> FetchResponse {
        if request.method == HttpMethod::Get {
            match request.path.as_str() {
                "/api/events.mux" => {
                    let narrow = self.api.mux(
                        RpcRequest::new(RpcId::new(Uuid::new_v4().to_string()), json!({})),
                        request.signal,
                    );
                    return sse_response(narrow);
                }
                "/api/events.host" => {
                    let narrow = self.api.host(
                        RpcRequest::new(RpcId::new(Uuid::new_v4().to_string()), json!({})),
                        request.signal,
                    );
                    return sse_response(narrow);
                }
                _ => {}
            }
        }
        FetchResponse::complete(self.fetch(request).await)
    }

    async fn respond(&self, body: Value, signal: AbortSignal) -> HttpResponse {
        let Ok(message) = parse_client_response(&body) else {
            return json_response(&RpcReceipt::Rejected {
                reason: crate::api::rpc::RpcReceiptReason::BadResponse,
            });
        };
        match self.api.respond(message, signal).await {
            Ok(receipt) => json_response(&receipt),
            Err(error) => HttpResponse::text(500, format!("handler failure: {error}")),
        }
    }

    async fn download(&self, request: HttpRequest) -> HttpResponse {
        let query = query_object(request.query.as_deref().unwrap_or_default());
        let Ok(query) = SessionLogQuery::parse(&Value::Object(query)) else {
            return HttpResponse::text(400, "missing or invalid sessionId query parameter");
        };
        let head = matches!(&request.method, HttpMethod::Other(method) if method == "HEAD");
        match self.api.session_log(query, request.signal).await {
            Ok(mut response) => {
                if head {
                    response.body.clear();
                    response.body_stream.take();
                }
                response
            }
            Err(error) => HttpResponse::text(500, format!("handler failure: {error}")),
        }
    }
}

impl FetchHandler for ApiProxyHandler {
    fn fetch_request(
        &self,
        request: HttpRequest,
    ) -> BoxFuture<'static, anyhow::Result<FetchResponse>> {
        let handler = Arc::new(Self {
            api: self.api.clone(),
        });
        async move { Ok(handler.fetch_request(request).await) }.boxed()
    }
}

struct RuntimeDownlinks {
    api: Arc<dyn ApiProxyRuntime>,
}

impl DownlinkApi for RuntimeDownlinks {
    fn mux(&self, signal: AbortSignal) -> DownlinkStream {
        let request = RpcRequest::new(RpcId::new(Uuid::new_v4().to_string()), json!({}));
        typed_downlink(self.api.mux(request, signal))
    }

    fn host(&self, signal: AbortSignal) -> DownlinkStream {
        let request = RpcRequest::new(RpcId::new(Uuid::new_v4().to_string()), json!({}));
        typed_downlink(self.api.host(request, signal))
    }
}

fn typed_downlink<F: Serialize + Send + 'static>(frames: ApiDownlinkStream<F>) -> DownlinkStream {
    frames
        .map(|frame| {
            let frame = frame?;
            Ok(EventFrame {
                rpc_id: frame.rpc_id,
                payload: serde_json::to_value(frame.payload)?,
            })
        })
        .boxed()
}

fn sse_response<F>(frames: ApiDownlinkStream<F>) -> FetchResponse
where
    F: Serialize + Send + 'static,
{
    let body = async_stream::stream! {
        // A comment proves physical establishment even when the Host stream is idle.
        yield Ok(b": connected\n\n".to_vec());
        futures::pin_mut!(frames);
        while let Some(next) = frames.next().await {
            match next {
                Ok(narrow) => match full_sse_frame(narrow) {
                    Ok(bytes) => yield Ok(bytes),
                    Err(error) => {
                        yield Ok(stream_error_frame(&error));
                        return;
                    }
                },
                Err(error) => {
                    yield Ok(stream_error_frame(&error));
                    return;
                }
            }
        }
    }
    .boxed();
    FetchResponse {
        status: 200,
        headers: [
            ("content-type".to_owned(), "text/event-stream".to_owned()),
            ("cache-control".to_owned(), "no-cache".to_owned()),
        ]
        .into_iter()
        .collect(),
        body: FetchBody::Stream(body),
    }
}

fn full_sse_frame<F: Serialize>(narrow: RpcRequest<F>) -> anyhow::Result<Vec<u8>> {
    let payload = serde_json::to_value(narrow.payload)?;
    let method = payload
        .as_object()
        .and_then(|object| object.get("type"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("stream payload has no string type"))?;
    let full = ServerRequest::new(narrow.rpc_id, method, payload.clone());
    Ok(format!("data: {}\n\n", serde_json::to_string(&full)?).into_bytes())
}

fn stream_error_frame(error: &anyhow::Error) -> Vec<u8> {
    let payload = json!({
        "type": "stream/error",
        "error": {
            "code": "internal",
            "message": error.to_string(),
            "details": {},
        },
    });
    let full = ServerRequest::new(
        RpcId::new(Uuid::new_v4().to_string()),
        "stream/error",
        payload,
    );
    format!(
        "data: {}\n\n",
        serde_json::to_string(&full).expect("stream error envelope must serialize")
    )
    .into_bytes()
}

fn salvage_rpc_id(body: &Value) -> RpcId {
    body.as_object()
        .and_then(|object| object.get("rpcId"))
        .and_then(Value::as_str)
        .map_or_else(|| RpcId::new("invalid-request"), RpcId::new)
}

fn bad_request(rpc_id: RpcId, message: String, issues: Vec<Value>) -> HttpResponse {
    json_response(&ServerResponse::<Value>::new(
        rpc_id,
        RpcResult::Failure {
            error: RpcError {
                code: "bad-request".to_owned(),
                message,
                details: Map::from_iter([("issues".to_owned(), Value::Array(issues))]),
            },
        },
    ))
}

fn error_issue(error: &crate::api::rpc::ContractError) -> Vec<Value> {
    vec![json!({
        "code": "custom",
        "path": [],
        "message": error.to_string(),
    })]
}

fn query_object(query: &str) -> Map<String, Value> {
    form_urlencoded::parse(query.as_bytes())
        .map(|(key, value)| (key.into_owned(), Value::String(value.into_owned())))
        .collect()
}

fn json_response(value: &impl Serialize) -> HttpResponse {
    HttpResponse {
        status: 200,
        headers: HashMap::from([("content-type".to_owned(), "application/json".to_owned())]),
        body: serde_json::to_vec(value).expect("API Proxy wire values must serialize"),
        body_stream: None,
    }
}

fn handler_panic(panic: &(dyn std::any::Any + Send)) -> HttpResponse {
    let message = panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())
        .unwrap_or("unknown panic");
    HttpResponse::text(500, format!("handler failure: {message}"))
}
