//! Newline-delimited JSON-RPC 2.0 over caller-owned asynchronous byte streams.

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

use futures::future::BoxFuture;
use parking_lot::{Mutex, RwLock};
use seekdeep_llm::AbortSignal;
use seekdeep_lossless_json::{JsonRef, JsonValue};
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader},
    sync::{Notify, oneshot},
    task::JoinHandle,
};

#[path = "wire.rs"]
mod wire;
pub use wire::JsonRpcRawResponseError;

/// Erased readable half owned by a line transport.
pub type BoxedJsonRpcInput = Pin<Box<dyn AsyncRead + Send + Unpin + 'static>>;
/// Erased writable half owned by a line transport.
pub type BoxedJsonRpcOutput = Pin<Box<dyn AsyncWrite + Send + Unpin + 'static>>;

/// Synchronous request-handler factory; the returned future produces the result field.
pub type JsonRpcRequestHandler = Arc<
    dyn Fn(String, Map<String, Value>) -> BoxFuture<'static, anyhow::Result<Value>> + Send + Sync,
>;
/// Synchronous notification observer.
pub type JsonRpcNotificationHandler = Arc<dyn Fn(String, Map<String, Value>) + Send + Sync>;
/// Request-handler factory preserving raw JSON strings and object keys.
pub type JsonRpcJsonRequestHandler =
    Arc<dyn Fn(String, JsonValue) -> BoxFuture<'static, anyhow::Result<JsonValue>> + Send + Sync>;
/// Notification observer preserving raw JSON strings and object keys.
pub type JsonRpcJsonNotificationHandler = Arc<dyn Fn(String, JsonValue) + Send + Sync>;
/// External input failure observer used by protocol-specific owners.
pub type JsonRpcTransportFailureHandler = Arc<dyn Fn(anyhow::Error) + Send + Sync>;
/// Observer invoked after one incoming request response has reached the output stream.
pub type JsonRpcResponseWrittenHandler = Arc<dyn Fn(String, bool) + Send + Sync>;

type PendingSender = oneshot::Sender<anyhow::Result<JsonValue>>;
type IncomingNotificationHandler =
    Arc<dyn Fn(String, JsonValue) -> anyhow::Result<()> + Send + Sync>;

/// JSON-RPC error response preserving its wire code and data.
#[derive(Clone, Debug, Error)]
#[error("{message}")]
pub struct JsonRpcResponseError {
    /// Numeric JSON-RPC code, when the peer supplied an integer.
    pub code: Option<i64>,
    /// Peer message or the stable fallback.
    pub message: String,
    /// Optional structured error payload.
    pub data: Option<Value>,
}

/// One bidirectional line-delimited endpoint.
pub struct JsonRpcLineTransport {
    input: Mutex<Option<BoxedJsonRpcInput>>,
    output: tokio::sync::Mutex<Option<BoxedJsonRpcOutput>>,
    started: AtomicBool,
    reader: Mutex<Option<JoinHandle<()>>>,
    next_request: AtomicU64,
    pending: Mutex<HashMap<String, PendingSender>>,
    /// Set under the pending-request lock by `close`, so no request registers after it.
    closed: AtomicBool,
    request_handler: RwLock<Option<JsonRpcJsonRequestHandler>>,
    notification_handler: RwLock<Option<IncomingNotificationHandler>>,
    failure_handler: RwLock<Option<JsonRpcTransportFailureHandler>>,
    response_written_handler: RwLock<Option<JsonRpcResponseWrittenHandler>>,
    incoming_requests: AtomicUsize,
    incoming_idle: Notify,
}

impl std::fmt::Debug for JsonRpcLineTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JsonRpcLineTransport")
            .field("started", &self.started.load(Ordering::Acquire))
            .field("pending", &self.pending.lock().len())
            .finish_non_exhaustive()
    }
}

impl JsonRpcLineTransport {
    /// Constructs an unstarted endpoint over caller-owned streams.
    #[must_use]
    pub fn new<R, W>(input: R, output: W) -> Arc<Self>
    where
        R: AsyncRead + Send + Unpin + 'static,
        W: AsyncWrite + Send + Unpin + 'static,
    {
        Self::from_boxed(Box::pin(input), Box::pin(output))
    }

    /// Constructs an unstarted endpoint from erased streams.
    #[must_use]
    pub fn from_boxed(input: BoxedJsonRpcInput, output: BoxedJsonRpcOutput) -> Arc<Self> {
        Arc::new(Self {
            input: Mutex::new(Some(input)),
            output: tokio::sync::Mutex::new(Some(output)),
            started: AtomicBool::new(false),
            reader: Mutex::new(None),
            next_request: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            request_handler: RwLock::new(None),
            notification_handler: RwLock::new(None),
            failure_handler: RwLock::new(None),
            response_written_handler: RwLock::new(None),
            incoming_requests: AtomicUsize::new(0),
            incoming_idle: Notify::new(),
        })
    }

