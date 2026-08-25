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
use serde_json::{Map, Value, json};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader},
    sync::{Notify, oneshot},
    task::JoinHandle,
};

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
/// External input failure observer used by protocol-specific owners.
pub type JsonRpcTransportFailureHandler = Arc<dyn Fn(anyhow::Error) + Send + Sync>;
/// Observer invoked after one incoming request response has reached the output stream.
pub type JsonRpcResponseWrittenHandler = Arc<dyn Fn(String, bool) + Send + Sync>;

type PendingSender = oneshot::Sender<anyhow::Result<Value>>;

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
    output: tokio::sync::Mutex<BoxedJsonRpcOutput>,
    started: AtomicBool,
    reader: Mutex<Option<JoinHandle<()>>>,
    next_request: AtomicU64,
    pending: Mutex<HashMap<String, PendingSender>>,
    request_handler: RwLock<Option<JsonRpcRequestHandler>>,
    notification_handler: RwLock<Option<JsonRpcNotificationHandler>>,
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
            output: tokio::sync::Mutex::new(output),
            started: AtomicBool::new(false),
            reader: Mutex::new(None),
            next_request: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            request_handler: RwLock::new(None),
            notification_handler: RwLock::new(None),
            failure_handler: RwLock::new(None),
            response_written_handler: RwLock::new(None),
            incoming_requests: AtomicUsize::new(0),
            incoming_idle: Notify::new(),
        })
    }

    /// Installs or replaces the incoming-request handler.
    pub fn on_request(&self, handler: JsonRpcRequestHandler) {
        *self.request_handler.write() = Some(handler);
    }

    /// Installs or replaces the notification observer.
    pub fn on_notification(&self, handler: JsonRpcNotificationHandler) {
        *self.notification_handler.write() = Some(handler);
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
    /// or response-channel failures.
    pub async fn request(
        self: &Arc<Self>,
        method: impl Into<String>,
        params: Map<String, Value>,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Value> {
        if let Some(signal) = signal.as_ref()
            && signal.is_aborted()
        {
            return Err(abort_error(signal));
        }
        let id = format!(
            "req_{:016x}",
            self.next_request.fetch_add(1, Ordering::AcqRel)
        );
        let method = method.into();
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().insert(id.clone(), sender);
        if let Err(error) = self
            .write_frame(json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }))
            .await
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
        let method = method.into();
        let frame = params.map_or_else(
            || json!({"jsonrpc":"2.0", "method": method}),
            |params| json!({"jsonrpc":"2.0", "method": method, "params": params}),
        );
        self.write_frame(frame).await
    }

    /// Waits for every earlier frame write to reach the stream.
    ///
    /// # Errors
    ///
    /// Returns the underlying flush failure.
    pub async fn flush(&self) -> anyhow::Result<()> {
        self.output.lock().await.flush().await?;
        Ok(())
    }

    /// Stops reading and rejects every pending request. Idempotent.
    pub fn close(&self) {
        if let Some(reader) = self.reader.lock().take() {
            reader.abort();
        }
        self.fail_pending("JSON-RPC transport closed");
    }

    /// Delivers EOF on the caller-owned output stream. Idempotent writers may
    /// accept repeated calls.
    ///
    /// # Errors
    ///
    /// Returns the underlying shutdown failure.
    pub async fn shutdown_output(&self) -> anyhow::Result<()> {
        self.output.lock().await.shutdown().await?;
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
        let Ok(Value::Object(mut frame)) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let id = frame.get("id").filter(|id| valid_id(id)).cloned();
        let method = frame
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        match (id, method) {
            (Some(id), Some(method)) => {
                let params = object_params(frame.remove("params"));
                self.handle_incoming_request(id, method, params);
            }
            (Some(Value::String(id)), None) => self.handle_incoming_response(&id, &frame),
            (None, Some(method)) => {
                if let Some(handler) = self.notification_handler.read().clone() {
                    handler(method, object_params(frame.remove("params")));
                }
            }
            (Some(_) | None, None) => {}
        }
    }

    fn handle_incoming_request(
        self: &Arc<Self>,
        id: Value,
        method: String,
        params: Map<String, Value>,
    ) {
        let transport = Arc::clone(self);
        let method_for_observer = method.clone();
        self.incoming_requests.fetch_add(1, Ordering::AcqRel);
        let handler = self.request_handler.read().clone();
        let operation: Pin<Box<dyn Future<Output = anyhow::Result<Value>> + Send>> = match handler {
            Some(handler) => handler(method, params),
            None => Box::pin(async move { Err(anyhow::anyhow!("method not found: {method}")) }),
        };
        tokio::spawn(async move {
            let response = operation.await;
            let succeeded = response.is_ok();
            let frame = match response {
                Ok(result) => json!({"jsonrpc":"2.0", "id": id, "result": result}),
                Err(error) => {
                    let code = if error.to_string().starts_with("method not found: ") {
                        -32601
                    } else {
                        -32603
                    };
                    json!({"jsonrpc":"2.0", "id": id, "error": {"code":code, "message":error.to_string()}})
                }
            };
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

    fn handle_incoming_response(&self, id: &str, frame: &Map<String, Value>) {
        let Some(pending) = self.pending.lock().remove(id) else {
            return;
        };
        let result = frame.get("error").and_then(Value::as_object).map_or_else(
            || Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
            |error| {
                Err(JsonRpcResponseError {
                    code: error.get("code").and_then(Value::as_i64),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("JSON-RPC error")
                        .to_owned(),
                    data: error.get("data").cloned(),
                }
                .into())
            },
        );
        let _ = pending.send(result);
    }

    async fn write_frame(&self, frame: Value) -> anyhow::Result<()> {
        let mut bytes = serde_json::to_vec(&frame)?;
        bytes.push(b'\n');
        self.output.lock().await.write_all(&bytes).await?;
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

fn valid_id(value: &Value) -> bool {
    value.is_string() || value.is_number()
}

fn object_params(value: Option<Value>) -> Map<String, Value> {
    value
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
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
