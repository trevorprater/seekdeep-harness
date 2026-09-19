//! JSON-RPC correlation and process ownership for one stdio language server.

use std::{
    collections::{BTreeMap, HashMap},
    error::Error,
    fmt,
    future::{Future, IntoFuture},
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_llm::AbortSignal;
use seekdeep_subprocess::{
    ProcessId, SubprocessCollect, SubprocessEnvironment, SubprocessHandleRef, SubprocessInput,
    SubprocessOutput, SubprocessOutputMode, SubprocessRuntime, SubprocessSpawnSpec,
    SubprocessStdinMode, SubprocessStdio,
};
use serde_json::{Map, Value, json};
use tokio::io::AsyncReadExt as _;

use crate::framing::{MessageDecoder, encode_message};

/// How to launch one server and bound its protocol and diagnostics.
#[derive(Clone, Debug, PartialEq)]
pub struct ConnectionSpec {
    /// Resolved absolute executable path, invoked without a shell.
    pub command: String,
    /// Arguments passed to the executable.
    pub args: Vec<String>,
    /// Canonical workspace used as the child working directory.
    pub cwd: PathBuf,
    /// Explicit child environment overrides.
    pub env: BTreeMap<String, String>,
    /// Largest framed server message accepted.
    pub max_message_bytes: usize,
    /// Largest stderr tail retained for diagnostics.
    pub max_stderr_bytes: usize,
    /// TERM-to-KILL and inherited-pipe grace in milliseconds.
    pub kill_grace_ms: f64,
    /// Static configuration retained for the instance's request handler.
    pub configuration: Option<Value>,
}

/// Owned future returned by an injected connection writer.
pub type ConnectionWriteFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'static>>;
/// Injectable framed-message writer used by transport conformance tests.
pub type ConnectionWriter =
    Arc<dyn Fn(SubprocessInput, Value) -> ConnectionWriteFuture + Send + Sync>;
/// Owned future returned by a server-to-client request handler.
pub type ConnectionServerRequestFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<Option<Value>>> + Send + 'static>>;
/// Handler for one server-to-client request. `None` preserves missing params or result.
pub type ConnectionServerRequestHandler =
    Arc<dyn Fn(String, Option<Value>) -> ConnectionServerRequestFuture + Send + Sync>;

#[derive(Debug)]
struct ConnectionFailure {
    message: String,
}

/// Cloneable request or transport failure preserving exact connection identity.
#[derive(Clone, Debug)]
pub struct ConnectionError(Arc<ConnectionFailure>);

impl fmt::Display for ConnectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0.message)
    }
}

impl Error for ConnectionError {}

#[derive(Debug, Default)]
struct RequestState {
    outcome: Mutex<Option<Result<Option<Value>, ConnectionError>>>,
    notify: tokio::sync::Notify,
}

impl RequestState {
    fn complete(&self, outcome: Result<Option<Value>, ConnectionError>) {
        let mut current = self.outcome.lock();
        if current.is_none() {
            *current = Some(outcome);
            self.notify.notify_waiters();
        }
    }

    async fn wait(&self) -> Result<Option<Value>, ConnectionError> {
        loop {
            let notified = self.notify.notified();
            if let Some(outcome) = self.outcome.lock().clone() {
                return outcome;
            }
            notified.await;
        }
    }
}

/// Cloneable handle to one already-started correlated JSON-RPC request.
#[derive(Clone, Debug)]
pub struct ConnectionRequest {
    state: Arc<RequestState>,
}

impl ConnectionRequest {
    /// Waits for the shared response or failure without consuming other observers.
    ///
    /// # Errors
    ///
    /// Returns the server response error or the connection's retained fatal cause.
    pub async fn wait(&self) -> anyhow::Result<Option<Value>> {
        self.state.wait().await.map_err(anyhow::Error::new)
    }
}

impl IntoFuture for ConnectionRequest {
    type Output = anyhow::Result<Option<Value>>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'static>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move { self.wait().await })
    }
}

#[derive(Default)]
struct ConnectionState {
    pending: HashMap<u64, Arc<RequestState>>,
    close_reason: Option<Arc<ConnectionFailure>>,
}

