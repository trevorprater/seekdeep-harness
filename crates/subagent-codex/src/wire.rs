//! Minimal Codex app-server product protocol over the shared line transport.

use std::sync::Arc;

use futures::FutureExt as _;
use parking_lot::Mutex;
use seekdeep_llm::{AbortSignal, ContentBlock};
use seekdeep_sdk_protocol::{BoxedJsonRpcInput, BoxedJsonRpcOutput, JsonRpcLineTransport};
use seekdeep_subagent::{SubagentResult, SubagentStopReason};
use serde_json::{Map, Value, json};
use tokio::sync::{Notify, oneshot};

#[derive(Default)]
struct FatalState {
    message: Mutex<Option<String>>,
    notify: Notify,
}

impl FatalState {
    fn fail(&self, error: &anyhow::Error) {
        let mut message = self.message.lock();
        if message.is_some() {
            return;
        }
        *message = Some(error.to_string());
        self.notify.notify_waiters();
    }

    async fn wait(&self) -> anyhow::Error {
        loop {
            let notified = self.notify.notified();
            if let Some(message) = self.message.lock().clone() {
                return anyhow::anyhow!(message);
            }
            notified.await;
        }
    }
}

struct WireState {
    thread_id: Option<String>,
    turn_id: Option<String>,
    pending_turn_id: Option<String>,
    turn_completed: Option<oneshot::Sender<Map<String, Value>>>,
    early_turn_notifications: Vec<(String, Map<String, Value>)>,
    last_final_answer: Option<String>,
    last_unphased_answer: Option<String>,
    closed: bool,
}

impl WireState {
    fn new() -> Self {
        Self {
            thread_id: None,
            turn_id: None,
            pending_turn_id: None,
            turn_completed: None,
            early_turn_notifications: Vec::new(),
            last_final_answer: None,
            last_unphased_answer: None,
            closed: false,
        }
    }
}

/// One app-server connection and its single ephemeral thread and turn.
pub struct CodexAppServerWire {
    transport: Arc<JsonRpcLineTransport>,
    fatal: Arc<FatalState>,
    state: Mutex<WireState>,
}

impl std::fmt::Debug for CodexAppServerWire {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodexAppServerWire")
            .field("transport", &self.transport)
            .finish_non_exhaustive()
    }
}

impl CodexAppServerWire {
    /// Constructs the private product wire over one process's pipe streams.
    #[must_use]
    pub fn new(input: BoxedJsonRpcInput, output: BoxedJsonRpcOutput) -> Arc<Self> {
        let transport = JsonRpcLineTransport::from_boxed(input, output);
        let wire = Arc::new(Self {
            transport: Arc::clone(&transport),
            fatal: Arc::new(FatalState::default()),
            state: Mutex::new(WireState::new()),
        });
        let weak = Arc::downgrade(&wire);
        transport.on_request(Arc::new(move |method, params| {
            let Some(wire) = weak.upgrade() else {
                return async { anyhow::bail!("subagent-codex: wire was dropped") }.boxed();
            };
            let result = wire.handle_server_request(&method, &params);
            if let Err(error) = &result {
                wire.fail(error);
            }
            async move { result }.boxed()
        }));
        let weak = Arc::downgrade(&wire);
        transport.on_notification(Arc::new(move |method, params| {
            let Some(wire) = weak.upgrade() else {
                return;
            };
            if let Err(error) = wire.handle_notification(&method, params) {
                wire.fail(&error);
            }
        }));
        let weak = Arc::downgrade(&wire);
        transport.on_input_failure(Arc::new(move |error| {
            if let Some(wire) = weak.upgrade() {
                wire.fail(&error);
            }
        }));
        wire
    }

    /// Starts reading app-server frames. Idempotent.
    pub fn start(&self) {
        self.transport.start();
    }

    /// Performs the required initialize and initialized handshake.
    ///
    /// # Errors
    ///
    /// Returns cancellation, transport, fatal-protocol, or malformed-response failures.
    pub async fn initialize(&self, signal: AbortSignal) -> anyhow::Result<()> {
        let response = self
            .guard_request(self.transport.request(
                "initialize",
                object(json!({
                    "clientInfo": {
                        "name": "seekdeep-harness",
                        "title": "SeekDeep Harness",
                        "version": "0.0.1",
                    },
                    "capabilities": {
                        "experimentalApi": false,
                        "requestAttestation": false,
                    },
                }))?,
                Some(signal),
            ))
            .await?;
        object_labeled(response, "initialize response")?;
        self.transport.notify("initialized", None).await?;
        self.transport.flush().await
    }

