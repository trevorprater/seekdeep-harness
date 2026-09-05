//! Initialized, serialized, abortable query lifecycle for one language server.

use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_llm::AbortSignal;
use seekdeep_lsp::{
    LSP_DISPOSED, LSP_UNSUPPORTED_OPERATION, LspError, LspOperation, LspProviderQuery,
    LspQueryResult,
};
use seekdeep_subprocess::{ProcessId, SubprocessRuntime};
use seekdeep_util::timeout::deadline;
use serde_json::{Map, Value, json};

use crate::{
    ConnectionError, ConnectionRequest, ConnectionServerRequestHandler, ConnectionSpec,
    ConnectionWriter, HostSource, LspConnection, WireInitializeResult, WireServerCapabilities,
    abort_error, abortable, negotiate_position_encoding, normalize_hover, normalize_locations,
    request_method, supports_operation, supports_transient_open,
};

/// Complete launch, initialization, query, and teardown configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct InstanceSpec {
    /// Resolved absolute executable path.
    pub command: String,
    /// Executable arguments.
    pub args: Vec<String>,
    /// Canonical subprocess working directory.
    pub cwd: PathBuf,
    /// Explicit child environment.
    pub env: BTreeMap<String, String>,
    /// Largest framed server message accepted.
    pub max_message_bytes: usize,
    /// Largest stderr tail retained.
    pub max_stderr_bytes: usize,
    /// TERM-to-KILL and cancellation grace in milliseconds.
    pub kill_grace_ms: f64,
    /// Static response value for each `workspace/configuration` item.
    pub configuration: Option<Value>,
    /// Canonical workspace file URI.
    pub workspace_uri: String,
    /// Static options forwarded in the initialize request.
    pub initialization_options: Option<Value>,
    /// Graceful `shutdown` and `exit` budget in milliseconds.
    pub shutdown_timeout_ms: f64,
}

impl InstanceSpec {
    fn connection_spec(&self) -> ConnectionSpec {
        ConnectionSpec {
            command: self.command.clone(),
            args: self.args.clone(),
            cwd: self.cwd.clone(),
            env: self.env.clone(),
            max_message_bytes: self.max_message_bytes,
            max_stderr_bytes: self.max_stderr_bytes,
            kill_grace_ms: self.kill_grace_ms,
            configuration: self.configuration.clone(),
        }
    }
}

#[derive(Clone, Debug)]
enum StoredFailure {
    Connection(ConnectionError),
    Lsp { message: String, code: &'static str },
    Message(String),
}

impl StoredFailure {
    fn from_error(error: &anyhow::Error) -> Self {
        if let Some(error) = error.downcast_ref::<ConnectionError>() {
            return Self::Connection(error.clone());
        }
        if let Some(error) = error.downcast_ref::<LspError>() {
            return Self::Lsp {
                message: error.message().to_owned(),
                code: error.code(),
            };
        }
        Self::Message(error.to_string())
    }

    fn into_error(self) -> anyhow::Error {
        match self {
            Self::Connection(error) => anyhow::Error::new(error),
            Self::Lsp { message, code } => anyhow::Error::new(LspError::new(message, code)),
            Self::Message(message) => anyhow::anyhow!(message),
        }
    }
}

#[derive(Debug, Default)]
struct ReadyState {
    outcome: Mutex<Option<Result<WireServerCapabilities, StoredFailure>>>,
    notify: tokio::sync::Notify,
}

impl ReadyState {
    fn complete(&self, outcome: Result<WireServerCapabilities, StoredFailure>) {
        let mut current = self.outcome.lock();
        if current.is_none() {
            *current = Some(outcome);
            self.notify.notify_waiters();
        }
    }

    async fn wait(&self) -> Result<WireServerCapabilities, StoredFailure> {
        loop {
            let notified = self.notify.notified();
            if let Some(outcome) = self.outcome.lock().clone() {
                return outcome;
            }
            notified.await;
        }
    }
}

#[derive(Debug, Default)]
struct TeardownState {
    started: bool,
    outcome: Option<Result<(), StoredFailure>>,
}

struct InstanceInner {
    spec: Arc<InstanceSpec>,
    connection: LspConnection,
    ready: Arc<ReadyState>,
    queue: tokio::sync::Mutex<()>,
    disposed: AtomicBool,
    teardown: Mutex<TeardownState>,
    teardown_notify: tokio::sync::Notify,
}

/// One initialized `(provider, canonical workspace)` language-server process.
#[derive(Clone)]
pub struct LspInstance {
    inner: Arc<InstanceInner>,
}

impl std::fmt::Debug for LspInstance {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LspInstance")
            .field("pid", &self.pid())
            .field("dead", &self.dead())
            .finish_non_exhaustive()
    }
}

