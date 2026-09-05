//! Low-level JSON-RPC client with process ownership and notification fan-out.

use std::{
    any::Any,
    collections::{HashMap, VecDeque},
    fmt::Write as _,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use parking_lot::Mutex;
use seekdeep_llm::{AbortSignal, ContentBlock};
use seekdeep_sdk_protocol::{
    InitializeParams, InitializeResult, JsonRpcLineTransport, JsonRpcResponseError,
    SessionPromptParams, SessionPromptResult,
};
use seekdeep_subprocess::{
    SubprocessCollect, SubprocessEnvironment, SubprocessHandleRef, SubprocessOutputMode,
    SubprocessOutputReaderHandle, SubprocessSpawnSpec, SubprocessStdinMode, SubprocessStdio,
    scrubbed_parent_env,
};
use seekdeep_subprocess_local::spawn_subprocess;
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::sync::Notify;

use crate::types::{HarnessClientOptions, HarnessNotification, NotificationFilter};

const STDERR_TAIL_LIMIT: usize = 400;

/// Runtime process or transport closure with exit and stderr context.
#[derive(Clone, Debug, Error)]
#[error("{message}")]
pub struct TransportClosedError {
    /// Complete diagnostic.
    pub message: String,
}

/// One request exceeded its configured timeout.
#[derive(Clone, Debug, Error)]
#[error("{message}")]
pub struct RequestTimeoutError {
    /// Method and duration diagnostic.
    pub message: String,
}

/// Runtime response violated the documented SDK protocol.
#[derive(Clone, Debug, Error)]
#[error("{message}")]
pub struct SdkProtocolError {
    /// Boundary diagnostic.
    pub message: String,
}

struct SubscriptionState {
    queue: Mutex<VecDeque<HarnessNotification>>,
    failure: Mutex<Option<SubscriptionFailure>>,
    filter: Option<NotificationFilter>,
    notify: Notify,
    closed: AtomicBool,
}

#[derive(Clone, Debug)]
enum SubscriptionFailure {
    Transport(TransportClosedError),
    Other(String),
}

impl SubscriptionFailure {
    fn to_error(&self) -> anyhow::Error {
        match self {
            Self::Transport(error) => anyhow::Error::new(error.clone()),
            Self::Other(error) => anyhow::Error::msg(error.clone()),
        }
    }
}

impl SubscriptionState {
    fn new(filter: Option<NotificationFilter>) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::new()),
            failure: Mutex::new(None),
            filter,
            notify: Notify::new(),
            closed: AtomicBool::new(false),
        })
    }

    fn fail(&self, failure: SubscriptionFailure, clear: bool) {
        if self.failure.lock().is_none() {
            *self.failure.lock() = Some(failure);
        }
        if clear {
            self.queue.lock().clear();
        }
        self.notify.notify_waiters();
    }

    fn push(&self, notification: &HarnessNotification) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        let matches = match self.filter.as_ref() {
            None => true,
            Some(filter) => {
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| filter(notification)));
                if let Err(payload) = &result {
                    self.closed.store(true, Ordering::Release);
                    self.fail(
                        SubscriptionFailure::Other(panic_message(payload.as_ref())),
                        false,
                    );
                    return false;
                }
                let Ok(matches) = result else {
                    unreachable!("filter panic returned above")
                };
                matches
            }
        };
        if !matches {
            return true;
        }
        self.queue.lock().push_back(notification.clone());
        self.notify.notify_one();
        true
    }

    async fn next(&self) -> anyhow::Result<HarnessNotification> {
        loop {
            let notified = self.notify.notified();
            if let Some(notification) = self.queue.lock().pop_front() {
                return Ok(notification);
            }
            if let Some(failure) = self.failure.lock().clone() {
                return Err(failure.to_error());
            }
            notified.await;
        }
    }
}

/// One client-side notification stream.
pub struct NotificationSubscription {
    id: u64,
    state: Arc<SubscriptionState>,
    client: Weak<HarnessClient>,
}

impl NotificationSubscription {
    /// Waits for the next matching notification.
    ///
    /// # Errors
    ///
    /// Returns the terminal subscription or runtime failure after queued items drain.
    pub async fn next(&self) -> anyhow::Result<HarnessNotification> {
        self.state.next().await
    }

    /// Drains one already-delivered notification without waiting.
    #[must_use]
    pub fn try_next(&self) -> Option<HarnessNotification> {
        self.state.queue.lock().pop_front()
    }