struct ConnectionInner {
    handle: SubprocessHandleRef,
    stdin: SubprocessInput,
    decoder: Mutex<MessageDecoder>,
    state: Mutex<ConnectionState>,
    next_id: AtomicU64,
    closed: AtomicBool,
    closed_notify: tokio::sync::Notify,
    on_server_request: ConnectionServerRequestHandler,
    writer: ConnectionWriter,
}

/// A live JSON-RPC endpoint bound to one owned child process tree.
#[derive(Clone)]
pub struct LspConnection {
    inner: Arc<ConnectionInner>,
}

impl fmt::Debug for LspConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LspConnection")
            .field("pid", &self.pid())
            .field("failed", &self.failed())
            .finish_non_exhaustive()
    }
}

impl LspConnection {
    /// Spawns one server and starts its stdout and close monitors.
    ///
    /// # Errors
    ///
    /// Returns subprocess request validation, setup, or missing-pipe failures.
    pub fn new(
        spec: &ConnectionSpec,
        spawner: &dyn SubprocessRuntime,
        on_server_request: ConnectionServerRequestHandler,
        writer: Option<ConnectionWriter>,
    ) -> anyhow::Result<Self> {
        let mut argv = Vec::with_capacity(spec.args.len() + 1);
        argv.push(spec.command.clone());
        argv.extend(spec.args.iter().cloned());
        let handle = spawner.spawn(SubprocessSpawnSpec {
            argv,
            cwd: spec.cwd.clone(),
            stdio: SubprocessStdio {
                stdin: SubprocessStdinMode::Pipe,
                stdout: SubprocessOutputMode::Pipe,
                stderr: SubprocessOutputMode::Collect(SubprocessCollect {
                    #[allow(clippy::cast_precision_loss)]
                    max_bytes: spec.max_stderr_bytes as f64,
                    spill: None,
                }),
            },
            grace_ms: spec.kill_grace_ms,
            signal: None,
            env: Some(
                spec.env
                    .iter()
                    .map(|(key, value)| (key.clone(), Some(value.clone())))
                    .collect::<SubprocessEnvironment>(),
            ),
        })?;
        let stdin = match handle.stdin() {
            Some(stdin) => stdin,
            None if handle.pid().as_i64() == -1 => {
                SubprocessInput::new(Box::pin(tokio::io::sink()))
            }
            None => anyhow::bail!(
                "lsp-stdio: subprocess implementation dropped a piped protocol stream"
            ),
        };
        let stdout = match handle.stdout() {
            Some(stdout) => Some(stdout),
            None if handle.pid().as_i64() == -1 => None,
            None => anyhow::bail!(
                "lsp-stdio: subprocess implementation dropped a piped protocol stream"
            ),
        };
        let inner = Arc::new(ConnectionInner {
            handle,
            stdin,
            decoder: Mutex::new(MessageDecoder::new(spec.max_message_bytes)),
            state: Mutex::new(ConnectionState::default()),
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            closed_notify: tokio::sync::Notify::new(),
            on_server_request,
            writer: writer.unwrap_or_else(default_writer),
        });
        if let Some(stdout) = stdout {
            spawn_stdout_reader(inner.clone(), stdout);
        }
        spawn_close_monitor(inner.clone());
        Ok(Self { inner })
    }

    /// Direct child pid, including the spawn-failure sentinel `-1`.
    #[must_use]
    pub fn pid(&self) -> ProcessId {
        self.inner.handle.pid()
    }

    /// Retained stderr tail, decoded by the subprocess provider.
    #[must_use]
    pub fn stderr_tail(&self) -> String {
        self.inner
            .handle
            .collected()
            .stderr
            .map_or_else(String::new, |reader| reader.read_from(0).text)
    }