impl LspInstance {
    /// Spawns the connection and begins initialization immediately.
    ///
    /// # Errors
    ///
    /// Returns synchronous subprocess or connection setup failures.
    pub fn new(
        spec: InstanceSpec,
        spawner: &dyn SubprocessRuntime,
        writer: Option<ConnectionWriter>,
    ) -> anyhow::Result<Self> {
        let spec = Arc::new(spec);
        let handler_spec = spec.clone();
        let handler: ConnectionServerRequestHandler = Arc::new(move |method, params| {
            let spec = handler_spec.clone();
            Box::pin(async move { answer_server_request(&spec, &method, params.as_ref()) })
        });
        let connection = LspConnection::new(&spec.connection_spec(), spawner, handler, writer)?;
        let inner = Arc::new(InstanceInner {
            spec,
            connection,
            ready: Arc::new(ReadyState::default()),
            queue: tokio::sync::Mutex::new(()),
            disposed: AtomicBool::new(false),
            teardown: Mutex::new(TeardownState::default()),
            teardown_notify: tokio::sync::Notify::new(),
        });
        spawn_initialize(inner.clone());
        Ok(Self { inner })
    }

    /// Direct child process identity.
    #[must_use]
    pub fn pid(&self) -> ProcessId {
        self.inner.connection.pid()
    }

    /// True after process close, fatal transport failure, or teardown start.
    #[must_use]
    pub fn dead(&self) -> bool {
        self.inner.disposed.load(Ordering::Acquire) || self.inner.connection.failed()
    }

    /// Tests whether an error is the connection's exact fatal transport cause.
    #[must_use]
    pub fn is_transport_failure(&self, error: &anyhow::Error) -> bool {
        self.inner.connection.failed_with(error)
    }

    /// Runs one source-prevalidated semantic query through the serialized lifecycle.
    ///
    /// # Errors
    ///
    /// Returns cancellation, initialization, capability, transport, protocol,
    /// normalization, or disposed-instance failures.
    pub async fn query(
        &self,
        request: LspProviderQuery,
        source: HostSource,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LspQueryResult> {
        self.inner.clone().query(request, source, signal).await
    }

    /// Runs the shared idempotent teardown transaction to process-tree quiescence.
    ///
    /// # Errors
    ///
    /// Returns an unexpected teardown primitive failure.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        self.inner.clone().start_teardown().await
    }

    pub(crate) fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl InstanceInner {
    async fn query(
        self: Arc<Self>,
        request: LspProviderQuery,
        source: HostSource,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LspQueryResult> {
        let _guard = self.wait_for_queue(signal).await?;
        let outcome = self.clone().run_query(request, source, signal).await;
        if let Err(error) = &outcome
            && self.connection.failed_with(error)
        {
            self.clone().start_teardown().await?;
        }
        outcome
    }

    async fn wait_for_queue(
        &self,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<tokio::sync::MutexGuard<'_, ()>> {
        let Some(signal) = signal else {
            return Ok(self.queue.lock().await);
        };
        if signal.is_aborted() {
            return Err(abort_error(signal));
        }
        tokio::select! {
            biased;
            guard = self.queue.lock() => Ok(guard),
            () = signal.cancelled() => Err(abort_error(signal)),
        }
    }

    async fn run_query(
        self: Arc<Self>,
        request: LspProviderQuery,
        source: HostSource,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LspQueryResult> {
        if self.disposed.load(Ordering::Acquire) {
            return Err(LspError::new("LSP instance was disposed", LSP_DISPOSED).into());
        }
        if let Some(signal) = signal.filter(|signal| signal.is_aborted()) {
            return Err(abort_error(signal));
        }
        let capabilities = match self.wait_ready(signal).await {
            Ok(capabilities) => capabilities,
            Err(error) => {
                if !self.dead() {
                    self.clone().start_teardown().await?;
                }
                return Err(error);
            }
        };
        if !supports_operation(&capabilities, request.operation) {
            return Err(LspError::new(
                format!("server does not support {}", request.operation.as_str()),
                LSP_UNSUPPORTED_OPERATION,
            )
            .into());
        }
        if !supports_transient_open(capabilities.text_document_sync.as_ref()) {
            return Err(LspError::new(
                "server does not support the transient textDocument/didOpen this host requires",
                LSP_UNSUPPORTED_OPERATION,
            )
            .into());
        }

        let uri = source.file_url;
        if let Some(signal) = signal.filter(|signal| signal.is_aborted()) {
            return Err(abort_error(signal));
        }
        let open = self.connection.notify(
            "textDocument/didOpen",
            Some(json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": request.language_id,
                    "version": 1,
                    "text": source.text,
                }
            })),
        );
        if let Err(error) = abortable(open, signal).await {
            self.clone().start_teardown().await?;
            return Err(error);
        }

        let outcome = self
            .send_request(request.operation, &uri, request.position, signal)
            .await
            .and_then(|payload| self.normalize(request.operation, payload.as_ref()));
        if !self.dead()
            && self
                .connection
                .notify(
                    "textDocument/didClose",
                    Some(json!({"textDocument": {"uri": uri}})),
                )
                .await
                .is_err()
        {
            let _ = self.clone().start_teardown().await;
        }
        outcome
    }

