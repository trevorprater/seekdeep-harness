//! Restartable synchronous process client with single-consumption notification routing.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_identity::{MessageId, RpcId, SessionId};
use seekdeep_llm::{ModelId, ProviderId};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    Error, ErrorKind, HarnessConfig, Host, IdSource, IncomingRequest, Notification, RequestId,
    Result, RuntimeProcess, process::read_lines, queue::Queue, runtime::resolve_path,
    values::python_str,
};

/// Opaque source-compatible notification registration identity.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SubscriptionId(String);

impl SubscriptionId {
    /// Returns the registration's UUID spelling.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Fallible user filter; failure is delivered only to its own subscription.
pub type NotificationFilter = Arc<dyn Fn(&Notification) -> Result<bool> + Send + Sync>;
/// Synchronous user observer invoked on the requesting or draining thread.
pub type NotificationObserver = Arc<dyn Fn(&Notification) -> Result<()> + Send + Sync>;

/// Optional notification delivery and timeout override for one request.
#[derive(Default)]
pub struct RequestOptions {
    /// None falls back to the client's current request timeout.
    pub timeout_seconds: Option<f64>,
    /// Drain matching notifications before waiting and before returning a response.
    pub on_notification: Option<NotificationObserver>,
    /// Applied only when an observer requires a temporary subscription.
    pub notification_filter: Option<NotificationFilter>,
    /// An existing subscription remains owned by its caller.
    pub notification_subscription: Option<Arc<NotificationSubscription>>,
}

#[derive(Clone)]
enum Predicate {
    All,
    Session(SessionId),
    User(NotificationFilter),
}

#[derive(Clone)]
struct Subscriber {
    queue: Arc<Queue<Result<Notification>>>,
    predicate: Predicate,
}

#[derive(Default)]
struct State {
    process: Option<Arc<RuntimeProcess>>,
    responses: IndexMap<RpcId, Arc<Queue<Result<Value>>>>,
    subscribers: IndexMap<SubscriptionId, Subscriber>,
    parents: BTreeMap<SessionId, SessionId>,
}

/// A lazy, restartable owner of one runtime subprocess.
pub struct Client {
    config: Mutex<HarnessConfig>,
    host: Host,
    ids: Arc<dyn IdSource>,
    lifecycle: Mutex<()>,
    state: Mutex<State>,
    notifications: Queue<Result<Notification>>,
    requests: Queue<Result<IncomingRequest>>,
    stderr: Mutex<VecDeque<String>>,
}

impl Client {
    /// Creates a client without resolving packages or starting a process.
    pub fn new(config: HarnessConfig, host: Host, ids: Arc<dyn IdSource>) -> Arc<Self> {
        Arc::new(Self {
            config: Mutex::new(config),
            host,
            ids,
            lifecycle: Mutex::new(()),
            state: Mutex::new(State::default()),
            notifications: Queue::default(),
            requests: Queue::default(),
            stderr: Mutex::new(VecDeque::new()),
        })
    }

    /// Current configuration snapshot.
    pub fn config(&self) -> HarnessConfig {
        self.config.lock().clone()
    }

    /// Replaces the configuration without restarting a running process.
    pub fn set_config(&self, config: HarnessConfig) {
        *self.config.lock() = config;
    }

    /// Returns the current process, including a process that has already exited.
    pub fn process(&self) -> Option<Arc<RuntimeProcess>> {
        self.state.lock().process.clone()
    }