    /// Creates the run's private ephemeral thread and retains its identity.
    ///
    /// # Errors
    ///
    /// Returns cancellation, transport, fatal-protocol, or malformed-response failures.
    pub async fn start_thread(&self, cwd: &str, signal: AbortSignal) -> anyhow::Result<()> {
        let response = self
            .guard_request(self.transport.request(
                "thread/start",
                object(json!({"cwd":cwd, "ephemeral":true}))?,
                Some(signal),
            ))
            .await?;
        let response = object_labeled(response, "thread/start response")?;
        let thread = object_labeled(
            response.get("thread").cloned().unwrap_or(Value::Null),
            "thread/start thread",
        )?;
        let id = nonempty_string(thread.get("id"), "thread/start thread id")?;
        anyhow::ensure!(
            thread.get("ephemeral") == Some(&Value::Bool(true)),
            "subagent-codex: app-server did not create an ephemeral thread"
        );
        self.state.lock().thread_id = Some(id);
        Ok(())
    }

    /// Submits one text-only task and waits for its authoritative terminal notification.
    ///
    /// # Errors
    ///
    /// Returns cancellation, protocol, malformed-shape, terminal-status, or empty-output failures.
    pub async fn run_turn(
        &self,
        texts: &[String],
        signal: AbortSignal,
    ) -> anyhow::Result<SubagentResult> {
        let thread_id = self
            .state
            .lock()
            .thread_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("subagent-codex: thread/start has not completed"))?;
        let (sender, receiver) = oneshot::channel();
        {
            let mut state = self.state.lock();
            anyhow::ensure!(
                state.turn_completed.is_none(),
                "subagent-codex: the one-shot wire already owns a turn"
            );
            state.turn_completed = Some(sender);
        }
        let input = texts
            .iter()
            .map(|text| json!({"type":"text", "text":text, "text_elements":[]}))
            .collect::<Vec<_>>();
        let response = self
            .guard_request(self.transport.request(
                "turn/start",
                object(json!({"threadId":thread_id, "input":input}))?,
                Some(signal.clone()),
            ))
            .await?;
        let response = object_labeled(response, "turn/start response")?;
        let turn = object_labeled(
            response.get("turn").cloned().unwrap_or(Value::Null),
            "turn/start turn",
        )?;
        let id = nonempty_string(turn.get("id"), "turn/start turn id")?;
        self.commit_turn_id(id)?;