    async fn wait_ready(
        &self,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<WireServerCapabilities> {
        let outcome = if let Some(signal) = signal {
            if signal.is_aborted() {
                return Err(abort_error(signal));
            }
            tokio::select! {
                biased;
                outcome = self.ready.wait() => outcome,
                () = signal.cancelled() => return Err(abort_error(signal)),
            }
        } else {
            self.ready.wait().await
        };
        outcome.map_err(StoredFailure::into_error)
    }

    async fn send_request(
        self: &Arc<Self>,
        operation: LspOperation,
        uri: &str,
        position: seekdeep_lsp::LspPosition,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<Option<Value>> {
        let mut params = Map::from_iter([
            ("textDocument".to_owned(), json!({"uri": uri})),
            ("position".to_owned(), serde_json::to_value(position)?),
        ]);
        if operation == LspOperation::FindReferences {
            params.insert("context".to_owned(), json!({"includeDeclaration": true}));
        }
        let request_id = self.connection.peek_next_id();
        let request = self
            .connection
            .request(request_method(operation), Some(Value::Object(params)));
        match signal {
            Some(signal) => self.race_abort(request, request_id, signal).await,
            None => request.await,
        }
    }

    async fn race_abort(
        self: &Arc<Self>,
        request: ConnectionRequest,
        request_id: u64,
        signal: &AbortSignal,
    ) -> anyhow::Result<Option<Value>> {
        if signal.is_aborted() {
            return Err(abort_error(signal));
        }
        let first_wait = request.clone();
        tokio::select! {
            biased;
            outcome = first_wait.wait() => return outcome,
            () = signal.cancelled() => {}
        }
        let error = abort_error(signal);
        self.connection.cancel(request_id);
        let grace = deadline(None, self.spec.kill_grace_ms, "LSP_CANCEL_GRACE")?;
        let settled = tokio::select! {
            biased;
            _ = request.wait() => true,
            () = grace.signal.cancelled() => false,
        };
        if !settled {
            self.clone().start_teardown().await?;
        }
        Err(error)
    }

    fn normalize(
        &self,
        operation: LspOperation,
        payload: Option<&Value>,
    ) -> anyhow::Result<LspQueryResult> {
        if operation == LspOperation::Hover {
            return Ok(LspQueryResult::Hover {
                hover: normalize_hover(payload)?,
            });
        }
        Ok(LspQueryResult::Locations {
            locations: normalize_locations(payload)?,
            resolved_workspace_uri: self.spec.workspace_uri.clone(),
        })
    }

    fn dead(&self) -> bool {
        self.disposed.load(Ordering::Acquire) || self.connection.failed()
    }

    async fn start_teardown(self: Arc<Self>) -> anyhow::Result<()> {
        self.disposed.store(true, Ordering::Release);
        let start = {
            let mut state = self.teardown.lock();
            if state.started {
                false
            } else {
                state.started = true;
                true
            }
        };
        if start {
            let inner = self.clone();
            tokio::spawn(async move {
                let outcome = inner.clone().tear_down().await;
                inner.complete_teardown(outcome);
            });
        }
        self.wait_teardown().await
    }

    async fn tear_down(self: Arc<Self>) -> anyhow::Result<()> {
        if let Ok(shutdown) = deadline(None, self.spec.shutdown_timeout_ms, "LSP_SHUTDOWN") {
            let _ = self.graceful_shutdown(&shutdown.signal).await;
        }
        self.force_terminate().await
    }

    async fn graceful_shutdown(&self, signal: &AbortSignal) -> anyhow::Result<()> {
        let shutdown = self.connection.request("shutdown", Some(Value::Null));
        wait_request_with_signal(shutdown, signal).await?;
        self.connection.notify("exit", Some(Value::Null)).await?;
        wait_closed_with_signal(&self.connection, signal).await
    }

    async fn force_terminate(&self) -> anyhow::Result<()> {
        self.connection.terminate();
        let ((), exited) = tokio::join!(
            self.connection.closed(),
            self.connection.wait_for_process_tree_exit(None),
        );
        anyhow::ensure!(exited?, "language-server process tree did not exit");
        Ok(())
    }

    fn complete_teardown(&self, outcome: anyhow::Result<()>) {
        let outcome = outcome.map_err(|error| StoredFailure::from_error(&error));
        self.teardown.lock().outcome = Some(outcome);
        self.teardown_notify.notify_waiters();
    }

    async fn wait_teardown(&self) -> anyhow::Result<()> {
        loop {
            let notified = self.teardown_notify.notified();
            if let Some(outcome) = self.teardown.lock().outcome.clone() {
                return outcome.map_err(StoredFailure::into_error);
            }
            notified.await;
        }
    }
}

fn spawn_initialize(inner: Arc<InstanceInner>) {
    tokio::spawn(async move {
        let outcome = initialize(&inner)
            .await
            .map_err(|error| StoredFailure::from_error(&error));
        inner.ready.complete(outcome);
    });
}

async fn initialize(inner: &InstanceInner) -> anyhow::Result<WireServerCapabilities> {
    let mut params = Map::from_iter([
        ("processId".to_owned(), Value::Null),
        (
            "rootUri".to_owned(),
            Value::String(inner.spec.workspace_uri.clone()),
        ),
        (
            "workspaceFolders".to_owned(),
            json!([{"uri": inner.spec.workspace_uri, "name": "workspace"}]),
        ),
        ("capabilities".to_owned(), client_capabilities()),
    ]);
    if let Some(options) = inner.spec.initialization_options.clone() {
        params.insert("initializationOptions".to_owned(), options);
    }
    let result = inner
        .connection
        .request("initialize", Some(Value::Object(params)))
        .await?
        .ok_or_else(|| anyhow::anyhow!("LSP initialize result was missing"))?;
    let result: WireInitializeResult = serde_json::from_value(result)
        .map_err(|error| anyhow::anyhow!("LSP initialize result was malformed: {error}"))?;
    negotiate_position_encoding(result.capabilities.position_encoding.as_deref())?;
    inner
        .connection
        .notify("initialized", Some(json!({})))
        .await?;
    Ok(result.capabilities)
}

fn answer_server_request(
    spec: &InstanceSpec,
    method: &str,
    params: Option<&Value>,
) -> anyhow::Result<Option<Value>> {
    if method == "workspace/configuration" {
        let count = params
            .and_then(|params| params.get("items"))
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        return Ok(Some(Value::Array(
            (0..count)
                .map(|_| spec.configuration.clone().unwrap_or(Value::Null))
                .collect(),
        )));
    }
    if matches!(
        method,
        "window/workDoneProgress/create"
            | "client/registerCapability"
            | "client/unregisterCapability"
    ) {
        return Ok(Some(Value::Null));
    }
    if method == "workspace/applyEdit" {
        anyhow::bail!("workspace/applyEdit is not permitted by this host");
    }
    anyhow::bail!("unsupported server request: {method}")
}

async fn wait_request_with_signal(
    request: ConnectionRequest,
    signal: &AbortSignal,
) -> anyhow::Result<Option<Value>> {
    if signal.is_aborted() {
        return Err(abort_error(signal));
    }
    tokio::select! {
        biased;
        outcome = request.wait() => outcome,
        () = signal.cancelled() => Err(abort_error(signal)),
    }
}

async fn wait_closed_with_signal(
    connection: &LspConnection,
    signal: &AbortSignal,
) -> anyhow::Result<()> {
    if signal.is_aborted() {
        return Err(abort_error(signal));
    }
    tokio::select! {
        biased;
        () = connection.closed() => Ok(()),
        () = signal.cancelled() => Err(abort_error(signal)),
    }
}

fn client_capabilities() -> Value {
    json!({
        "general": {"positionEncodings": ["utf-16"]},
        "workspace": {"workspaceFolders": true, "configuration": true},
        "textDocument": {
            "synchronization": {"dynamicRegistration": false},
            "hover": {"contentFormat": ["markdown", "plaintext"]},
            "definition": {"linkSupport": true},
            "implementation": {"linkSupport": true},
            "references": {},
        },
    })
}