    /// Whether a fatal transport failure has been retained.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.inner.state.lock().close_reason.is_some()
    }

    /// Tests exact retained-failure identity for pool invalidation.
    #[must_use]
    pub fn failed_with(&self, error: &anyhow::Error) -> bool {
        let Some(error) = error.downcast_ref::<ConnectionError>() else {
            return false;
        };
        self.inner
            .state
            .lock()
            .close_reason
            .as_ref()
            .is_some_and(|reason| Arc::ptr_eq(reason, &error.0))
    }

    /// Sends one JSON-RPC request and returns its correlated response future.
    #[must_use]
    pub fn request(&self, method: impl Into<String>, params: Option<Value>) -> ConnectionRequest {
        let id = self.inner.next_id.fetch_add(1, Ordering::AcqRel);
        let request = Arc::new(RequestState::default());
        let write = {
            let mut state = self.inner.state.lock();
            if let Some(reason) = state.close_reason.clone() {
                request.complete(Err(ConnectionError(reason)));
                false
            } else {
                state.pending.insert(id, request.clone());
                true
            }
        };
        if write {
            let inner = self.inner.clone();
            let message = request_message(id, method.into(), params);
            tokio::spawn(async move {
                let _ = inner.write(message).await;
            });
        }
        ConnectionRequest { state: request }
    }

    /// Sends one JSON-RPC notification and awaits its write settlement.
    #[must_use]
    pub fn notify(
        &self,
        method: impl Into<String>,
        params: Option<Value>,
    ) -> ConnectionWriteFuture {
        let inner = self.inner.clone();
        let message = notification_message(method.into(), params);
        Box::pin(async move { inner.write(message).await.map_err(anyhow::Error::new) })
    }

    /// Best-effort `$/cancelRequest`; a closed or failed connection makes this a no-op.
    pub fn cancel(&self, request_id: u64) {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            let _ = inner
                .write(json!({
                    "jsonrpc": "2.0",
                    "method": "$/cancelRequest",
                    "params": {"id": request_id},
                }))
                .await;
        });
    }

    /// Numeric id assigned by the next [`Self::request`] call.
    #[must_use]
    pub fn peek_next_id(&self) -> u64 {
        self.inner.next_id.load(Ordering::Acquire)
    }

    /// Starts idempotent process-tree termination escalation.
    pub fn terminate(&self) {
        self.inner.handle.terminate();
    }

    /// Waits until the direct process close boundary settles.
    pub async fn closed(&self) {
        self.inner.wait_closed().await;
    }

    /// Waits for complete process-tree exit, optionally bounded by a signal.
    ///
    /// # Errors
    ///
    /// Returns provider liveness or cleanup failures.
    pub async fn wait_for_process_tree_exit(
        &self,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<bool> {
        self.inner.handle.wait_for_exit(signal.cloned()).await
    }
}

impl ConnectionInner {
    async fn write(&self, message: Value) -> Result<(), ConnectionError> {
        if let Some(reason) = self.state.lock().close_reason.clone() {
            return Err(ConnectionError(reason));
        }
        match (self.writer)(self.stdin.clone(), message).await {
            Ok(()) => Ok(()),
            Err(error) => {
                let failure = failure(error.to_string());
                self.fail(failure.clone());
                Err(ConnectionError(failure))
            }
        }
    }

    fn on_stdout(self: &Arc<Self>, chunk: &[u8]) {
        let messages = match self.decoder.lock().push(chunk) {
            Ok(messages) => messages,
            Err(error) => {
                self.fail(failure(error.to_string()));
                self.handle.terminate();
                return;
            }
        };
        for message in messages {
            self.dispatch(&message);
        }
    }

    fn dispatch(self: &Arc<Self>, message: &Value) {
        let Some(frame) = message.as_object() else {
            return;
        };
        let id = frame.get("id").cloned();
        let method = frame
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if let (Some(method), Some(id)) = (method.as_ref(), id.as_ref())
            && (id.is_number() || id.is_string())
        {
            let inner = self.clone();
            let method = method.clone();
            let id = id.clone();
            let params = frame.get("params").cloned();
            tokio::spawn(async move {
                inner.handle_server_request(id, method, params).await;
            });
            return;
        }
        if method.is_some() {
            return;
        }
        if let Some(id) = id.and_then(|id| id.as_u64()) {
            self.handle_response(id, frame);
        }
    }

    async fn handle_server_request(
        self: Arc<Self>,
        id: Value,
        method: String,
        params: Option<Value>,
    ) {
        match (self.on_server_request)(method, params).await {
            Ok(result) => {
                let message = response_message(id.clone(), result);
                if let Err(error) = self.write(message).await {
                    let message = error.to_string();
                    let _ = self.write(error_response_message(&id, &message)).await;
                }
            }
            Err(error) => {
                let message = error.to_string();
                let _ = self.write(error_response_message(&id, &message)).await;
            }
        }
    }

