//! Client-side two-level protocol parsing over Connection's physical carrier.

use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Weak},
    time::Duration,
};

use futures::{StreamExt as _, future::BoxFuture, stream::BoxStream};
use parking_lot::Mutex;
use seekdeep_client_connection::{
    ClientConnectionHandle, EnvelopeSubscription as WebEnvelopeSubscription, EventFrame,
    HttpMethod, HttpRequest, HttpResponse, HttpTransport, HttpTransportFuture, RpcResult,
    ServerResponse, StreamApi, UnaryTimeoutPolicy, WebApiClient, WebApiContract, WebApiDownlink,
    WebConnectionRpc,
};
use seekdeep_llm::AbortSignal;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use uuid::Uuid;

use crate::handler::{FetchBody, FetchHandler, FetchResponse};

use crate::api::{
    agent_presets::{
        AgentPresetCopyRequest, AgentPresetIdValue, AgentPresetListValue,
        AgentPresetOpenDocumentValue, AgentPresetReadValue, AgentPresetSelectRequest,
    },
    credentials::{
        CredentialsDescribeRequest, CredentialsDescribeValue, CredentialsSetRequest,
        CredentialsUnsetRequest,
    },
    events::{HostFrame, MuxFrame},
    goals::{GoalClearValue, GoalCreateRequest, GoalEditRequest, GoalRefRequest, GoalRefValue},
    host::{
        DirectoryListing, EmptyRequest, HostCreateDirectoryRequest, HostDescribeValue,
        HostListDirectoryRequest, HostOpenPathRequest, HostOpenPathValue, HostPathValue,
        HostPickDirectoryValue,
    },
    llm::{LlmDiscoverModelsRequest, LlmDiscoverModelsValue, LlmModelsValue, LlmProvidersValue},
    method::{RpcMethod, parse_unary_value},
    rpc::{
        ClientResponse, RpcReceipt, RpcRequest, RpcResponse, parse_rpc_receipt,
        parse_server_request, parse_server_response,
    },
    sessions::{
        AcceptedValue, SessionAttachmentRequest, SessionAttachmentValue, SessionCreateRequest,
        SessionCreateValue, SessionForkRequest, SessionHistoryRequest, SessionHistoryValue,
        SessionIdValue, SessionListRequest, SessionListValue, SessionModelsValue,
        SessionPromptRequest, SessionPromptValue, SessionRenameRequest, SessionRenameValue,
        SessionSearchRequest, SessionSearchValue, SessionSelectModelRequest,
        SessionSelectModelValue, SessionUpdateQueueRequest,
    },
    settings::{
        SettingsDescribeValue, SettingsMutateRequest, SettingsNamespaceView,
        SettingsOpenDocumentValue, SettingsReplaceRequest, SettingsUpdateRequest,
    },
    skills::{SkillListRequest, SkillListValue},
    subagents::{
        SubagentHistoryRequest, SubagentInterruptRequest, SubagentListRequest, SubagentListValue,
        SubagentPromptRequest, SubagentPromptValue,
    },
    workspace::{
        WorkspaceArchiveSessionRequest, WorkspaceArchiveSessionValue, WorkspaceCreateRequest,
        WorkspaceCreateValue, WorkspaceDeleteValue, WorkspaceIdRequest,
        WorkspaceInsertBeforeRequest, WorkspaceInsertBeforeValue,
        WorkspaceInsertSessionBeforeRequest, WorkspaceListValue, WorkspaceRenameRequest,
        WorkspaceValue,
    },
};

/// Observer for one microtask-like batch of complete wire envelopes.
pub type EnvelopeListener = Arc<dyn Fn(&[Value]) + Send + Sync>;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

struct LocalEnvelopeListener {
    id: Uuid,
    callback: EnvelopeListener,
}

#[derive(Default)]
struct LocalEnvelopeState {
    buffer: Vec<Value>,
    flush_scheduled: bool,
    listeners: Vec<LocalEnvelopeListener>,
}

enum EnvelopeSubscriptionInner {
    Web(WebEnvelopeSubscription),
    Local {
        state: Weak<Mutex<LocalEnvelopeState>>,
        id: Uuid,
    },
}

/// Idempotent disposer for one API-client envelope observer.
pub struct EnvelopeSubscription(EnvelopeSubscriptionInner);

impl std::fmt::Debug for EnvelopeSubscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EnvelopeSubscription")
            .finish_non_exhaustive()
    }
}