    /// Starts the process once; a completed close permits a later restart.
    ///
    /// # Errors
    /// Propagates late package lookup, path resolution, spawn, or reader-start failures.
    pub fn start(self: &Arc<Self>) -> Result<()> {
        let _lifecycle = self.lifecycle.lock();
        {
            let mut state = self.state.lock();
            if state.process.is_some() {
                return Ok(());
            }
            state.parents.clear();
        }
        let config = self.config();
        let argv = match config
            .launch_args_override
            .as_ref()
            .filter(|argv| !argv.is_empty())
        {
            Some(argv) => argv.clone(),
            None => self.default_launch_args(&config)?,
        };
        let mut environment = (self.host.environment)();
        if let Some(overrides) = &config.env {
            environment.extend(overrides.clone());
        }
        if config.launch_args_override.is_none()
            && config.runtime_bin.is_none()
            && config.bridge_bin.is_none()
            && environment
                .get("SEEKDEEP_CORDIS_CONFIG")
                .is_none_or(String::is_empty)
        {
            environment.insert(
                "SEEKDEEP_CORDIS_CONFIG".to_owned(),
                (self.host.bundled_config)()?,
            );
        }
        let cwd = config
            .cwd
            .as_deref()
            .map(|path| resolve_path(std::path::Path::new(path), &(self.host.cwd)()?))
            .transpose()?;
        let (process, stdout, stderr) = RuntimeProcess::spawn(argv, cwd.as_deref(), &environment)?;
        self.state.lock().process = Some(Arc::clone(&process));
        if let Err(error) = self.start_readers(&process, stdout, stderr) {
            process.close_stdin();
            let _ = process.kill();
            let _ = process.wait(None);
            process.cancelled.store(true, Ordering::Release);
            process.finish_readers();
            self.state.lock().process = None;
            self.fail_waiters(error.clone());
            return Err(error);
        }
        Ok(())
    }

    fn default_launch_args(&self, config: &HarnessConfig) -> Result<Vec<String>> {
        if let Some(runtime) = &config.runtime_bin {
            return Ok(vec![runtime.clone()]);
        }
        if let Some(bridge) = &config.bridge_bin {
            return Ok(vec![bridge.clone()]);
        }
        (self.host.bundled_launch)().map_err(|error| {
            if error.import_error {
                Error::new(ErrorKind::FileNotFound,
                    "Unable to locate the bundled SeekDeep Harness SDK runtime. Install seekdeep-harness-runtime-bin or set HarnessConfig.runtime_bin."
                ).caused_by(error)
            } else {
                error
            }
        })
    }

    fn start_readers(
        self: &Arc<Self>,
        process: &Arc<RuntimeProcess>,
        stdout: std::process::ChildStdout,
        stderr: std::process::ChildStderr,
    ) -> Result<()> {
        let owner = Arc::downgrade(self);
        let runtime = Arc::clone(process);
        let stdout_reader = std::thread::Builder::new()
            .name("seekdeep-runtime-reader".to_owned())
            .spawn(move || {
                let result = read_lines(stdout, &runtime.cancelled, |line| {
                    if line.trim_matches(crate::values::is_whitespace).is_empty() {
                        return;
                    }
                    if let (Ok(message), Some(owner)) =
                        (serde_json::from_str(&line), owner.upgrade())
                    {
                        owner.handle_message(&message);
                    }
                });
                if let Some(owner) = owner.upgrade() {
                    if let Err(error) = result {
                        owner.fail_waiters(error);
                    }
                    owner
                        .fail_waiters(owner.closed_error("SeekDeep Harness runtime stdout closed"));
                }
                runtime.stdout_done.finish();
            })
            .map_err(|error| Error::io(&error, None))?;
        process.readers.lock().push(stdout_reader);
        let owner = Arc::downgrade(self);
        let runtime = Arc::clone(process);
        let stderr_reader = std::thread::Builder::new()
            .name("seekdeep-runtime-stderr".to_owned())
            .spawn(move || {
                if let Err(error) = read_lines(stderr, &runtime.cancelled, |line| {
                    if let Some(owner) = owner.upgrade() {
                        owner.append_stderr(
                            line.trim_end_matches(crate::values::is_whitespace)
                                .to_owned(),
                        );
                    }
                }) {
                    eprintln!("seekdeep-runtime-stderr: {error}");
                }
                runtime.stderr_done.finish();
            })
            .map_err(|error| Error::io(&error, None))?;
        process.readers.lock().push(stderr_reader);
        Ok(())
    }