        let completed = self.guard_completion(receiver, signal).await?;
        let terminal = object_labeled(
            completed.get("turn").cloned().unwrap_or(Value::Null),
            "turn/completed turn",
        )?;
        let status = terminal.get("status").cloned().unwrap_or(Value::Null);
        if context_window_exceeded(&terminal) {
            return Ok(SubagentResult {
                output: self.collect_output(),
                structured: None,
                stop_reason: SubagentStopReason::MaxTokens,
            });
        }
        anyhow::ensure!(
            status == Value::String("completed".to_owned()),
            "subagent-codex: Codex turn ended with status {}{}",
            js_string(&status),
            if status == Value::String("failed".to_owned()) {
                format!(
                    ": {}",
                    serde_json::to_string(terminal.get("error").unwrap_or(&Value::Null))?
                )
            } else {
                String::new()
            }
        );
        let output = self.collect_output();
        anyhow::ensure!(
            !output.is_empty(),
            "subagent-codex: Codex completed without a final answer"
        );
        Ok(SubagentResult {
            output,
            structured: None,
            stop_reason: SubagentStopReason::Completed,
        })
    }

    /// Best-effort remote cancellation of the active open turn.
    pub fn interrupt(&self) {
        let request = {
            let state = self.state.lock();
            if state.closed || state.turn_completed.is_none() {
                return;
            }
            let (Some(thread_id), Some(turn_id)) = (state.thread_id.clone(), state.turn_id.clone())
            else {
                return;
            };
            (thread_id, turn_id)
        };
        let transport = Arc::clone(&self.transport);
        tokio::spawn(async move {
            let _ = transport
                .request(
                    "turn/interrupt",
                    object(json!({"threadId":request.0, "turnId":request.1})).unwrap_or_default(),
                    None,
                )
                .await;
        });
    }

    /// Returns the latest selected final or unphased answer, preserving bytes.
    #[must_use]
    pub fn collect_output(&self) -> Vec<ContentBlock> {
        let state = self.state.lock();
        let selected = state
            .last_final_answer
            .as_ref()
            .or(state.last_unphased_answer.as_ref());
        selected
            .filter(|text| !text.trim().is_empty())
            .map_or_else(Vec::new, |text| {
                vec![ContentBlock::Text { text: text.clone() }]
            })
    }

    /// Detaches transport listeners and rejects outstanding requests. Idempotent.
    pub fn close(&self) {
        let mut state = self.state.lock();
        if state.closed {
            return;
        }
        state.closed = true;
        drop(state);
        self.transport.close();
    }

    /// Delivers EOF on app-server stdin.
    ///
    /// # Errors
    ///
    /// Returns the child-pipe shutdown failure.
    pub async fn close_input(&self) -> anyhow::Result<()> {
        self.transport.shutdown_output().await
    }

    async fn guard_request<F>(&self, pending: F) -> anyhow::Result<Value>
    where
        F: std::future::Future<Output = anyhow::Result<Value>>,
    {
        tokio::pin!(pending);
        let fatal = self.fatal.wait();
        tokio::pin!(fatal);
        tokio::select! {
            biased;
            error = &mut fatal => Err(error),
            result = &mut pending => result,
        }
    }

    async fn guard_completion(
        &self,
        receiver: oneshot::Receiver<Map<String, Value>>,
        signal: AbortSignal,
    ) -> anyhow::Result<Map<String, Value>> {
        let fatal = self.fatal.wait();
        tokio::pin!(fatal);
        tokio::select! {
            biased;
            error = &mut fatal => Err(error),
            () = signal.cancelled() => Err(abort_error(&signal)),
            result = receiver => result.map_err(|_| anyhow::anyhow!("subagent-codex: turn completion channel closed")),
        }
    }

    fn fail(&self, error: &anyhow::Error) {
        self.fatal.fail(error);
    }

    fn handle_server_request(
        &self,
        method: &str,
        params: &Map<String, Value>,
    ) -> anyhow::Result<Value> {
        match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                self.validate_run_ids(params, false)?;
                Ok(json!({"decision": unattended_decision(params)?}))
            }
            "item/permissions/requestApproval" => {
                self.validate_run_ids(params, false)?;
                Ok(json!({"permissions":{}, "scope":"turn"}))
            }
            "item/tool/requestUserInput" => {
                self.validate_run_ids(params, false)?;
                Ok(json!({"answers":{}}))
            }
            "mcpServer/elicitation/request" => {
                self.validate_run_ids(params, true)?;
                Ok(json!({"action":"decline", "content":null, "_meta":null}))
            }
            _ => anyhow::bail!(
                "subagent-codex: unsupported app-server request {}",
                serde_json::to_string(method)?
            ),
        }
    }

    fn validate_run_ids(
        &self,
        params: &Map<String, Value>,
        nullable_turn: bool,
    ) -> anyhow::Result<()> {
        let mut state = self.state.lock();
        let thread_id = state.thread_id.as_deref().unwrap_or_default();
        anyhow::ensure!(
            params.get("threadId").and_then(Value::as_str) == Some(thread_id),
            "subagent-codex: app-server request referenced another thread"
        );
        if nullable_turn && params.get("turnId") == Some(&Value::Null) {
            return Ok(());
        }
        let id = nonempty_string(params.get("turnId"), "server request turn id")?;
        if state.turn_id.is_none() {
            observe_pending_turn_id(&mut state, id)?;
        } else {
            anyhow::ensure!(
                state.turn_id.as_deref() == Some(id.as_str()),
                "subagent-codex: app-server request referenced another turn"
            );
        }
        Ok(())
    }

    fn handle_notification(&self, method: &str, params: Map<String, Value>) -> anyhow::Result<()> {
        let mut state = self.state.lock();
        handle_notification_inner(&mut state, method, params)
    }

    fn commit_turn_id(&self, id: String) -> anyhow::Result<()> {
        let mut state = self.state.lock();
        anyhow::ensure!(
            state
                .pending_turn_id
                .as_ref()
                .is_none_or(|pending| pending == &id),
            "subagent-codex: turn/start response did not match the active turn"
        );
        state.turn_id = Some(id);
        let notifications = std::mem::take(&mut state.early_turn_notifications);
        for (method, params) in notifications {
            handle_notification_inner(&mut state, &method, params)?;
        }
        Ok(())
    }
}