    /// Installs a checked ordinary-value request handler.
    /// Unrepresentable parameters produce an error response before the handler runs.
    pub fn on_request(&self, handler: JsonRpcRequestHandler) {
        self.on_request_json(Arc::new(
            move |method, params| -> BoxFuture<'static, anyhow::Result<JsonValue>> {
                let params = match wire::ordinary_params(params) {
                    Ok(params) => params,
                    Err(error) => return Box::pin(std::future::ready(Err(error))),
                };
                let response = handler(method, params);
                Box::pin(async move { response.await.map(JsonValue::from) })
            },
        ));
    }

    /// Installs or replaces the lossless incoming-request handler.
    /// Missing or non-object parameters normalize to an empty object.
    pub fn on_request_json(&self, handler: JsonRpcJsonRequestHandler) {
        *self.request_handler.write() = Some(handler);
    }

    /// Installs a checked ordinary-value notification observer.
    /// Unrepresentable parameters reach the input-failure observer instead of the callback.
    pub fn on_notification(&self, handler: JsonRpcNotificationHandler) {
        *self.notification_handler.write() = Some(Arc::new(move |method, params| {
            handler(method, wire::ordinary_params(params)?);
            Ok(())
        }));
    }

    /// Installs or replaces the lossless notification observer.
    /// Missing or non-object parameters normalize to an empty object.
    pub fn on_notification_json(&self, handler: JsonRpcJsonNotificationHandler) {
        *self.notification_handler.write() = Some(Arc::new(move |method, params| {
            handler(method, params);
            Ok(())
        }));
    }

    /// Installs or replaces the external input-failure observer.
    pub fn on_input_failure(&self, handler: JsonRpcTransportFailureHandler) {
        *self.failure_handler.write() = Some(handler);
    }

    /// Installs or replaces the post-response-write observer.
    pub fn on_response_written(&self, handler: JsonRpcResponseWrittenHandler) {
        *self.response_written_handler.write() = Some(handler);
    }

    /// Waits until every request already accepted by the reader has written its response.
    pub async fn when_incoming_idle(&self) {
        loop {
            let notified = self.incoming_idle.notified();
            if self.incoming_requests.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }

    /// Starts consuming input frames. Repeated calls are no-ops.
    pub fn start(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(input) = self.input.lock().take() else {
            return;
        };
        let transport = Arc::clone(self);
        *self.reader.lock() = Some(tokio::spawn(async move {
            transport.read_loop(input).await;
        }));
    }

    /// Sends one request and waits for its matching result or cancellation.
    ///
    /// # Errors
    ///
    /// Returns pre-write cancellation, output I/O, transport closure, peer response,
    /// response-channel failures, or a result that ordinary JSON values cannot represent.
    pub async fn request(
        self: &Arc<Self>,
        method: impl Into<String>,
        params: Map<String, Value>,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Value> {
        wire::ordinary_response(
            self.request_json(method, JsonValue::from(Value::Object(params)), signal)
                .await,
        )
    }

    /// Sends raw JSON parameters and retains the complete result or peer error.
    ///
    /// # Errors
    /// Returns cancellation, I/O, transport closure, response-channel failure, or
    /// [`JsonRpcRawResponseError`] without converting application values to UTF-8.
    pub async fn request_json(
        self: &Arc<Self>,
        method: impl Into<String>,
        params: JsonValue,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<JsonValue> {
        self.request_inner(method.into(), params, signal, None)
            .await
    }

    /// Sends one request and emits a correlated protocol cancellation notification
    /// when the caller signal wins.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::request`]. Cancellation-notification
    /// write failure is deliberately secondary to the caller's abort outcome.
    pub async fn request_with_cancellation(
        self: &Arc<Self>,
        method: impl Into<String>,
        params: Map<String, Value>,
        signal: AbortSignal,
        cancellation_method: impl Into<String>,
    ) -> anyhow::Result<Value> {
        wire::ordinary_response(
            self.request_json_with_cancellation(
                method,
                JsonValue::from(Value::Object(params)),
                signal,
                cancellation_method,
            )
            .await,
        )
    }

    /// Sends raw JSON and emits a correlated cancellation notification when aborted.
    ///
    /// # Errors
    /// Returns the same failures as [`Self::request_json`]; cancellation-write failures
    /// remain secondary to the caller's abort outcome.
    pub async fn request_json_with_cancellation(
        self: &Arc<Self>,
        method: impl Into<String>,
        params: JsonValue,
        signal: AbortSignal,
        cancellation_method: impl Into<String>,
    ) -> anyhow::Result<JsonValue> {
        self.request_inner(
            method.into(),
            params,
            Some(signal),
            Some(cancellation_method.into()),
        )
        .await
    }

    async fn request_inner(
        self: &Arc<Self>,
        method: String,
        params: JsonValue,
        signal: Option<AbortSignal>,
        cancellation_method: Option<String>,
    ) -> anyhow::Result<JsonValue> {
        if let Some(signal) = signal.as_ref()
            && signal.is_aborted()
        {
            return Err(abort_error(signal));
        }
        let id = format!(
            "req_{:016x}",
            self.next_request.fetch_add(1, Ordering::AcqRel)
        );
        let (sender, receiver) = oneshot::channel();
        {
            // Registration and closure share one lock: a request that starts after
            // `close` fails here instead of waiting on a reader that no longer exists.
            let mut pending = self.pending.lock();
            anyhow::ensure!(
                !self.closed.load(Ordering::Acquire),
                "JSON-RPC transport closed"
            );
            pending.insert(id.clone(), sender);
        }
        // The frame write runs as its own task so that a full pipe (a peer that stopped
        // reading) cannot hold the caller past its cancellation: the caller stops waiting
        // and forgets the correlation while the write still completes whole, which keeps
        // the line stream well-formed for later frames.
        let writer = Arc::clone(self);
        let frame = wire::request(&id, &method, params);
        let write = tokio::spawn(async move { writer.write_frame(frame).await });
        let written = match signal.as_ref() {
            Some(signal) => tokio::select! {
                biased;
                result = write => result,
                () = signal.cancelled() => {
                    self.pending.lock().remove(&id);
                    return Err(abort_error(signal));
                }
            },
            None => write.await,
        };
        if let Err(error) = written
            .map_err(anyhow::Error::from)
            .and_then(|result| result)
        {
            self.pending.lock().remove(&id);
            return Err(error);
        }
        let response = async {
            receiver
                .await
                .map_err(|_| anyhow::anyhow!("JSON-RPC response channel closed"))?
        };
        let Some(signal) = signal else {
            return response.await;
        };
        tokio::select! {
            biased;
            result = response => result,
            () = signal.cancelled() => {
                self.pending.lock().remove(&id);
                if let Some(cancellation_method) = cancellation_method {
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(1),
                        self.notify(
                            cancellation_method,
                            Some(Map::from_iter([
                                ("requestId".to_owned(), Value::String(id)),
                                (
                                    "reason".to_owned(),
                                    Value::String("request cancelled".to_owned()),
                                ),
                            ])),
                        ),
                    )
                    .await;
                }
                Err(abort_error(&signal))
            }
        }
    }

    /// Sends one notification.
    ///
    /// # Errors
    ///
    /// Returns output serialization or I/O failures.
    pub async fn notify(
        &self,
        method: impl Into<String>,
        params: Option<Map<String, Value>>,
    ) -> anyhow::Result<()> {
        self.notify_json(
            method,
            params.map(|params| JsonValue::from(Value::Object(params))),
        )
        .await
    }

    /// Sends a notification whose parameters retain every JSON string and key.
    /// `None` omits the `params` member; `Some(null)` keeps explicit JSON null.
    ///
    /// # Errors
    /// Returns output serialization or I/O failures.
    pub async fn notify_json(
        &self,
        method: impl Into<String>,
        params: Option<JsonValue>,
    ) -> anyhow::Result<()> {
        let method = method.into();
        self.write_frame(wire::notification(&method, params)).await
    }

    /// Waits for every earlier frame write to reach the stream.
    ///
    /// # Errors
    ///
    /// Returns the underlying flush failure.
    pub async fn flush(&self) -> anyhow::Result<()> {
        let mut output = self.output.lock().await;
        let output = output
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("JSON-RPC output closed"))?;
        output.flush().await?;
        Ok(())
    }

    /// Stops reading and rejects every pending request. Idempotent.
    pub fn close(&self) {
        if let Some(reader) = self.reader.lock().take() {
            reader.abort();
        }
        let pending = {
            let mut pending = self.pending.lock();
            self.closed.store(true, Ordering::Release);
            std::mem::take(&mut *pending)
        };
        for sender in pending.into_values() {
            let _ = sender.send(Err(anyhow::anyhow!("JSON-RPC transport closed")));
        }
    }

    /// Delivers EOF on the caller-owned output stream. Idempotent writers may
    /// accept repeated calls.
    ///
    /// # Errors
    ///
    /// Returns the underlying shutdown failure.
    pub async fn shutdown_output(&self) -> anyhow::Result<()> {
        let output = self.output.lock().await.take();
        let Some(mut output) = output else {
            return Ok(());
        };
        output.shutdown().await?;
        Ok(())
    }

    /// Current request-correlation entries, exposed for invariant tests.
    #[doc(hidden)]
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.lock().len()
    }

    async fn read_loop(self: Arc<Self>, input: BoxedJsonRpcInput) {
        let mut reader = BufReader::new(input);
        let mut bytes = Vec::new();
        loop {
            bytes.clear();
            match reader.read_until(b'\n', &mut bytes).await {
                Ok(0) => {
                    self.input_failed(anyhow::anyhow!("JSON-RPC input closed"));
                    return;
                }
                Ok(_) => {
                    let line = String::from_utf8_lossy(&bytes);
                    let line = line.trim();
                    if !line.is_empty() {
                        self.handle_line(line);
                    }
                }
                Err(error) => {
                    self.input_failed(error.into());
                    return;
                }
            }
        }
    }

    fn handle_line(self: &Arc<Self>, line: &str) {
        let Ok(frame) = JsonValue::parse(line.to_owned()) else {
            return;
        };
        if !frame.as_ref().is_object() {
            return;
        }
        let id = frame
            .get("id")
            .filter(|id| wire::valid_id(*id))
            .map(JsonRef::to_owned);
        let method = frame
            .get("method")
            .filter(|method| method.is_string())
            .map(JsonRef::deserialize::<String>)
            .transpose();
        let method = match method {
            Ok(method) => method,
            Err(error) => {
                self.input_failed(
                    anyhow::Error::new(error)
                        .context("JSON-RPC method identifier cannot be represented as UTF-8"),
                );
                return;
            }
        };
        match (id, method) {
            (Some(id), Some(method)) => {
                let params = wire::object_params(frame.get("params"));
                self.handle_incoming_request(id, method, params);
            }
            (Some(id), None) => {
                if let Ok(id) = id.deserialize::<String>() {
                    self.handle_incoming_response(&id, &frame);
                }
            }
            (None, Some(method)) => {
                let handler = self.notification_handler.read().clone();
                if let Some(handler) = handler
                    && let Err(error) = handler(method, wire::object_params(frame.get("params")))
                {
                    self.input_failed(error);
                }
            }
            (None, None) => {}
        }
    }

    fn handle_incoming_request(self: &Arc<Self>, id: JsonValue, method: String, params: JsonValue) {
        let transport = Arc::clone(self);
        let method_for_observer = method.clone();
        self.incoming_requests.fetch_add(1, Ordering::AcqRel);
        let handler = self.request_handler.read().clone();
        let operation: Pin<Box<dyn Future<Output = anyhow::Result<JsonValue>> + Send>> =
            match handler {
                Some(handler) => handler(method, params),
                None => Box::pin(async move { Err(anyhow::anyhow!("method not found: {method}")) }),
            };
        tokio::spawn(async move {
            let response = operation.await;
            let succeeded = response.is_ok();
            let frame = wire::response(id, response);
            let write = transport.write_frame(frame).await;
            if transport.incoming_requests.fetch_sub(1, Ordering::AcqRel) == 1 {
                transport.incoming_idle.notify_waiters();
            }
            match write {
                Ok(()) => {
                    if let Some(handler) = transport.response_written_handler.read().clone() {
                        handler(method_for_observer, succeeded);
                    }
                }
                Err(error) => transport.input_failed(error),
            }
        });
    }

    fn handle_incoming_response(&self, id: &str, frame: &JsonValue) {
        let Some(pending) = self.pending.lock().remove(id) else {
            return;
        };
        let _ = pending.send(wire::decode_response(frame));
    }

    async fn write_frame(&self, frame: JsonValue) -> anyhow::Result<()> {
        let mut bytes = serde_json::to_vec(&frame)?;
        bytes.push(b'\n');
        let mut output = self.output.lock().await;
        let output = output
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("JSON-RPC output closed"))?;
        output.write_all(&bytes).await?;
        Ok(())
    }

    fn input_failed(&self, error: anyhow::Error) {
        let message = error.to_string();
        self.fail_pending(&message);
        if let Some(handler) = self.failure_handler.read().clone() {
            handler(error);
        }
    }

    fn fail_pending(&self, message: &str) {
        let pending = std::mem::take(&mut *self.pending.lock());
        for sender in pending.into_values() {
            let _ = sender.send(Err(anyhow::anyhow!(message.to_owned())));
        }
    }
}

fn abort_error(signal: &AbortSignal) -> anyhow::Error {
    if let Some(error) = signal.error_reason() {
        return anyhow::anyhow!(error.to_string());
    }
    let reason = signal.reason().unwrap_or(Value::Null);
    let rendered = match reason {
        Value::String(value) => value,
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(_) => String::new(),
        Value::Object(_) => "[object Object]".to_owned(),
    };
    anyhow::anyhow!("JSON-RPC request aborted: {rendered}")
}