    /// Detaches, drops queued notifications, and fails pending waits.
    pub fn close(&self) {
        if self.state.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Some(client) = self.client.upgrade() {
            client.state.lock().subscriptions.remove(&self.id);
        }
        self.state.fail(
            SubscriptionFailure::Transport(TransportClosedError {
                message: "notification subscription closed".to_owned(),
            }),
            true,
        );
    }
}

impl Drop for NotificationSubscription {
    fn drop(&mut self) {
        self.close();
    }
}

struct ClientState {
    child: Option<SubprocessHandleRef>,
    transport: Option<Arc<JsonRpcLineTransport>>,
    stderr: Option<SubprocessOutputReaderHandle>,
    exit: Option<Result<seekdeep_subprocess::SubprocessOutcome, String>>,
    closing: bool,
    closed: bool,
    subscriptions: HashMap<u64, Arc<SubscriptionState>>,
    parents: HashMap<String, String>,
}

impl ClientState {
    fn new() -> Self {
        Self {
            child: None,
            transport: None,
            stderr: None,
            exit: None,
            closing: false,
            closed: false,
            subscriptions: HashMap::new(),
            parents: HashMap::new(),
        }
    }
}

#[derive(Default)]
struct CloseState {
    started: AtomicBool,
    result: Mutex<Option<Result<(), String>>>,
    notify: Notify,
}

/// Low-level SDK runtime process client.
pub struct HarnessClient {
    /// Immutable launch options.
    pub options: HarnessClientOptions,
    state: Mutex<ClientState>,
    start_lock: tokio::sync::Mutex<()>,
    serial: AtomicU64,
    close: Arc<CloseState>,
}

impl HarnessClient {
    /// Constructs a lazy client.
    #[must_use]
    pub fn new(options: HarnessClientOptions) -> Arc<Self> {
        Arc::new(Self {
            options,
            state: Mutex::new(ClientState::new()),
            start_lock: tokio::sync::Mutex::new(()),
            serial: AtomicU64::new(0),
            close: Arc::new(CloseState::default()),
        })
    }