    fn handle_response(&self, id: u64, frame: &Map<String, Value>) {
        let pending = self.state.lock().pending.remove(&id);
        let Some(pending) = pending else {
            return;
        };
        let outcome = match frame.get("error") {
            Some(error) if error.is_object() || error.is_array() => {
                let message = error
                    .as_object()
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("LSP error response");
                Err(ConnectionError(failure(message)))
            }
            _ => Ok(frame.get("result").cloned()),
        };
        pending.complete(outcome);
    }

    fn fail(&self, failure: Arc<ConnectionFailure>) {
        let pending = {
            let mut state = self.state.lock();
            if state.close_reason.is_none() {
                state.close_reason = Some(failure.clone());
            }
            state
                .pending
                .drain()
                .map(|(_, sender)| sender)
                .collect::<Vec<_>>()
        };
        let error = ConnectionError(failure);
        for request in pending {
            request.complete(Err(error.clone()));
        }
    }

    fn close(&self) {
        let reason = {
            let mut state = self.state.lock();
            let reason = state.close_reason.clone().unwrap_or_else(|| {
                let reason = failure(self.exit_message());
                state.close_reason = Some(reason.clone());
                reason
            });
            let waiting = state
                .pending
                .drain()
                .map(|(_, sender)| sender)
                .collect::<Vec<_>>();
            (reason, waiting)
        };
        let error = ConnectionError(reason.0);
        for request in reason.1 {
            request.complete(Err(error.clone()));
        }
        self.closed.store(true, Ordering::Release);
        self.closed_notify.notify_waiters();
    }

    fn exit_message(&self) -> String {
        let tail = self
            .handle
            .collected()
            .stderr
            .map_or_else(String::new, |reader| reader.read_from(0).text);
        let tail = tail.trim();
        if tail.is_empty() {
            "language server exited".to_owned()
        } else {
            format!("language server exited; stderr: {tail}")
        }
    }

    async fn wait_closed(&self) {
        loop {
            let notified = self.closed_notify.notified();
            if self.closed.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

fn default_writer() -> ConnectionWriter {
    Arc::new(|stdin, message| {
        Box::pin(async move {
            let bytes = encode_message(&message)?;
            stdin.write_all(&bytes).await?;
            Ok(())
        })
    })
}

fn spawn_stdout_reader(inner: Arc<ConnectionInner>, stdout: SubprocessOutput) {
    tokio::spawn(async move {
        let mut stdout = stdout.lock().await;
        let mut buffer = vec![0_u8; 8192];
        loop {
            match stdout.read(&mut buffer).await {
                Ok(0) => return,
                Ok(read) => inner.on_stdout(&buffer[..read]),
                Err(error) => {
                    inner.fail(failure(error.to_string()));
                    inner.handle.terminate();
                    return;
                }
            }
        }
    });
}

fn spawn_close_monitor(inner: Arc<ConnectionInner>) {
    tokio::spawn(async move {
        if let Err(error) = inner.handle.done().await {
            inner.fail(failure(error.to_string()));
        }
        inner.close();
    });
}

fn failure(message: impl Into<String>) -> Arc<ConnectionFailure> {
    Arc::new(ConnectionFailure {
        message: message.into(),
    })
}

fn request_message(id: u64, method: String, params: Option<Value>) -> Value {
    let mut message = Map::from_iter([
        ("jsonrpc".to_owned(), Value::String("2.0".to_owned())),
        ("id".to_owned(), Value::from(id)),
        ("method".to_owned(), Value::String(method)),
    ]);
    if let Some(params) = params {
        message.insert("params".to_owned(), params);
    }
    Value::Object(message)
}

fn notification_message(method: String, params: Option<Value>) -> Value {
    let mut message = Map::from_iter([
        ("jsonrpc".to_owned(), Value::String("2.0".to_owned())),
        ("method".to_owned(), Value::String(method)),
    ]);
    if let Some(params) = params {
        message.insert("params".to_owned(), params);
    }
    Value::Object(message)
}

fn response_message(id: Value, result: Option<Value>) -> Value {
    let mut message = Map::from_iter([
        ("jsonrpc".to_owned(), Value::String("2.0".to_owned())),
        ("id".to_owned(), id),
    ]);
    if let Some(result) = result {
        message.insert("result".to_owned(), result);
    }
    Value::Object(message)
}

fn error_response_message(id: &Value, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": -32601, "message": message},
    })
}