fn handle_notification_inner(
    state: &mut WireState,
    method: &str,
    params: Map<String, Value>,
) -> anyhow::Result<()> {
    if method == "turn/started" {
        let thread_id = nonempty_string(params.get("threadId"), "turn/started thread id")?;
        if state.thread_id.as_deref() != Some(thread_id.as_str()) {
            return Ok(());
        }
        let turn = object_labeled(
            params.get("turn").cloned().unwrap_or(Value::Null),
            "turn/started turn",
        )?;
        if state.turn_completed.is_some() && state.turn_id.is_none() {
            observe_pending_turn_id(
                state,
                nonempty_string(turn.get("id"), "turn/started turn id")?,
            )?;
        }
        return Ok(());
    }
    if method == "item/completed" {
        let thread_id = nonempty_string(params.get("threadId"), "item/completed thread id")?;
        if state.thread_id.as_deref() != Some(thread_id.as_str()) {
            return Ok(());
        }
        let id = nonempty_string(params.get("turnId"), "item/completed turn id")?;
        if state.turn_id.is_none() {
            if state.turn_completed.is_some() {
                observe_pending_turn_id(state, id)?;
                state
                    .early_turn_notifications
                    .push((method.to_owned(), params));
            }
            return Ok(());
        }
        if state.turn_id.as_deref() != Some(id.as_str()) {
            return Ok(());
        }
        let item = object_labeled(
            params.get("item").cloned().unwrap_or(Value::Null),
            "item/completed item",
        )?;
        if item.get("type") != Some(&Value::String("agentMessage".to_owned())) {
            return Ok(());
        }
        let text = item
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                anyhow::anyhow!("subagent-codex: app-server returned an invalid agent message")
            })?
            .to_owned();
        match item.get("phase") {
            Some(Value::String(phase)) if phase == "final_answer" => {
                state.last_final_answer = Some(text);
            }
            Some(Value::Null) => state.last_unphased_answer = Some(text),
            Some(Value::String(phase)) if phase == "commentary" => {}
            phase => anyhow::bail!(
                "subagent-codex: app-server returned an unknown agent message phase {}",
                phase.map_or_else(|| "undefined".to_owned(), js_string)
            ),
        }
        return Ok(());
    }
    if method != "turn/completed" {
        return Ok(());
    }
    let thread_id = nonempty_string(params.get("threadId"), "turn/completed thread id")?;
    if state.thread_id.as_deref() != Some(thread_id.as_str()) {
        return Ok(());
    }
    let turn = object_labeled(
        params.get("turn").cloned().unwrap_or(Value::Null),
        "turn/completed turn",
    )?;
    let id = nonempty_string(turn.get("id"), "turn/completed turn id")?;
    if state.turn_completed.is_none() {
        return Ok(());
    }
    if state.turn_id.is_none() {
        observe_pending_turn_id(state, id)?;
        state
            .early_turn_notifications
            .push((method.to_owned(), params));
        return Ok(());
    }
    if state.turn_id.as_deref() != Some(id.as_str()) {
        return Ok(());
    }
    let status = turn
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    anyhow::ensure!(
        matches!(status, "completed" | "interrupted" | "failed"),
        "subagent-codex: app-server returned invalid terminal turn status {status}"
    );
    if let Some(sender) = state.turn_completed.take() {
        let _ = sender.send(params);
    }
    Ok(())
}

fn observe_pending_turn_id(state: &mut WireState, id: String) -> anyhow::Result<()> {
    anyhow::ensure!(
        state.turn_completed.is_some(),
        "subagent-codex: app-server referenced a turn before turn/start"
    );
    anyhow::ensure!(
        state
            .pending_turn_id
            .as_ref()
            .is_none_or(|pending| pending == &id),
        "subagent-codex: app-server referenced conflicting turns"
    );
    state.pending_turn_id = Some(id);
    Ok(())
}

fn unattended_decision(params: &Map<String, Value>) -> anyhow::Result<&'static str> {
    match params.get("availableDecisions") {
        None | Some(Value::Null) => Ok("decline"),
        Some(Value::Array(decisions)) => {
            if decisions.iter().any(|value| value == "cancel") {
                Ok("cancel")
            } else if decisions.iter().any(|value| value == "decline") {
                Ok("decline")
            } else {
                anyhow::bail!("subagent-codex: app-server offered no unattended approval decision")
            }
        }
        Some(_) => {
            anyhow::bail!("subagent-codex: app-server offered no unattended approval decision")
        }
    }
}

fn object(value: Value) -> anyhow::Result<Map<String, Value>> {
    let Value::Object(value) = value else {
        anyhow::bail!("subagent-codex: internal protocol params are not an object");
    };
    Ok(value)
}

fn object_labeled(value: Value, label: &str) -> anyhow::Result<Map<String, Value>> {
    let Value::Object(value) = value else {
        anyhow::bail!("subagent-codex: app-server returned invalid {label}");
    };
    Ok(value)
}

fn nonempty_string(value: Option<&Value>, label: &str) -> anyhow::Result<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("subagent-codex: app-server returned invalid {label}"))
}

fn context_window_exceeded(turn: &Map<String, Value>) -> bool {
    turn.get("status") == Some(&Value::String("failed".to_owned()))
        && turn
            .get("error")
            .and_then(Value::as_object)
            .and_then(|error| error.get("codexErrorInfo"))
            == Some(&Value::String("contextWindowExceeded".to_owned()))
}

fn abort_error(signal: &AbortSignal) -> anyhow::Error {
    if let Some(error) = signal.error_reason() {
        return anyhow::anyhow!(error.to_string());
    }
    anyhow::anyhow!(
        "subagent-codex: app-server request aborted: {}",
        signal
            .reason()
            .as_ref()
            .map_or("null".to_owned(), js_string)
    )
}

fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(_) => String::new(),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}