    /// Spawns the runtime and starts frame processing. Idempotent while live.
    ///
    /// # Errors
    ///
    /// Returns reuse-after-close, spawn, or missing-pipe failures.
    pub async fn start(self: &Arc<Self>) -> anyhow::Result<()> {
        let _guard = self.start_lock.lock().await;
        {
            let state = self.state.lock();
            anyhow::ensure!(
                !state.closed && !state.closing,
                TransportClosedError {
                    message: "SeekDeep Harness runtime client is closed".to_owned()
                }
            );
            if state.child.is_some() {
                return Ok(());
            }
        }
        let child: SubprocessHandleRef = spawn_subprocess(
            SubprocessSpawnSpec {
                argv: std::iter::once(self.options.command.clone())
                    .chain(self.options.args.clone())
                    .collect(),
                cwd: self
                    .options
                    .cwd
                    .clone()
                    .map_or(std::env::current_dir()?, Into::into),
                stdio: SubprocessStdio {
                    stdin: SubprocessStdinMode::Pipe,
                    stdout: SubprocessOutputMode::Pipe,
                    stderr: SubprocessOutputMode::Collect(SubprocessCollect {
                        max_bytes: 2_000_000.0,
                        spill: None,
                    }),
                },
                grace_ms: self.options.dispose_grace_ms,
                signal: None,
                env: Some(self.complete_environment()),
            },
            None,
        )
        .map_err(|error| {
            anyhow::Error::new(TransportClosedError {
                message: format!("SeekDeep Harness runtime failed to start\nspawn error: {error}"),
            })
        })?;
        let transport = prepare_transport_or_cleanup(&child).await?;
        let weak = Arc::downgrade(self);
        transport.on_notification(Arc::new(move |method, params| {
            if let Some(client) = weak.upgrade() {
                client.dispatch(&HarnessNotification { method, params });
            }
        }));
        transport.start();
        let stderr = child.collected().stderr;
        {
            let mut state = self.state.lock();
            state.child = Some(Arc::clone(&child));
            state.transport = Some(Arc::clone(&transport));
            state.stderr = stderr;
        }
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let outcome = child.done().await.map_err(|error| error.to_string());
            if let Some(client) = weak.upgrade() {
                client.process_ended(outcome);
            }
        });
        Ok(())
    }

    /// Performs the process-wide handshake.
    ///
    /// # Errors
    ///
    /// Returns request or response-shape failures.
    pub async fn initialize(
        self: &Arc<Self>,
        params: InitializeParams,
    ) -> anyhow::Result<InitializeResult> {
        let value = self
            .request("initialize", value_object(&params)?, None)
            .await?;
        serde_json::from_value(value.clone()).map_err(|_| {
            anyhow::Error::new(SdkProtocolError {
                message: format!("initialize returned no server identity: {value}"),
            })
        })
    }

    /// Queues one prompt and returns its durable message identity.
    ///
    /// # Errors
    ///
    /// Returns request or response-shape failures.
    pub async fn prompt(
        self: &Arc<Self>,
        session_id: seekdeep_core::session::SessionId,
        content_blocks: Vec<ContentBlock>,
    ) -> anyhow::Result<seekdeep_llm::MessageId> {
        let params = SessionPromptParams {
            session_id,
            content_blocks,
        };
        let value = self
            .request("session/prompt", value_object(&params)?, None)
            .await?;
        serde_json::from_value::<SessionPromptResult>(value.clone())
            .map(|result| result.message_id)
            .map_err(|_| {
                anyhow::Error::new(SdkProtocolError {
                    message: format!("session/prompt returned no message id: {value}"),
                })
            })
    }

    /// Sends one request with an optional per-call timeout.
    ///
    /// # Errors
    ///
    /// Returns response, timeout, or process-contextual transport failures.
    pub async fn request(
        self: &Arc<Self>,
        method: &str,
        params: Map<String, Value>,
        timeout_ms: Option<f64>,
    ) -> anyhow::Result<Value> {
        self.start().await?;
        let (exited, transport) = {
            let state = self.state.lock();
            (state.exit.is_some(), state.transport.clone())
        };
        if exited {
            return Err(anyhow::Error::new(
                self.closed_error("SeekDeep Harness runtime is not running"),
            ));
        }
        let transport = transport.ok_or_else(|| {
            anyhow::Error::new(TransportClosedError {
                message: "SeekDeep Harness runtime is not running".to_owned(),
            })
        })?;
        let timeout = timeout_ms.or(self.options.request_timeout_ms);
        let (signal, timer) = timeout.map_or((None, None), |duration| {
            let signal = AbortSignal::default();
            let timer_signal = signal.clone();
            let method = method.to_owned();
            let timer = tokio::spawn(async move {
                tokio::time::sleep(javascript_timer_duration(duration)).await;
                timer_signal.abort_with_reason(Value::String(format!(
                    "{method} timed out after {duration}ms waiting for the SeekDeep Harness runtime"
                )));
            });
            (Some(signal), Some(timer))
        });
        let response = transport.request(method, params, signal.clone()).await;
        if let Some(timer) = timer {
            timer.abort();
        }
        match response {
            Ok(value) => Ok(value),
            Err(error) if error.downcast_ref::<JsonRpcResponseError>().is_some() => Err(error),
            Err(_error) if signal.as_ref().is_some_and(AbortSignal::is_aborted) => {
                Err(anyhow::Error::new(RequestTimeoutError {
                    message: signal
                        .and_then(|signal| signal.reason())
                        .and_then(|reason| reason.as_str().map(str::to_owned))
                        .unwrap_or_else(|| "SDK request timed out".to_owned()),
                }))
            }
            Err(error) => {
                self.settle_process_edge().await;
                Err(anyhow::Error::new(self.closed_error(&error.to_string())))
            }
        }
    }

    /// Subscribes to runtime notifications.
    #[must_use]
    pub fn subscribe(
        self: &Arc<Self>,
        filter: Option<NotificationFilter>,
    ) -> NotificationSubscription {
        let id = self.serial.fetch_add(1, Ordering::AcqRel);
        let state = SubscriptionState::new(filter);
        let failed = {
            let mut client = self.state.lock();
            if client.closed || client.closing || client.exit.is_some() {
                true
            } else {
                client.subscriptions.insert(id, Arc::clone(&state));
                false
            }
        };
        if failed {
            state.fail(
                SubscriptionFailure::Transport(
                    self.closed_error("SeekDeep Harness runtime closed"),
                ),
                false,
            );
        }
        NotificationSubscription {
            id,
            state,
            client: Arc::downgrade(self),
        }
    }

    /// Subscribes to a session and descendants discovered from lineage edges.
    #[must_use]
    pub fn subscribe_session_tree(
        self: &Arc<Self>,
        session_id: &seekdeep_core::session::SessionId,
    ) -> NotificationSubscription {
        let weak = Arc::downgrade(self);
        let root = session_id.to_string();
        self.subscribe(Some(Arc::new(move |notification| {
            let Some(client) = weak.upgrade() else {
                return false;
            };
            let params = &notification.params;
            if matches!(
                notification.method.as_str(),
                "subagent.started" | "subagent.finished"
            ) {
                return params
                    .get("parentSessionId")
                    .and_then(Value::as_str)
                    .is_some_and(|id| client.is_descendant(id, &root))
                    || params.get("childSessionId").and_then(Value::as_str) == Some(&root);
            }
            params
                .get("sessionId")
                .and_then(Value::as_str)
                .is_some_and(|id| client.is_descendant(id, &root))
        })))
    }

    /// Requests shutdown and reaps the complete process tree. Idempotent.
    ///
    /// # Errors
    ///
    /// Returns the authoritative process-tree disposal failure.
    pub async fn close(self: &Arc<Self>) -> anyhow::Result<()> {
        if !self.close.started.swap(true, Ordering::AcqRel) {
            self.state.lock().closing = true;
            let result = self.perform_close().await;
            *self.close.result.lock() = Some(result.map_err(|error| error.to_string()));
            self.close.notify.notify_waiters();
        }
        loop {
            let notified = self.close.notify.notified();
            if let Some(result) = self.close.result.lock().clone() {
                return result.map_err(anyhow::Error::msg);
            }
            notified.await;
        }
    }

    fn complete_environment(&self) -> SubprocessEnvironment {
        let Some(configured) = self.options.env.as_ref() else {
            return std::env::vars()
                .map(|(key, value)| (key, Some(value)))
                .collect();
        };
        let mut environment = scrubbed_parent_env()
            .into_keys()
            .map(|key| (key.to_string_lossy().into_owned(), None))
            .collect::<SubprocessEnvironment>();
        environment.extend(
            configured
                .iter()
                .map(|(key, value)| (key.clone(), Some(value.clone()))),
        );
        environment
    }

    fn dispatch(&self, notification: &HarnessNotification) {
        if notification.method == "subagent.started"
            && let (Some(parent), Some(child)) = (
                notification
                    .params
                    .get("parentSessionId")
                    .and_then(Value::as_str),
                notification
                    .params
                    .get("childSessionId")
                    .and_then(Value::as_str),
            )
            && !parent.is_empty()
            && !child.is_empty()
            && parent != child
        {
            self.state
                .lock()
                .parents
                .insert(child.to_owned(), parent.to_owned());
        }
        let subscriptions = self
            .state
            .lock()
            .subscriptions
            .iter()
            .map(|(id, state)| (*id, Arc::clone(state)))
            .collect::<Vec<_>>();
        let mut failed = Vec::new();
        for (id, subscription) in subscriptions {
            if !subscription.push(notification) {
                failed.push(id);
            }
        }
        if !failed.is_empty() {
            let mut state = self.state.lock();
            for id in failed {
                state.subscriptions.remove(&id);
            }
        }
    }

    fn is_descendant(&self, session: &str, root: &str) -> bool {
        let parents = self.state.lock().parents.clone();
        let mut current = session;
        let mut visited = std::collections::HashSet::new();
        while visited.insert(current.to_owned()) {
            if current == root {
                return true;
            }
            let Some(parent) = parents.get(current) else {
                return false;
            };
            current = parent;
        }
        false
    }

    fn process_ended(&self, outcome: Result<seekdeep_subprocess::SubprocessOutcome, String>) {
        let subscriptions = {
            let mut state = self.state.lock();
            state.exit = Some(outcome);
            if let Some(transport) = &state.transport {
                transport.close();
            }
            state.subscriptions.values().cloned().collect::<Vec<_>>()
        };
        let error = self.closed_error("SeekDeep Harness runtime exited");
        for subscription in subscriptions {
            subscription.fail(SubscriptionFailure::Transport(error.clone()), false);
        }
    }

    async fn settle_process_edge(&self) {
        for _ in 0..10 {
            if self.state.lock().exit.is_some() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    async fn perform_close(self: &Arc<Self>) -> anyhow::Result<()> {
        let (child, transport) = {
            let state = self.state.lock();
            (state.child.clone(), state.transport.clone())
        };
        let Some(child) = child else {
            let subscriptions = {
                let mut state = self.state.lock();
                state.closed = true;
                state.closing = false;
                state.subscriptions.values().cloned().collect::<Vec<_>>()
            };
            for subscription in subscriptions {
                subscription.fail(
                    SubscriptionFailure::Transport(TransportClosedError {
                        message: "SeekDeep Harness runtime closed".to_owned(),
                    }),
                    false,
                );
            }
            return Ok(());
        };
        let _ = self.request_internal_shutdown(transport.as_ref()).await;
        if let Some(transport) = &transport {
            let _ = transport.shutdown_output().await;
        }
        let exited = tokio::select! {
            result = child.wait_for_exit(None) => result?,
            () = tokio::time::sleep(javascript_timer_duration(self.options.dispose_eof_grace_ms)) => false,
        };
        if !exited {
            child.terminate();
            anyhow::ensure!(
                child.wait_for_exit(None).await?,
                "runtime process did not exit after termination"
            );
        }
        let _ = child.done().await;
        if let Some(transport) = transport {
            transport.close();
        }
        let subscriptions = {
            let mut state = self.state.lock();
            state.closed = true;
            state.closing = false;
            state.subscriptions.values().cloned().collect::<Vec<_>>()
        };
        let error = self.closed_error("SeekDeep Harness runtime closed");
        for subscription in subscriptions {
            subscription.fail(SubscriptionFailure::Transport(error.clone()), false);
        }
        Ok(())
    }

    async fn request_internal_shutdown(
        &self,
        transport: Option<&Arc<JsonRpcLineTransport>>,
    ) -> anyhow::Result<()> {
        let Some(transport) = transport else {
            return Ok(());
        };
        let signal = AbortSignal::default();
        let timer = signal.clone();
        let timeout = self.options.shutdown_timeout_ms;
        let task = tokio::spawn(async move {
            tokio::time::sleep(javascript_timer_duration(timeout)).await;
            timer.abort_with_reason(Value::String("shutdown timed out".to_owned()));
        });
        let result = transport
            .request("shutdown", Map::new(), Some(signal))
            .await;
        task.abort();
        let _ = result?;
        Ok(())
    }

    fn closed_error(&self, reason: &str) -> TransportClosedError {
        let state = self.state.lock();
        let mut lines = vec![reason.to_owned()];
        if let Some(exit) = &state.exit {
            match exit {
                Ok(outcome) => lines.push(format!(
                    "exit code: {}",
                    outcome
                        .exit_code
                        .map_or("null".to_owned(), |code| code.to_string())
                )),
                Err(error) => lines.push(format!("spawn error: {error}")),
            }
        }
        if let Some(stderr) = &state.stderr {
            let text = stderr.read_from(0).text;
            let tail = text
                .lines()
                .rev()
                .take(STDERR_TAIL_LIMIT)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            if !tail.is_empty() {
                lines.push(format!("stderr tail:\n{tail}"));
            }
        }
        TransportClosedError {
            message: lines.join("\n"),
        }
    }
}

fn value_object(value: &impl serde::Serialize) -> anyhow::Result<Map<String, Value>> {
    let Value::Object(value) = serde_json::to_value(value)? else {
        anyhow::bail!("SDK request params are not an object");
    };
    Ok(value)
}

async fn prepare_transport_or_cleanup(
    child: &SubprocessHandleRef,
) -> anyhow::Result<Arc<JsonRpcLineTransport>> {
    match prepare_transport(child).await {
        Ok(transport) => Ok(transport),
        Err(error) => {
            child.terminate();
            let _ = child.wait_for_exit(None).await;
            let process = child.done().await;
            let mut message = format!("SeekDeep Harness runtime failed to start\n{error}");
            if let Err(error) = process {
                write!(&mut message, "\nspawn error: {error}")?;
            }
            Err(anyhow::Error::new(TransportClosedError { message }))
        }
    }
}

async fn prepare_transport(
    child: &SubprocessHandleRef,
) -> anyhow::Result<Arc<JsonRpcLineTransport>> {
    let stdin = child
        .stdin()
        .ok_or_else(|| anyhow::anyhow!("SDK runtime dropped piped stdin"))?;
    let stdout = child
        .stdout()
        .ok_or_else(|| anyhow::anyhow!("SDK runtime dropped piped stdout"))?;
    let output = stdin
        .take_writer()
        .await
        .ok_or_else(|| anyhow::anyhow!("SDK runtime stdin was already claimed"))?;
    let input = stdout.take_reader().await;
    Ok(JsonRpcLineTransport::from_boxed(input, output))
}

fn javascript_timer_duration(milliseconds: f64) -> std::time::Duration {
    const MAX_TIMER_DELAY_MS: f64 = 2_147_483_647.0;
    let normalized =
        if milliseconds.is_finite() && (1.0..=MAX_TIMER_DELAY_MS).contains(&milliseconds) {
            milliseconds
        } else {
            1.0
        };
    std::time::Duration::from_secs_f64(normalized / 1_000.0)
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_owned()
    } else {
        "notification filter failed".to_owned()
    }
}