    /// Performs shutdown, closes stdin, terminates, and reaps the owned process.
    ///
    /// # Errors
    /// Native termination/wait failures propagate; shutdown protocol failures are retained in stderr.
    pub fn close(self: &Arc<Self>) -> Result<()> {
        let _lifecycle = self.lifecycle.lock();
        let Some(process) = self.process() else {
            return Ok(());
        };
        let timeout = self.config().shutdown_timeout_seconds;
        if let Err(error) = self.request_object(
            "shutdown",
            None,
            RequestOptions {
                timeout_seconds: timeout,
                ..RequestOptions::default()
            },
        ) {
            self.append_stderr(format!("shutdown request failed: {error}"));
        }
        process.close_stdin();
        if process.poll()?.is_none() {
            process.terminate()?;
        }
        match process.wait(timeout) {
            Ok(_) => {}
            Err(error) if error.kind == ErrorKind::SubprocessTimeout => {
                process.kill()?;
                process.wait(None)?;
            }
            Err(error) => return Err(error),
        }
        self.state.lock().process = None;
        self.fail_waiters(self.closed_error("SeekDeep Harness runtime closed"));
        process.finish_readers();
        Ok(())
    }

    /// Initializes through the caller's response validator and closes on any failure.
    ///
    /// # Errors
    /// Propagates request or validation errors after reaping the process.
    pub fn initialize<T>(
        self: &Arc<Self>,
        cwd: &str,
        provider: &ProviderId,
        model: &ModelId,
        max_tokens: Option<Value>,
        validate: impl FnOnce(Map<String, Value>) -> Result<T>,
    ) -> Result<T> {
        let result = (|| {
            let cwd = resolve_path(std::path::Path::new(cwd), &(self.host.cwd)()?)?;
            let mut payload = json!({"cwd":cwd,"provider":provider,"model":model})
                .as_object()
                .cloned()
                .unwrap_or_default();
            if let Some(tokens) = max_tokens {
                payload.insert("maxTokens".to_owned(), tokens);
            }
            validate(self.request_object(
                "initialize",
                Some(Value::Object(payload)),
                RequestOptions::default(),
            )?)
        })();
        if result.is_err() {
            self.close()?;
        }
        result
    }

    /// Submits a prompt using a session-tree filter for a temporary observer subscription.
    ///
    /// # Errors
    /// Propagates request errors or a response without a string messageId.
    pub fn session_prompt(
        self: &Arc<Self>,
        session_id: &SessionId,
        content_blocks: Value,
        options: RequestOptions,
    ) -> Result<MessageId> {
        self.session_prompt_with(session_id, content_blocks, options, |response| {
            response
                .get("messageId")
                .and_then(Value::as_str)
                .map(MessageId::new)
                .ok_or_else(|| {
                    Error::new(
                        ErrorKind::Value,
                        "session/prompt response requires a string messageId",
                    )
                })
        })
    }

    /// Submits a prompt through a binding-provided response model validator.
    ///
    /// # Errors
    /// Propagates request and validator failures without closing an otherwise live process.
    pub fn session_prompt_with(
        self: &Arc<Self>,
        session_id: &SessionId,
        content_blocks: Value,
        mut options: RequestOptions,
        validate: impl FnOnce(Map<String, Value>) -> Result<MessageId>,
    ) -> Result<MessageId> {
        let owner = Arc::downgrade(self);
        let session = session_id.clone();
        options.notification_filter = Some(Arc::new(move |notification| {
            Ok(owner
                .upgrade()
                .is_some_and(|owner| owner.belongs_to_session(notification, &session)))
        }));
        let mut payload = json!({"sessionId":session_id});
        payload["contentBlocks"] = content_blocks;
        let response = self.request_object("session/prompt", Some(payload), options)?;
        validate(response)
    }