impl EnvelopeSubscription {
    /// Removes this exact observer without disturbing later registrations.
    pub fn dispose(&self) {
        match &self.0 {
            EnvelopeSubscriptionInner::Web(subscription) => subscription.dispose(),
            EnvelopeSubscriptionInner::Local { state, id } => {
                if let Some(state) = state.upgrade() {
                    state.lock().listeners.retain(|listener| listener.id != *id);
                }
            }
        }
    }
}

trait ApiClientTransport: Send + Sync + 'static {
    fn call_unary_with_policy(
        &self,
        method: String,
        payload: Value,
        signal: AbortSignal,
        timeout_policy: UnaryTimeoutPolicy,
    ) -> BoxFuture<'static, anyhow::Result<ServerResponse>>;

    fn respond_raw(
        &self,
        message: Value,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<Value>>;

    fn mux(
        &self,
        signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>>;

    fn host(
        &self,
        signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>>;

    fn subscribe_envelopes(&self, callback: EnvelopeListener) -> EnvelopeSubscription;
}

struct WebClientTransport(Arc<WebApiClient>);

impl ApiClientTransport for WebClientTransport {
    fn call_unary_with_policy(
        &self,
        method: String,
        payload: Value,
        signal: AbortSignal,
        timeout_policy: UnaryTimeoutPolicy,
    ) -> BoxFuture<'static, anyhow::Result<ServerResponse>> {
        self.0
            .call_unary_with_policy(method, payload, signal, timeout_policy)
    }

    fn respond_raw(
        &self,
        message: Value,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<Value>> {
        self.0.respond_raw(message, signal)
    }

    fn mux(
        &self,
        signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>> {
        self.0.mux(signal, on_open)
    }

    fn host(
        &self,
        signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>> {
        self.0.host(signal, on_open)
    }

    fn subscribe_envelopes(&self, callback: EnvelopeListener) -> EnvelopeSubscription {
        EnvelopeSubscription(EnvelopeSubscriptionInner::Web(
            self.0.subscribe_envelopes(callback),
        ))
    }
}

#[derive(Clone)]
struct InProcessTransport {
    handler: Arc<dyn FetchHandler>,
    timeout: Duration,
    contract: ApiProxyContract,
    envelopes: Arc<Mutex<LocalEnvelopeState>>,
}

impl InProcessTransport {
    fn new(handler: Arc<dyn FetchHandler>, timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            handler,
            timeout,
            contract: ApiProxyContract,
            envelopes: Arc::new(Mutex::new(LocalEnvelopeState::default())),
        })
    }

    fn fetch_with_policy(
        &self,
        mut request: HttpRequest,
        signal: AbortSignal,
        timeout_policy: UnaryTimeoutPolicy,
    ) -> BoxFuture<'static, anyhow::Result<FetchResponse>> {
        let transport = self.clone();
        Box::pin(async move {
            match timeout_policy {
                UnaryTimeoutPolicy::Bounded => {
                    let timeout_signal = AbortSignal::default();
                    request.signal = AbortSignal::fuse(&signal, &timeout_signal);
                    tokio::select! {
                        () = tokio::time::sleep(transport.timeout) => {
                            timeout_signal.abort();
                            anyhow::bail!("transport timed out");
                        }
                        () = signal.cancelled() => {
                            anyhow::bail!(abort_message(&signal));
                        }
                        response = transport.handler.fetch_request(request) => response,
                    }
                }
                UnaryTimeoutPolicy::CallerSignalOnly => {
                    request.signal = signal.clone();
                    tokio::select! {
                        () = signal.cancelled() => {
                            anyhow::bail!(abort_message(&signal));
                        }
                        response = transport.handler.fetch_request(request) => response,
                    }
                }
            }
        })
    }

    async fn post_json(
        &self,
        path: &str,
        body: Vec<u8>,
        signal: AbortSignal,
        timeout_policy: UnaryTimeoutPolicy,
    ) -> anyhow::Result<Vec<u8>> {
        let mut request = HttpRequest::new(HttpMethod::Post, path);
        request
            .headers
            .insert("content-type".to_owned(), "application/json".to_owned());
        request.body = body;
        let response = self
            .fetch_with_policy(request, signal, timeout_policy)
            .await
            .map_err(|error| anyhow::anyhow!("transport failure for {path}: {error}"))?;
        anyhow::ensure!(
            (200..300).contains(&response.status),
            "transport failure for {path}: HTTP {}",
            response.status
        );
        match response.body {
            FetchBody::Complete(body) => Ok(body),
            FetchBody::Stream(_) => {
                anyhow::bail!("transport failure for {path}: unexpected streaming response")
            }
        }
    }

    fn open_sse(
        &self,
        path: &'static str,
        downlink: WebApiDownlink,
        signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>> {
        let transport = self.clone();
        Box::pin(async_stream::try_stream! {
            if signal.is_aborted() {
                Err(anyhow::anyhow!(abort_message(&signal)))?;
            }
            let mut request = HttpRequest::new(HttpMethod::Get, path);
            request.signal = signal.clone();
            let response = tokio::select! {
                () = signal.cancelled() => Err(anyhow::anyhow!(abort_message(&signal))),
                response = transport.handler.fetch_request(request) => response,
            }?;
            if !(200..300).contains(&response.status) {
                Err(anyhow::anyhow!(
                    "transport failure for {path}: HTTP {}",
                    response.status
                ))?;
            }
            let _ = catch_unwind(AssertUnwindSafe(|| on_open()));
            let mut body: BoxStream<'static, anyhow::Result<Vec<u8>>> = match response.body {
                FetchBody::Complete(bytes) => futures::stream::once(async move { Ok(bytes) }).boxed(),
                FetchBody::Stream(body) => body,
            };
            let mut buffer = Vec::new();
            loop {
                let chunk = tokio::select! {
                    () = signal.cancelled() => return,
                    chunk = body.next() => chunk,
                };
                let Some(chunk) = chunk else { return };
                buffer.extend(chunk?);
                while let Some(boundary) = find_sse_boundary(&buffer) {
                    let chunk = buffer.drain(..boundary).collect::<Vec<_>>();
                    buffer.drain(..2);
                    let data = sse_data(&chunk);
                    if data.is_empty() {
                        continue;
                    }
                    let parsed = (|| -> anyhow::Result<(Value, EventFrame)> {
                        let wire: Value = serde_json::from_slice(&data)?;
                        let full = parse_server_request(&wire)?;
                        let payload = transport.contract.parse_downlink_payload(downlink, &full.payload)?;
                        let observed = serde_json::to_value(&full)?;
                        Ok((observed, EventFrame { rpc_id: full.rpc_id, payload }))
                    })();
                    match parsed {
                        Ok((full, frame)) => {
                            transport.observe(full);
                            yield frame;
                        }
                        Err(error) => {
                            tracing::error!(%error, path, "host-apiproxy: dropping malformed SSE frame");
                        }
                    }
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

impl ApiClientTransport for InProcessTransport {
    fn call_unary_with_policy(
        &self,
        method: String,
        payload: Value,
        signal: AbortSignal,
        timeout_policy: UnaryTimeoutPolicy,
    ) -> BoxFuture<'static, anyhow::Result<ServerResponse>> {
        let transport = self.clone();
        Box::pin(async move {
            let rpc_id = seekdeep_client_connection::RpcId::new(Uuid::new_v4().to_string());
            let message = seekdeep_client_connection::ClientRequest::new(
                rpc_id.clone(),
                method.clone(),
                payload,
            );
            transport.observe(serde_json::to_value(&message)?);
            let path = format!("/api/{method}");
            let body = transport
                .post_json(&path, serde_json::to_vec(&message)?, signal, timeout_policy)
                .await?;
            let wire: Value = serde_json::from_slice(&body)?;
            let mut full = transport.contract.parse_server_response(&wire)?;
            transport.observe(serde_json::to_value(&full)?);
            anyhow::ensure!(
                full.rpc_id == rpc_id,
                "rpcId mismatch for {method}: sent {rpc_id}, got {}",
                full.rpc_id
            );
            if let RpcResult::Success { value } = &full.result {
                full.result = RpcResult::Success {
                    value: transport
                        .contract
                        .parse_unary_success_value(&method, value.as_ref())?,
                };
            }
            Ok(full)
        })
    }

    fn respond_raw(
        &self,
        message: Value,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<Value>> {
        let transport = self.clone();
        Box::pin(async move {
            transport.observe(message.clone());
            let body = transport
                .post_json(
                    "/api/respond",
                    serde_json::to_vec(&message)?,
                    signal,
                    UnaryTimeoutPolicy::Bounded,
                )
                .await?;
            Ok(serde_json::from_slice(&body)?)
        })
    }

    fn mux(
        &self,
        signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>> {
        self.open_sse("/api/events.mux", WebApiDownlink::Mux, signal, on_open)
    }

    fn host(
        &self,
        signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>> {
        self.open_sse("/api/events.host", WebApiDownlink::Host, signal, on_open)
    }

    fn subscribe_envelopes(&self, callback: EnvelopeListener) -> EnvelopeSubscription {
        let id = Uuid::now_v7();
        self.envelopes
            .lock()
            .listeners
            .push(LocalEnvelopeListener { id, callback });
        EnvelopeSubscription(EnvelopeSubscriptionInner::Local {
            state: Arc::downgrade(&self.envelopes),
            id,
        })
    }
}

impl HttpTransport for InProcessTransport {
    fn fetch(&self, request: HttpRequest) -> HttpTransportFuture {
        let transport = self.clone();
        let signal = request.signal.clone();
        Box::pin(async move {
            let response = transport
                .fetch_with_policy(request, signal, UnaryTimeoutPolicy::CallerSignalOnly)
                .await?;
            let FetchBody::Complete(body) = response.body else {
                anyhow::bail!("in-process HTTP caller received a streaming response");
            };
            Ok(HttpResponse {
                status: response.status,
                headers: response.headers,
                body,
                body_stream: None,
            })
        })
    }
}

impl StreamApi for InProcessTransport {
    fn describe(&self) -> BoxFuture<'static, anyhow::Result<RpcResult<Value>>> {
        let call = ApiClientTransport::call_unary_with_policy(
            self,
            RpcMethod::HostDescribe.as_str().to_owned(),
            serde_json::json!({}),
            AbortSignal::default(),
            UnaryTimeoutPolicy::Bounded,
        );
        Box::pin(async move { Ok(call.await?.result) })
    }

    fn mux(
        &self,
        signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>> {
        self.open_sse("/api/events.mux", WebApiDownlink::Mux, signal, on_open)
    }

    fn host(
        &self,
        signal: AbortSignal,
        on_open: Arc<dyn Fn() + Send + Sync>,
    ) -> BoxStream<'static, anyhow::Result<EventFrame>> {
        self.open_sse("/api/events.host", WebApiDownlink::Host, signal, on_open)
    }
}

fn abort_message(signal: &AbortSignal) -> String {
    signal.reason().map_or_else(
        || "This operation was aborted".to_owned(),
        |reason| match reason {
            Value::String(reason) => reason,
            other => other.to_string(),
        },
    )
}

fn find_sse_boundary(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|window| window == b"\n\n")
}

fn sse_data(chunk: &[u8]) -> Vec<u8> {
    let mut data = Vec::new();
    for line in chunk.split(|byte| *byte == b'\n') {
        if let Some(value) = line.strip_prefix(b"data: ") {
            data.extend_from_slice(value);
        }
    }
    data
}

/// Concrete API Proxy schema contract installed into the physical web carrier.
#[derive(Clone, Copy, Debug, Default)]
pub struct ApiProxyContract;

impl WebApiContract for ApiProxyContract {
    fn parse_server_response(&self, value: &Value) -> anyhow::Result<ServerResponse> {
        Ok(parse_server_response(value)?)
    }

    fn parse_unary_success_value(
        &self,
        method: &str,
        value: Option<&Value>,
    ) -> anyhow::Result<Option<Value>> {
        let value = value.ok_or_else(|| {
            anyhow::anyhow!("API Proxy method {method} returned success without a value")
        })?;
        Ok(Some(parse_unary_value(method, value)?))
    }

    fn parse_downlink_payload(
        &self,
        downlink: WebApiDownlink,
        payload: &Value,
    ) -> anyhow::Result<Value> {
        match downlink {
            WebApiDownlink::Mux => Ok(serde_json::to_value(MuxFrame::parse(payload)?)?),
            WebApiDownlink::Host => Ok(serde_json::to_value(HostFrame::parse(payload)?)?),
        }
    }
}

/// Creates the real web carrier with API Proxy's method and frame schemas installed.
///
/// # Errors
///
/// Rejects malformed bases and non-HTTP schemes.
pub fn new_web_api_client(base: Option<&str>) -> anyhow::Result<Arc<WebApiClient>> {
    WebApiClient::with_contract(base, Arc::new(ApiProxyContract))
}

/// Creates the contracted web carrier with an explicit bounded-unary timeout.
///
/// # Errors
///
/// Rejects malformed bases and non-HTTP schemes.
pub fn new_web_api_client_with_timeout(
    base: Option<&str>,
    timeout: Duration,
) -> anyhow::Result<Arc<WebApiClient>> {
    WebApiClient::with_timeout_and_contract(base, timeout, Arc::new(ApiProxyContract))
}

/// Normalizes a business result exactly as the concrete client would after
/// first-level parsing. This small helper is useful to in-process carriers.
///
/// # Errors
///
/// Rejects an omitted/invalid success value or a malformed closed error.
pub fn parse_method_result(
    method: &str,
    result: &RpcResult<Value>,
) -> anyhow::Result<RpcResult<Value>> {
    crate::api::method::parse_unary_result(method, result)
}

/// Typed client-consumption face of the complete API Proxy contract.
#[derive(Clone)]
pub struct ApiClient {
    transport: Arc<dyn ApiClientTransport>,
    connection_handle: Arc<ClientConnectionHandle>,
    /// Session operations.
    pub sessions: SessionsClient,
    /// Delegated child-session operations.
    pub subagents: SubagentsClient,
    /// Host capability operations.
    pub host: HostClient,
    /// Workspace operations.
    pub workspace: WorkspaceClient,
    /// Skill catalog operations.
    pub skills: SkillsClient,
    /// Agent preset operations.
    pub agent_presets: AgentPresetsClient,
    /// Downstream event streams.
    pub events: EventsClient,
    /// Goal operations.
    pub goals: GoalsClient,
    /// Settings operations.
    pub settings: SettingsClient,
    /// Credential operations.
    pub credentials: CredentialsClient,
    /// LLM configuration/catalog operations.
    pub llm: LlmClient,
}

impl std::fmt::Debug for ApiClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ApiClient").finish_non_exhaustive()
    }
}

impl ApiClient {
    /// Creates the complete typed client over a real contracted web carrier.
    ///
    /// # Errors
    ///
    /// Rejects malformed bases and non-HTTP schemes.
    pub fn new(base: Option<&str>) -> anyhow::Result<Self> {
        Ok(Self::from_transport(new_web_api_client(base)?))
    }

    /// Creates the complete typed client with an explicit unary deadline.
    ///
    /// # Errors
    ///
    /// Rejects malformed bases and non-HTTP schemes.
    pub fn with_timeout(base: Option<&str>, timeout: Duration) -> anyhow::Result<Self> {
        Ok(Self::from_transport(new_web_api_client_with_timeout(
            base, timeout,
        )?))
    }

    /// Projects a preconstructed contracted transport into every domain face.
    #[must_use]
    pub fn from_transport(transport: Arc<WebApiClient>) -> Self {
        let connection_handle = transport.connection_handle();
        let transport: Arc<dyn ApiClientTransport> = Arc::new(WebClientTransport(transport));
        Self::from_parts(transport, connection_handle)
    }

    fn from_parts(
        transport: Arc<dyn ApiClientTransport>,
        connection_handle: Arc<ClientConnectionHandle>,
    ) -> Self {
        Self {
            sessions: SessionsClient(transport.clone()),
            subagents: SubagentsClient(transport.clone()),
            host: HostClient(transport.clone()),
            workspace: WorkspaceClient(transport.clone()),
            skills: SkillsClient(transport.clone()),
            agent_presets: AgentPresetsClient(transport.clone()),
            events: EventsClient(transport.clone()),
            goals: GoalsClient(transport.clone()),
            settings: SettingsClient(transport.clone()),
            credentials: CredentialsClient(transport.clone()),
            llm: LlmClient(transport.clone()),
            connection_handle,
            transport,
        }
    }

    /// Returns Connection's complete RPC/stream handle over the same carrier.
    #[must_use]
    pub fn connection_handle(&self) -> Arc<ClientConnectionHandle> {
        self.connection_handle.clone()
    }

    /// Subscribes to microtask-like batches of all four full-form envelopes.
    #[must_use]
    pub fn subscribe_envelopes(&self, callback: EnvelopeListener) -> EnvelopeSubscription {
        self.transport.subscribe_envelopes(callback)
    }

    /// Delivers one Client response to a pending Host request.
    ///
    /// # Errors
    ///
    /// Returns transport or closed receipt-schema failures.
    pub async fn respond(
        &self,
        message: ClientResponse,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<RpcReceipt> {
        let value = self
            .transport
            .respond_raw(serde_json::to_value(message)?, signal.unwrap_or_default())
            .await?;
        Ok(parse_rpc_receipt(&value)?)
    }
}

/// Complete typed API client over an injected fetch-shaped handler.
///
/// This carrier exercises the same HTTP envelopes and SSE bytes as the web
/// client while opening no socket and performing no DNS lookup.
#[derive(Clone, Debug)]
pub struct InProcessApiClient {
    inner: ApiClient,
}

impl InProcessApiClient {
    /// Creates an in-process client with the normal bounded-unary timeout.
    #[must_use]
    pub fn new<H>(handler: Arc<H>) -> Self
    where
        H: FetchHandler,
    {
        let handler: Arc<dyn FetchHandler> = handler;
        Self::from_fetch_handler_with_timeout(handler, DEFAULT_TIMEOUT)
    }

    /// Creates an in-process client with an explicit bounded-unary timeout.
    #[must_use]
    pub fn with_timeout<H>(handler: Arc<H>, timeout: Duration) -> Self
    where
        H: FetchHandler,
    {
        let handler: Arc<dyn FetchHandler> = handler;
        Self::from_fetch_handler_with_timeout(handler, timeout)
    }

    /// Creates a client from a dynamically selected fetch-shaped handler.
    #[must_use]
    pub fn from_fetch_handler(handler: Arc<dyn FetchHandler>) -> Self {
        Self::from_fetch_handler_with_timeout(handler, DEFAULT_TIMEOUT)
    }

    fn from_fetch_handler_with_timeout(handler: Arc<dyn FetchHandler>, timeout: Duration) -> Self {
        let transport = InProcessTransport::new(handler, timeout);
        let http: Arc<dyn HttpTransport> = transport.clone();
        let caller = WebConnectionRpc::new(http);
        let streams: Arc<dyn StreamApi> = transport.clone();
        let connection_handle = ClientConnectionHandle::with_streams(caller, streams, true);
        let api_transport: Arc<dyn ApiClientTransport> = transport;
        Self {
            inner: ApiClient::from_parts(api_transport, connection_handle),
        }
    }

    /// Unwraps the common typed client face.
    #[must_use]
    pub fn into_inner(self) -> ApiClient {
        self.inner
    }
}

impl std::ops::Deref for InProcessApiClient {
    type Target = ApiClient;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

macro_rules! unary_method {
    ($name:ident, $method:ident, $request:ty, $response:ty) => {
        /// Calls one compiler-closed API Proxy method through the complete two-level carrier.
        ///
        /// # Errors
        ///
        /// Returns transport, correlation, schema, or typed-decoding failures.
        pub async fn $name(
            &self,
            payload: $request,
            signal: Option<AbortSignal>,
        ) -> anyhow::Result<RpcResponse<$response>> {
            typed_call(
                &self.0,
                RpcMethod::$method.as_str(),
                payload,
                signal,
                UnaryTimeoutPolicy::Bounded,
            )
            .await
        }
    };
    ($name:ident, $method:ident, $request:ty, $response:ty, caller_only) => {
        /// Calls one user-paced method without the default unary deadline.
        ///
        /// # Errors
        ///
        /// Returns transport, cancellation, correlation, schema, or typed-decoding failures.
        pub async fn $name(
            &self,
            payload: $request,
            signal: Option<AbortSignal>,
        ) -> anyhow::Result<RpcResponse<$response>> {
            typed_call(
                &self.0,
                RpcMethod::$method.as_str(),
                payload,
                signal,
                UnaryTimeoutPolicy::CallerSignalOnly,
            )
            .await
        }
    };
}

async fn typed_call<P, T>(
    transport: &Arc<dyn ApiClientTransport>,
    method: &str,
    payload: P,
    signal: Option<AbortSignal>,
    timeout_policy: UnaryTimeoutPolicy,
) -> anyhow::Result<RpcResponse<T>>
where
    P: Serialize,
    T: DeserializeOwned,
{
    let response = transport
        .call_unary_with_policy(
            method.to_owned(),
            serde_json::to_value(payload)?,
            signal.unwrap_or_default(),
            timeout_policy,
        )
        .await?;
    let result = match response.result {
        RpcResult::Success { value: Some(value) } => RpcResult::Success {
            value: Some(serde_json::from_value(value)?),
        },
        RpcResult::Success { value: None } => {
            anyhow::bail!("API Proxy method {method} returned success without a value")
        }
        RpcResult::Failure { error } => RpcResult::Failure { error },
    };
    Ok(RpcResponse::new(response.rpc_id, result))
}

/// Typed Session domain client.
#[derive(Clone)]
pub struct SessionsClient(Arc<dyn ApiClientTransport>);

impl SessionsClient {
    unary_method!(list, SessionList, SessionListRequest, SessionListValue);
    unary_method!(
        search,
        SessionSearch,
        SessionSearchRequest,
        SessionSearchValue
    );
    unary_method!(
        create,
        SessionCreate,
        SessionCreateRequest,
        SessionCreateValue
    );
    unary_method!(
        history,
        SessionHistory,
        SessionHistoryRequest,
        SessionHistoryValue
    );
    unary_method!(models, SessionModels, SessionIdValue, SessionModelsValue);
    unary_method!(
        select_model,
        SessionSelectModel,
        SessionSelectModelRequest,
        SessionSelectModelValue
    );
    unary_method!(
        rename,
        SessionRename,
        SessionRenameRequest,
        SessionRenameValue
    );
    unary_method!(fork, SessionFork, SessionForkRequest, SessionIdValue);
    unary_method!(
        prompt,
        SessionPrompt,
        SessionPromptRequest,
        SessionPromptValue
    );
    unary_method!(
        attachment,
        SessionAttachment,
        SessionAttachmentRequest,
        SessionAttachmentValue
    );
    unary_method!(
        update_queue,
        SessionUpdateQueue,
        SessionUpdateQueueRequest,
        AcceptedValue
    );
    unary_method!(cancel, SessionCancel, SessionIdValue, AcceptedValue);
}

/// Typed Subagent domain client.
#[derive(Clone)]
pub struct SubagentsClient(Arc<dyn ApiClientTransport>);

impl SubagentsClient {
    unary_method!(list, SubagentList, SubagentListRequest, SubagentListValue);
    unary_method!(
        history,
        SubagentHistory,
        SubagentHistoryRequest,
        SessionHistoryValue
    );
    unary_method!(
        prompt,
        SubagentPrompt,
        SubagentPromptRequest,
        SubagentPromptValue
    );
    unary_method!(
        interrupt,
        SubagentInterrupt,
        SubagentInterruptRequest,
        AcceptedValue
    );
}

/// Typed Host domain client.
#[derive(Clone)]
pub struct HostClient(Arc<dyn ApiClientTransport>);

impl HostClient {
    unary_method!(describe, HostDescribe, EmptyRequest, HostDescribeValue);
    unary_method!(
        pick_directory,
        HostPickDirectory,
        EmptyRequest,
        HostPickDirectoryValue,
        caller_only
    );
    unary_method!(
        list_directory,
        HostListDirectory,
        HostListDirectoryRequest,
        DirectoryListing
    );
    unary_method!(
        create_directory,
        HostCreateDirectory,
        HostCreateDirectoryRequest,
        HostPathValue
    );
    unary_method!(
        open_path,
        HostOpenPath,
        HostOpenPathRequest,
        HostOpenPathValue
    );
}

/// Typed Workspace domain client.
#[derive(Clone)]
pub struct WorkspaceClient(Arc<dyn ApiClientTransport>);

impl WorkspaceClient {
    unary_method!(list, WorkspaceList, EmptyRequest, WorkspaceListValue);
    unary_method!(
        create,
        WorkspaceCreate,
        WorkspaceCreateRequest,
        WorkspaceCreateValue
    );
    unary_method!(
        rename,
        WorkspaceRename,
        WorkspaceRenameRequest,
        WorkspaceValue
    );
    unary_method!(
        delete,
        WorkspaceDelete,
        WorkspaceIdRequest,
        WorkspaceDeleteValue
    );
    unary_method!(
        insert_before,
        WorkspaceInsertBefore,
        WorkspaceInsertBeforeRequest,
        WorkspaceInsertBeforeValue
    );
    unary_method!(
        insert_session_before,
        WorkspaceInsertSessionBefore,
        WorkspaceInsertSessionBeforeRequest,
        WorkspaceValue
    );
    unary_method!(
        archive_session,
        WorkspaceArchiveSession,
        WorkspaceArchiveSessionRequest,
        WorkspaceArchiveSessionValue
    );
}

/// Typed Skill domain client.
#[derive(Clone)]
pub struct SkillsClient(Arc<dyn ApiClientTransport>);

impl SkillsClient {
    unary_method!(list, SkillList, SkillListRequest, SkillListValue);
}

/// Typed Agent Preset domain client.
#[derive(Clone)]
pub struct AgentPresetsClient(Arc<dyn ApiClientTransport>);

impl AgentPresetsClient {
    unary_method!(list, AgentPresetList, EmptyRequest, AgentPresetListValue);
    unary_method!(
        select,
        AgentPresetSelect,
        AgentPresetSelectRequest,
        AgentPresetIdValue
    );
    unary_method!(
        read,
        AgentPresetRead,
        AgentPresetIdValue,
        AgentPresetReadValue
    );
    unary_method!(
        copy,
        AgentPresetCopy,
        AgentPresetCopyRequest,
        AgentPresetIdValue
    );
    unary_method!(
        open_document,
        AgentPresetOpenDocument,
        AgentPresetIdValue,
        AgentPresetOpenDocumentValue
    );
    unary_method!(remove, AgentPresetRemove, AgentPresetIdValue, EmptyRequest);
}

/// Typed Goal domain client.
#[derive(Clone)]
pub struct GoalsClient(Arc<dyn ApiClientTransport>);

impl GoalsClient {
    unary_method!(create, GoalCreate, GoalCreateRequest, GoalRefValue);
    unary_method!(edit, GoalEdit, GoalEditRequest, GoalRefValue);
    unary_method!(pause, GoalPause, GoalRefRequest, GoalRefValue);
    unary_method!(resume, GoalResume, GoalRefRequest, GoalRefValue);
    unary_method!(complete, GoalComplete, GoalRefRequest, GoalRefValue);
    unary_method!(clear, GoalClear, GoalRefRequest, GoalClearValue);
}

/// Typed Settings domain client.
#[derive(Clone)]
pub struct SettingsClient(Arc<dyn ApiClientTransport>);

impl SettingsClient {
    unary_method!(
        describe,
        SettingsDescribe,
        EmptyRequest,
        SettingsDescribeValue
    );
    unary_method!(
        open_document,
        SettingsOpenDocument,
        EmptyRequest,
        SettingsOpenDocumentValue
    );
    unary_method!(
        update,
        SettingsUpdate,
        SettingsUpdateRequest,
        SettingsNamespaceView
    );
    unary_method!(
        replace,
        SettingsReplace,
        SettingsReplaceRequest,
        SettingsNamespaceView
    );
    unary_method!(
        mutate,
        SettingsMutate,
        SettingsMutateRequest,
        SettingsNamespaceView
    );
}

/// Typed Credential domain client.
#[derive(Clone)]
pub struct CredentialsClient(Arc<dyn ApiClientTransport>);

impl CredentialsClient {
    unary_method!(
        describe,
        CredentialsDescribe,
        CredentialsDescribeRequest,
        CredentialsDescribeValue
    );
    unary_method!(set, CredentialsSet, CredentialsSetRequest, EmptyRequest);
    unary_method!(
        unset,
        CredentialsUnset,
        CredentialsUnsetRequest,
        EmptyRequest
    );
}

/// Typed LLM configuration/catalog domain client.
#[derive(Clone)]
pub struct LlmClient(Arc<dyn ApiClientTransport>);

impl LlmClient {
    unary_method!(providers, LlmProviders, EmptyRequest, LlmProvidersValue);
    unary_method!(models, LlmModels, EmptyRequest, LlmModelsValue);
    unary_method!(
        discover_models,
        LlmDiscoverModels,
        LlmDiscoverModelsRequest,
        LlmDiscoverModelsValue
    );
}

/// Typed downstream event client.
#[derive(Clone)]
pub struct EventsClient(Arc<dyn ApiClientTransport>);

impl EventsClient {
    /// Opens the lazy mux stream.
    #[must_use]
    pub fn mux(
        &self,
        _payload: EmptyRequest,
        signal: AbortSignal,
        on_open: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> BoxStream<'static, anyhow::Result<RpcRequest<MuxFrame>>> {
        typed_stream(
            self.0
                .mux(signal, on_open.unwrap_or_else(|| Arc::new(|| {}))),
        )
    }

    /// Opens the lazy Host stream.
    #[must_use]
    pub fn host(
        &self,
        _payload: EmptyRequest,
        signal: AbortSignal,
        on_open: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> BoxStream<'static, anyhow::Result<RpcRequest<HostFrame>>> {
        typed_stream(
            self.0
                .host(signal, on_open.unwrap_or_else(|| Arc::new(|| {}))),
        )
    }
}

fn typed_stream<T: DeserializeOwned + Send + 'static>(
    stream: BoxStream<'static, anyhow::Result<EventFrame>>,
) -> BoxStream<'static, anyhow::Result<RpcRequest<T>>> {
    stream
        .map(|frame| {
            let frame = frame?;
            Ok(RpcRequest::new(
                frame.rpc_id,
                serde_json::from_value(frame.payload)?,
            ))
        })
        .boxed()
}
