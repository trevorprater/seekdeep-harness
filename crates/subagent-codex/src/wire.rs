//! Minimal Codex app-server product protocol over the shared line transport.

use std::sync::Arc;

use futures::FutureExt as _;
use parking_lot::Mutex;
use seekdeep_llm::{AbortSignal, ContentBlock, JsonString};
use seekdeep_lossless_json::JsonValue;
use seekdeep_sdk_protocol::{BoxedJsonRpcInput, BoxedJsonRpcOutput, JsonRpcLineTransport};
use seekdeep_subagent::{SubagentResult, SubagentStopReason};
use serde_json::{Value, json};
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
    turn_completed: Option<oneshot::Sender<JsonValue>>,
    early_turn_notifications: Vec<(String, JsonValue)>,
    last_final_answer: Option<JsonString>,
    last_unphased_answer: Option<JsonString>,
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
        transport.on_request_json(Arc::new(move |method, params| {
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
        transport.on_notification_json(Arc::new(move |method, params| {
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
            .guard_request(
                self.transport.request_json(
                    "initialize",
                    json!({
                        "clientInfo": {
                            "name": "seekdeep-harness",
                            "title": "SeekDeep Harness",
                            "version": "0.0.1",
                        },
                        "capabilities": {
                            "experimentalApi": false,
                            "requestAttestation": false,
                        },
                    })
                    .into(),
                    Some(signal),
                ),
            )
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
            .guard_request(self.transport.request_json(
                "thread/start",
                json!({"cwd":cwd, "ephemeral":true}).into(),
                Some(signal),
            ))
            .await?;
        let response = object_labeled(response, "thread/start response")?;
        let thread = object_labeled(
            response
                .get_value("thread")
                .cloned()
                .unwrap_or_else(|| Value::Null.into()),
            "thread/start thread",
        )?;
        let id = nonempty_string(thread.get_value("id"), "thread/start thread id")?;
        anyhow::ensure!(
            thread.get_value("ephemeral").and_then(JsonValue::as_bool) == Some(true),
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
        texts: &[JsonString],
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
            .map(|text| {
                JsonValue::object([
                    ("type", json!("text").into()),
                    ("text", text.clone().into()),
                    ("text_elements", json!([]).into()),
                ])
            })
            .collect::<Vec<_>>();
        let response = self
            .guard_request(self.transport.request_json(
                "turn/start",
                JsonValue::object([
                    ("threadId", json!(thread_id).into()),
                    ("input", JsonValue::array(&input)),
                ]),
                Some(signal.clone()),
            ))
            .await?;
        let response = object_labeled(response, "turn/start response")?;
        let turn = object_labeled(
            response
                .get_value("turn")
                .cloned()
                .unwrap_or_else(|| Value::Null.into()),
            "turn/start turn",
        )?;
        let id = nonempty_string(turn.get_value("id"), "turn/start turn id")?;
        self.commit_turn_id(id)?;

        let completed = self.guard_completion(receiver, signal).await?;
        let terminal = object_labeled(
            completed
                .get_value("turn")
                .cloned()
                .unwrap_or_else(|| Value::Null.into()),
            "turn/completed turn",
        )?;
        let status = terminal
            .get_value("status")
            .cloned()
            .unwrap_or_else(|| Value::Null.into());
        if context_window_exceeded(&terminal) {
            return Ok(SubagentResult {
                output: self.collect_output(),
                structured: None,
                stop_reason: SubagentStopReason::MaxTokens,
            });
        }
        anyhow::ensure!(
            status == "completed",
            "subagent-codex: Codex turn ended with status {}{}",
            js_string_json(&status),
            if status == "failed" {
                format!(
                    ": {}",
                    terminal
                        .get_value("error")
                        .map_or_else(|| "undefined".to_owned(), JsonValue::stringify)
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
                .request_json(
                    "turn/interrupt",
                    json!({"threadId":request.0, "turnId":request.1}).into(),
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

    async fn guard_request<F, T>(&self, pending: F) -> anyhow::Result<T>
    where
        F: std::future::Future<Output = anyhow::Result<T>>,
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
        receiver: oneshot::Receiver<JsonValue>,
        signal: AbortSignal,
    ) -> anyhow::Result<JsonValue> {
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

    fn handle_server_request(&self, method: &str, params: &JsonValue) -> anyhow::Result<JsonValue> {
        match method {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                self.validate_run_ids(params, false)?;
                Ok(json!({"decision": unattended_decision(params)?}).into())
            }
            "item/permissions/requestApproval" => {
                self.validate_run_ids(params, false)?;
                Ok(json!({"permissions":{}, "scope":"turn"}).into())
            }
            "item/tool/requestUserInput" => {
                self.validate_run_ids(params, false)?;
                Ok(json!({"answers":{}}).into())
            }
            "mcpServer/elicitation/request" => {
                self.validate_run_ids(params, true)?;
                Ok(json!({"action":"decline", "content":null, "_meta":null}).into())
            }
            _ => anyhow::bail!(
                "subagent-codex: unsupported app-server request {}",
                serde_json::to_string(method)?
            ),
        }
    }

    fn validate_run_ids(&self, params: &JsonValue, nullable_turn: bool) -> anyhow::Result<()> {
        let mut state = self.state.lock();
        let thread_id = state.thread_id.as_deref().unwrap_or_default();
        anyhow::ensure!(
            params.get_value("threadId").and_then(JsonValue::as_str) == Some(thread_id),
            "subagent-codex: app-server request referenced another thread"
        );
        if nullable_turn && params.get_value("turnId").is_some_and(JsonValue::is_null) {
            return Ok(());
        }
        let id = nonempty_string(params.get_value("turnId"), "server request turn id")?;
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

    fn handle_notification(&self, method: &str, params: JsonValue) -> anyhow::Result<()> {
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
    params: JsonValue,
) -> anyhow::Result<()> {
    if method == "turn/started" {
        let thread_id = nonempty_string(params.get_value("threadId"), "turn/started thread id")?;
        if state.thread_id.as_deref() != Some(thread_id.as_str()) {
            return Ok(());
        }
        let turn = object_labeled(
            params
                .get_value("turn")
                .cloned()
                .unwrap_or_else(|| Value::Null.into()),
            "turn/started turn",
        )?;
        if state.turn_completed.is_some() && state.turn_id.is_none() {
            observe_pending_turn_id(
                state,
                nonempty_string(turn.get_value("id"), "turn/started turn id")?,
            )?;
        }
        return Ok(());
    }
    if method == "item/completed" {
        let thread_id = nonempty_string(params.get_value("threadId"), "item/completed thread id")?;
        if state.thread_id.as_deref() != Some(thread_id.as_str()) {
            return Ok(());
        }
        let id = nonempty_string(params.get_value("turnId"), "item/completed turn id")?;
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
            params
                .get_value("item")
                .cloned()
                .unwrap_or_else(|| Value::Null.into()),
            "item/completed item",
        )?;
        return record_agent_message(state, &item);
    }
    if method != "turn/completed" {
        return Ok(());
    }
    let thread_id = nonempty_string(params.get_value("threadId"), "turn/completed thread id")?;
    if state.thread_id.as_deref() != Some(thread_id.as_str()) {
        return Ok(());
    }
    let turn = object_labeled(
        params
            .get_value("turn")
            .cloned()
            .unwrap_or_else(|| Value::Null.into()),
        "turn/completed turn",
    )?;
    let id = nonempty_string(turn.get_value("id"), "turn/completed turn id")?;
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
        .get_value("status")
        .and_then(JsonValue::as_str)
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

fn record_agent_message(state: &mut WireState, item: &JsonValue) -> anyhow::Result<()> {
    if item.get_value("type").and_then(JsonValue::as_str) != Some("agentMessage") {
        return Ok(());
    }
    let text = item
        .get("text")
        .and_then(|text| text.deserialize::<JsonString>().ok())
        .ok_or_else(|| {
            anyhow::anyhow!("subagent-codex: app-server returned an invalid agent message")
        })?;
    match item.get_value("phase") {
        Some(phase) if phase == "final_answer" => {
            state.last_final_answer = Some(text);
        }
        Some(phase) if phase.is_null() => state.last_unphased_answer = Some(text),
        Some(phase) if phase == "commentary" => {}
        phase => anyhow::bail!(
            "subagent-codex: app-server returned an unknown agent message phase {}",
            phase.map_or_else(|| "undefined".to_owned(), JsonValue::stringify)
        ),
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

fn unattended_decision(params: &JsonValue) -> anyhow::Result<&'static str> {
    match params.get_value("availableDecisions") {
        None => Ok("decline"),
        Some(value) if value.is_null() => Ok("decline"),
        Some(value) if value.is_array() => {
            let decisions = value.as_array().expect("array was checked");
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

fn object_labeled(value: JsonValue, label: &str) -> anyhow::Result<JsonValue> {
    if !value.is_object() {
        anyhow::bail!("subagent-codex: app-server returned invalid {label}");
    }
    Ok(value)
}

fn nonempty_string(value: Option<&JsonValue>, label: &str) -> anyhow::Result<String> {
    value
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("subagent-codex: app-server returned invalid {label}"))
}

fn context_window_exceeded(turn: &JsonValue) -> bool {
    turn.get_value("status").and_then(JsonValue::as_str) == Some("failed")
        && turn
            .get_value("error")
            .and_then(|error| error.get_value("codexErrorInfo"))
            .and_then(JsonValue::as_str)
            == Some("contextWindowExceeded")
}

fn js_string_json(value: &JsonValue) -> String {
    if let Some(value) = value.as_str() {
        value.to_owned()
    } else if value.is_array() {
        String::new()
    } else if value.is_object() {
        "[object Object]".to_owned()
    } else {
        value.as_raw().to_owned()
    }
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