    /// Requests an object result; foreign bindings may apply their caller-provided model afterwards.
    ///
    /// # Errors
    /// Propagates request errors and rejects non-object responses.
    pub fn request_object(
        self: &Arc<Self>,
        method: &str,
        params: Option<Value>,
        options: RequestOptions,
    ) -> Result<Map<String, Value>> {
        self.request_raw(method, params, options)?
            .as_object()
            .cloned()
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Type,
                    format!("{method} response must be a JSON object"),
                )
            })
    }

    /// Waits for a single response, draining observer notifications on the caller's thread.
    ///
    /// # Errors
    /// Propagates write, callback, transport, JSON-RPC, and timeout failures.
    pub fn request_raw(
        self: &Arc<Self>,
        method: &str,
        params: Option<Value>,
        options: RequestOptions,
    ) -> Result<Value> {
        let RequestOptions {
            timeout_seconds,
            on_notification,
            notification_filter,
            notification_subscription,
        } = options;
        let id = RpcId::new(self.ids.next_uuid().to_string());
        let waiter = Arc::new(Queue::default());
        self.state
            .lock()
            .responses
            .insert(id.clone(), Arc::clone(&waiter));
        let temporary = if on_notification.is_some() && notification_subscription.is_none() {
            Some(self.subscribe_notifications(notification_filter))
        } else {
            None
        };
        let subscription = notification_subscription.or_else(|| temporary.clone());
        let _pending = PendingRequest {
            owner: Arc::downgrade(self),
            id: id.clone(),
            temporary,
        };
        let mut message = json!({"jsonrpc":"2.0","id":id,"method":method});
        if let Some(params) = params {
            message["params"] = params;
        }
        self.write_message(&message)?;
        let timeout = timeout_seconds.or(self.config().request_timeout_seconds);
        let deadline = timeout.map(|timeout| (self.host.monotonic)() + timeout);
        loop {
            if let (Some(observer), Some(subscription)) = (&on_notification, &subscription) {
                subscription.drain(observer)?;
            }
            let mut wait = on_notification.as_ref().map(|_| 0.05_f64);
            if let Some(deadline) = deadline {
                let remaining = deadline - (self.host.monotonic)();
                if remaining <= 0.0 {
                    let diagnostics = self.runtime_diagnostics();
                    let suffix = if diagnostics.is_empty() {
                        String::new()
                    } else {
                        format!("\n{diagnostics}")
                    };
                    return Err(Error::new(
                        ErrorKind::Timeout,
                        format!("{method} timed out waiting for SeekDeep Harness runtime{suffix}"),
                    ));
                }
                wait = Some(wait.map_or(remaining, |interval| interval.min(remaining)));
            }
            let item = if let Some(item) = waiter.try_pop() {
                Some(item)
            } else {
                let wait = wait.map(wait_duration).transpose()?;
                waiter.pop(wait)
            };
            if let Some(item) = item {
                if let (Some(observer), Some(subscription)) = (&on_notification, &subscription) {
                    subscription.drain(observer)?;
                }
                return item;
            }
        }
    }

    /// Sends a notification without allocating a response waiter.
    ///
    /// # Errors
    /// Propagates unavailable-process or write failures.
    pub fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        let mut message = json!({"jsonrpc":"2.0","method":method});
        if let Some(params) = params {
            message["params"] = params;
        }
        self.write_message(&message)
    }

    /// Sends a result to an incoming peer request.
    ///
    /// # Errors
    /// Propagates unavailable-process or write failures.
    pub fn respond(&self, id: &RequestId, result: Value) -> Result<()> {
        let mut message = json!({"jsonrpc":"2.0","id":id});
        message["result"] = result;
        self.write_message(&message)
    }

    /// Sends a JSON-RPC error, omitting absent data.
    ///
    /// # Errors
    /// Propagates unavailable-process or write failures.
    pub fn respond_error(
        &self,
        id: &RequestId,
        code: Value,
        message: &str,
        data: Option<Value>,
    ) -> Result<()> {
        let mut error = Value::Object(Map::new());
        error["code"] = code;
        error["message"] = json!(message);
        if let Some(data) = data {
            error["data"] = data;
        }
        self.write_message(&json!({"jsonrpc":"2.0","id":id,"error":error}))
    }

    /// Writes one compact JSON line under the process's write lock.
    ///
    /// # Errors
    /// Returns a typed transport error, retaining the write failure as its cause.
    pub fn write_message(&self, message: &Value) -> Result<()> {
        let process = self.process().ok_or_else(|| {
            Error::new(
                ErrorKind::TransportClosed,
                "SeekDeep Harness runtime is not running",
            )
        })?;
        let mut payload = serde_json::to_vec(message)
            .map_err(|error| Error::new(ErrorKind::Type, error.to_string()))?;
        payload.push(b'\n');
        process.write(&payload).map_err(|cause| {
            self.closed_error("Failed to write to SeekDeep Harness runtime")
                .caused_by(cause)
        })
    }

    /// Registers a notification queue without consuming unmatched global notifications.
    pub fn subscribe_notifications(
        self: &Arc<Self>,
        predicate: Option<NotificationFilter>,
    ) -> Arc<NotificationSubscription> {
        self.subscribe(predicate.map_or(Predicate::All, Predicate::User))
    }

    /// Registers a root session and descendants learned from subagent.started edges.
    pub fn subscribe_session(
        self: &Arc<Self>,
        session: SessionId,
    ) -> Arc<NotificationSubscription> {
        self.subscribe(Predicate::Session(session))
    }

    fn subscribe(self: &Arc<Self>, predicate: Predicate) -> Arc<NotificationSubscription> {
        let id = SubscriptionId(self.ids.next_uuid().to_string());
        let queue = Arc::new(Queue::default());
        self.state.lock().subscribers.insert(
            id.clone(),
            Subscriber {
                queue: Arc::clone(&queue),
                predicate,
            },
        );
        Arc::new(NotificationSubscription {
            client: Arc::clone(self),
            id,
            queue,
            closed: AtomicBool::new(false),
        })
    }

    /// Waits for one unmatched notification or queued transport failure.
    ///
    /// # Errors
    /// Propagates the failure queued for this read.
    pub fn next_notification(&self) -> Result<Notification> {
        self.notifications
            .pop(None)
            .unwrap_or_else(|| Err(Error::new(ErrorKind::Empty, "")))
    }

    /// Reads an unmatched notification without blocking.
    ///
    /// # Errors
    /// Returns Empty when no item exists, or the queued failure.
    pub fn try_notification(&self) -> Result<Notification> {
        self.notifications
            .try_pop()
            .unwrap_or_else(|| Err(Error::new(ErrorKind::Empty, "")))
    }

    /// Number of currently unmatched notification or error items.
    pub fn notification_count(&self) -> usize {
        self.notifications.len()
    }

    /// Waits for an incoming peer request or queued transport failure.
    ///
    /// # Errors
    /// Propagates the failure queued for this read.
    pub fn next_request(&self) -> Result<IncomingRequest> {
        self.requests
            .pop(None)
            .unwrap_or_else(|| Err(Error::new(ErrorKind::Empty, "")))
    }

    /// Routes one decoded message; unknown methods and absent JSON-RPC version fields remain accepted.
    pub fn handle_message(&self, message: &Value) {
        let Some(message) = message.as_object() else {
            return;
        };
        let id = message.get("id").and_then(RequestId::from_value);
        let method = message.get("method").and_then(Value::as_str);
        let payload = || {
            message
                .get("params")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default()
        };
        if let (Some(id), Some(method)) = (&id, method) {
            self.requests.push(Ok(IncomingRequest {
                id: id.clone(),
                method: method.to_owned(),
                payload: payload(),
            }));
            return;
        }
        if let Some(id) = id {
            let waiter = self
                .state
                .lock()
                .responses
                .shift_remove(&RpcId::new(id.correlation_key()));
            let Some(waiter) = waiter else {
                return;
            };
            if let Some(error) = message.get("error").and_then(Value::as_object) {
                let mut failure = Error::new(
                    ErrorKind::JsonRpc,
                    error
                        .get("message")
                        .map_or_else(|| "JSON-RPC error".to_owned(), python_str),
                );
                failure.code = error
                    .get("code")
                    .filter(|value| value.is_i64() || value.is_u64() || value.is_boolean())
                    .cloned();
                failure.data = error.get("data").cloned();
                waiter.push(Err(failure));
            } else {
                waiter.push(Ok(message.get("result").cloned().unwrap_or(Value::Null)));
            }
            return;
        }
        let Some(method) = method else {
            return;
        };
        self.route_notification(Notification {
            method: method.to_owned(),
            payload: payload(),
        });
    }

    fn route_notification(&self, notification: Notification) {
        let subscribers = {
            let mut state = self.state.lock();
            if notification.method == "subagent.started" {
                let parent = notification
                    .payload
                    .get("parentSessionId")
                    .and_then(Value::as_str);
                let child = notification
                    .payload
                    .get("childSessionId")
                    .and_then(Value::as_str);
                if let (Some(parent), Some(child)) = (parent, child)
                    && !parent.is_empty()
                    && !child.is_empty()
                    && parent != child
                {
                    state
                        .parents
                        .insert(SessionId::new(child), SessionId::new(parent));
                }
            }
            state
                .subscribers
                .iter()
                .map(|(id, subscriber)| (id.clone(), subscriber.clone()))
                .collect::<Vec<_>>()
        };
        let mut delivered = false;
        for (id, subscriber) in subscribers {
            let matches = match &subscriber.predicate {
                Predicate::All => Ok(true),
                Predicate::Session(session) => Ok(self.belongs_to_session(&notification, session)),
                Predicate::User(predicate) => predicate(&notification),
            };
            match matches {
                Ok(true) => {
                    subscriber.queue.push(Ok(notification.clone()));
                    delivered = true;
                }
                Ok(false) => {}
                Err(error) => {
                    let mut state = self.state.lock();
                    if state
                        .subscribers
                        .get(&id)
                        .is_some_and(|entry| Arc::ptr_eq(&entry.queue, &subscriber.queue))
                    {
                        state.subscribers.shift_remove(&id);
                    }
                    drop(state);
                    subscriber.queue.push(Err(error));
                }
            }
        }
        if !delivered {
            self.notifications.push(Ok(notification));
        }
    }

    fn belongs_to_session(&self, notification: &Notification, session: &SessionId) -> bool {
        let state = self.state.lock();
        if matches!(
            notification.method.as_str(),
            "subagent.started" | "subagent.finished"
        ) {
            if notification
                .payload
                .get("parentSessionId")
                .and_then(Value::as_str)
                .is_some_and(|parent| descendant(&state.parents, parent, session))
            {
                return true;
            }
            return notification
                .payload
                .get("childSessionId")
                .and_then(Value::as_str)
                == Some(session.as_str());
        }
        notification
            .payload
            .get("sessionId")
            .and_then(Value::as_str)
            .is_some_and(|related| descendant(&state.parents, related, session))
    }

    /// Fails pending requests and subscriptions, then enqueues the same failure globally.
    pub fn fail_waiters(&self, error: Error) {
        let (waiters, subscribers) = {
            let mut state = self.state.lock();
            (
                std::mem::take(&mut state.responses),
                std::mem::take(&mut state.subscribers),
            )
        };
        for (_, waiter) in waiters {
            waiter.push(Err(error.clone()));
        }
        for (_, subscriber) in subscribers {
            subscriber.queue.push(Err(error.clone()));
        }
        self.notifications.push(Err(error.clone()));
        self.requests.push(Err(error));
    }

    fn append_stderr(&self, line: String) {
        let mut stderr = self.stderr.lock();
        if stderr.len() == 400 {
            stderr.pop_front();
        }
        stderr.push_back(line);
    }

    /// Captures an available exit code and at most 400 stderr lines.
    pub fn runtime_diagnostics(&self) -> String {
        let process = self.process();
        if let Some(process) = &process {
            process.collect_final_stderr();
        }
        let mut parts = Vec::new();
        if let Some(code) = process.and_then(|process| process.poll().ok().flatten()) {
            parts.push(format!("exit code: {code}"));
        }
        let stderr = self.stderr.lock();
        if !stderr.is_empty() {
            parts.push(format!(
                "stderr tail:\n{}",
                stderr.iter().cloned().collect::<Vec<_>>().join("\n")
            ));
        }
        parts.join("\n")
    }

    fn closed_error(&self, reason: &str) -> Error {
        let diagnostics = self.runtime_diagnostics();
        Error::new(
            ErrorKind::TransportClosed,
            if diagnostics.is_empty() {
                reason.to_owned()
            } else {
                format!("{reason}\n{diagnostics}")
            },
        )
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if let Some(process) = self.state.get_mut().process.take() {
            process.close_stdin();
            let _ = process.kill();
            let _ = process.wait(Some(2.0));
            process.cancelled.store(true, Ordering::Release);
            process.finish_readers();
        }
    }
}

fn descendant(parents: &BTreeMap<SessionId, SessionId>, session: &str, root: &SessionId) -> bool {
    let mut current = SessionId::new(session);
    let mut visited = BTreeSet::new();
    while visited.insert(current.clone()) {
        if current == *root {
            return true;
        }
        let Some(parent) = parents.get(&current) else {
            return false;
        };
        current = parent.clone();
    }
    false
}

fn wait_duration(seconds: f64) -> Result<Duration> {
    Duration::try_from_secs_f64(seconds).map_err(|_| {
        if seconds.is_nan() {
            Error::new(ErrorKind::Value, "Invalid value NaN (not a number)")
        } else {
            Error::new(
                ErrorKind::Overflow,
                "timestamp out of range for platform time_t",
            )
        }
    })
}

struct PendingRequest {
    owner: Weak<Client>,
    id: RpcId,
    temporary: Option<Arc<NotificationSubscription>>,
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            owner.state.lock().responses.shift_remove(&self.id);
        }
        if let Some(subscription) = &self.temporary {
            subscription.close();
        }
    }
}

/// Explicitly disposable notification queue; queued data remains readable after close.
pub struct NotificationSubscription {
    client: Arc<Client>,
    id: SubscriptionId,
    queue: Arc<Queue<Result<Notification>>>,
    closed: AtomicBool,
}

impl NotificationSubscription {
    /// Stable source registration identity.
    pub fn id(&self) -> &SubscriptionId {
        &self.id
    }

    /// Stops future routing without discarding already queued notifications.
    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.client.state.lock().subscribers.shift_remove(&self.id);
        }
    }

    /// Whether this wrapper's close operation has been called.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Waits for one notification, including after explicit close.
    ///
    /// # Errors
    /// Propagates a queued filter or transport failure.
    pub fn next(&self) -> Result<Notification> {
        self.queue
            .pop(None)
            .unwrap_or_else(|| Err(Error::new(ErrorKind::Empty, "")))
    }

    /// Reads one notification without waiting.
    ///
    /// # Errors
    /// Returns Empty or a queued filter/transport failure.
    pub fn try_next(&self) -> Result<Notification> {
        self.queue
            .try_pop()
            .unwrap_or_else(|| Err(Error::new(ErrorKind::Empty, "")))
    }

    /// Number of queued notifications and failures.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Invokes the observer in queue order until empty or the first error.
    ///
    /// # Errors
    /// Propagates the exact queued or observer failure after consuming its triggering item.
    pub fn drain(&self, observer: &NotificationObserver) -> Result<()> {
        while let Some(item) = self.queue.try_pop() {
            observer(&item?)?;
        }
        Ok(())
    }
}
