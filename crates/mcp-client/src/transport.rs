//! Native stdio and HTTP MCP client generations.

use std::{
    collections::BTreeMap,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::StreamExt as _;
use parking_lot::Mutex;
use seekdeep_llm::AbortSignal;
use seekdeep_sdk_protocol::JsonRpcLineTransport;
use serde_json::{Map, Value, json};
use tokio::{process::Child, sync::OnceCell};

use crate::{
    Config,
    protocol::{
        McpClient, McpClientFactory, McpClientSignals, McpToolPage, configured_headers,
        normalize_tool_schemas,
    },
};

const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[
    "2025-11-25",
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
    "2024-10-07",
];
const CANCEL_NOTIFICATION: &str = "notifications/cancelled";
const TOOL_LIST_CHANGED_NOTIFICATION: &str = "notifications/tools/list_changed";
const CHILD_EXIT_POLL: Duration = Duration::from_millis(10);
const CHILD_COOPERATIVE_CLOSE: Duration = Duration::from_secs(2);
const SDK_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CANCEL_NOTIFICATION_TIMEOUT: Duration = Duration::from_secs(1);

/// Production factory for native stdio and Rust HTTP generations.
#[derive(Debug, Default)]
pub struct NativeMcpClientFactory;

#[async_trait]
impl McpClientFactory for NativeMcpClientFactory {
    async fn create(&self, config: &Config) -> anyhow::Result<Arc<dyn McpClient>> {
        match config {
            Config::Stdio {
                command,
                args,
                env,
                cwd,
                ..
            } => Ok(Arc::new(StdioMcpClient::new(
                command.clone(),
                args.clone(),
                env.clone(),
                cwd.clone(),
            ))),
            Config::StreamableHttp { url, headers, .. } => Ok(Arc::new(HttpMcpClient::new(
                url,
                headers,
                HttpMode::Streamable,
            )?)),
            Config::StatelessHttp {
                url,
                headers,
                protocol_version,
                ..
            } => Ok(Arc::new(HttpMcpClient::new(
                url,
                headers,
                HttpMode::Stateless {
                    protocol_version: protocol_version.clone(),
                },
            )?)),
        }
    }
}

struct StdioState {
    transport: Arc<JsonRpcLineTransport>,
    child: tokio::sync::Mutex<Option<Child>>,
    signals: Arc<McpClientSignals>,
    closing: AtomicBool,
}

impl StdioState {
    fn monitor(self: &Arc<Self>) {
        let state = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                let outcome = {
                    let mut child = state.child.lock().await;
                    match child.as_mut() {
                        Some(child) => child.try_wait(),
                        None => return,
                    }
                };
                match outcome {
                    Ok(Some(_)) | Err(_) => {
                        state.transport.close();
                        state.signals.close();
                        return;
                    }
                    Ok(None) => tokio::time::sleep(CHILD_EXIT_POLL).await,
                }
            }
        });
    }

    async fn close(&self) -> anyhow::Result<()> {
        if self.closing.swap(true, Ordering::AcqRel) {
            self.signals.closed_signal().cancelled().await;
            return Ok(());
        }
        let _ = self.transport.shutdown_output().await;
        let mut failure = None;
        {
            let mut child = self.child.lock().await;
            if let Some(child) = child.as_mut() {
                match tokio::time::timeout(CHILD_COOPERATIVE_CLOSE, child.wait()).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => failure = Some(anyhow::Error::new(error)),
                    Err(_) => {
                        if let Err(error) = child.start_kill() {
                            failure = Some(anyhow::Error::new(error));
                        } else if let Err(error) = child.wait().await {
                            failure = Some(anyhow::Error::new(error));
                        }
                    }
                }
            }
        }
        self.transport.close();
        self.signals.close();
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// One child-process MCP client generation.
struct StdioMcpClient {
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    cwd: String,
    state: OnceCell<Arc<StdioState>>,
    signals: Arc<McpClientSignals>,
}

impl std::fmt::Debug for StdioMcpClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StdioMcpClient")
            .field("command", &self.command)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

impl StdioMcpClient {
    fn new(command: String, args: Vec<String>, env: BTreeMap<String, String>, cwd: String) -> Self {
        Self {
            command,
            args,
            env,
            cwd,
            state: OnceCell::new(),
            signals: Arc::new(McpClientSignals::default()),
        }
    }

    async fn state(&self) -> anyhow::Result<&Arc<StdioState>> {
        self.state
            .get_or_try_init(|| async {
                let mut command = tokio::process::Command::new(&self.command);
                command
                    .args(&self.args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::inherit())
                    .kill_on_drop(true)
                    .env_clear();
                for (name, value) in seekdeep_subprocess::scrubbed_parent_env() {
                    command.env(name, value);
                }
                command.envs(&self.env);
                if !self.cwd.is_empty() {
                    command.current_dir(&self.cwd);
                }
                let mut child = command.spawn()?;
                let input = child
                    .stdout
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("mcp-client: child stdout pipe is missing"))?;
                let output = child
                    .stdin
                    .take()
                    .ok_or_else(|| anyhow::anyhow!("mcp-client: child stdin pipe is missing"))?;
                let transport = JsonRpcLineTransport::new(input, output);
                let signals = Arc::clone(&self.signals);
                transport.on_notification(Arc::new(move |method, _| {
                    if method == TOOL_LIST_CHANGED_NOTIFICATION {
                        signals.tools_changed();
                    }
                }));
                transport.start();
                let state = Arc::new(StdioState {
                    transport,
                    child: tokio::sync::Mutex::new(Some(child)),
                    signals: Arc::clone(&self.signals),
                    closing: AtomicBool::new(false),
                });
                state.monitor();
                Ok(state)
            })
            .await
    }

    async fn request(
        &self,
        method: &str,
        params: Map<String, Value>,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Value> {
        let transport = &self.state().await?.transport;
        if let Some(signal) = signal {
            transport
                .request_with_cancellation(method, params, signal, CANCEL_NOTIFICATION)
                .await
        } else {
            let timeout_signal = AbortSignal::default();
            let request = transport.request_with_cancellation(
                method,
                params,
                timeout_signal.clone(),
                CANCEL_NOTIFICATION,
            );
            tokio::pin!(request);
            tokio::select! {
                result = &mut request => result,
                () = tokio::time::sleep(SDK_REQUEST_TIMEOUT) => {
                    timeout_signal.abort_with_reason(Value::String("MCP request timed out".to_owned()));
                    let _ = request.await;
                    anyhow::bail!("MCP request timed out after 60000ms")
                }
            }
        }
    }
}

#[async_trait]
impl McpClient for StdioMcpClient {
    async fn connect(&self) -> anyhow::Result<()> {
        let result = self
            .request(
                "initialize",
                Map::from_iter([
                    (
                        "protocolVersion".to_owned(),
                        Value::String(LATEST_PROTOCOL_VERSION.to_owned()),
                    ),
                    ("capabilities".to_owned(), json!({})),
                    (
                        "clientInfo".to_owned(),
                        json!({"name":"seekdeep-mcp-client","version":"0.0.1"}),
                    ),
                ]),
                None,
            )
            .await?;
        validate_initialize(&result)?;
        self.state()
            .await?
            .transport
            .notify("notifications/initialized", None)
            .await?;
        Ok(())
    }

    async fn list_tools(&self, cursor: Option<&str>) -> anyhow::Result<McpToolPage> {
        let params = cursor.map_or_else(Map::new, |cursor| {
            Map::from_iter([("cursor".to_owned(), Value::String(cursor.to_owned()))])
        });
        Ok(serde_json::from_value(
            self.request("tools/list", params, None).await?,
        )?)
    }

    async fn call_tool(
        &self,
        raw_name: &str,
        arguments: Map<String, Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<Value> {
        self.request(
            "tools/call",
            Map::from_iter([
                ("name".to_owned(), Value::String(raw_name.to_owned())),
                ("arguments".to_owned(), Value::Object(arguments)),
            ]),
            Some(signal),
        )
        .await
    }

    async fn close(&self) -> anyhow::Result<()> {
        if let Some(state) = self.state.get() {
            state.close().await
        } else {
            self.signals.close();
            Ok(())
        }
    }

    fn closed_signal(&self) -> AbortSignal {
        self.signals.closed_signal()
    }

    fn list_change_generation(&self) -> u64 {
        self.signals.list_change_generation()
    }

    async fn wait_list_change(&self, after: u64) {
        self.signals.wait_list_change(after).await;
    }
}

#[derive(Clone, Debug)]
enum HttpMode {
    Streamable,
    Stateless { protocol_version: String },
}

struct HttpMcpClient {
    url: url::Url,
    headers: reqwest::header::HeaderMap,
    mode: HttpMode,
    http: reqwest::Client,
    session_id: Mutex<Option<String>>,
    negotiated_protocol: Mutex<Option<String>>,
    next_id: AtomicU64,
    closed: AtomicBool,
    signals: Arc<McpClientSignals>,
    sse_task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl std::fmt::Debug for HttpMcpClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpMcpClient")
            .field("url", &self.url)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl HttpMcpClient {
    fn new(url: &str, headers: &BTreeMap<String, String>, mode: HttpMode) -> anyhow::Result<Self> {
        Ok(Self {
            url: url::Url::parse(url)?,
            headers: configured_headers(headers)?,
            mode,
            http: reqwest::Client::builder().build()?,
            session_id: Mutex::new(None),
            negotiated_protocol: Mutex::new(None),
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            signals: Arc::new(McpClientSignals::default()),
            sse_task: tokio::sync::Mutex::new(None),
        })
    }

    fn assert_open(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.closed.load(Ordering::Acquire),
            "MCP HTTP transport is closed"
        );
        Ok(())
    }

    async fn request(
        &self,
        method: &str,
        params: Map<String, Value>,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Value> {
        self.assert_open()?;
        let id = self.next_id.fetch_add(1, Ordering::AcqRel);
        let mut params = params;
        if let HttpMode::Stateless { protocol_version } = &self.mode {
            params.insert(
                "_meta".to_owned(),
                json!({
                    "io.modelcontextprotocol/protocolVersion":protocol_version,
                    "io.modelcontextprotocol/clientCapabilities":{}
                }),
            );
        }
        let wire = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let send = self.send_wire(&wire, Some(id));
        let mut reply = match signal {
            Some(signal) => {
                tokio::select! {
                    result = send => result?,
                    () = signal.cancelled() => {
                        if matches!(self.mode, HttpMode::Streamable) {
                            let _ = tokio::time::timeout(
                                CANCEL_NOTIFICATION_TIMEOUT,
                                self.notification(
                                    CANCEL_NOTIFICATION,
                                    Map::from_iter([
                                        ("requestId".to_owned(), Value::from(id)),
                                        ("reason".to_owned(), Value::String("request cancelled".to_owned())),
                                    ]),
                                ),
                            ).await;
                        }
                        anyhow::bail!("MCP request aborted")
                    }
                }
            }
            None => {
                tokio::select! {
                    result = send => result?,
                    () = tokio::time::sleep(SDK_REQUEST_TIMEOUT) => {
                        if matches!(self.mode, HttpMode::Streamable) {
                            let _ = tokio::time::timeout(
                                CANCEL_NOTIFICATION_TIMEOUT,
                                self.notification(
                                    CANCEL_NOTIFICATION,
                                    Map::from_iter([
                                        ("requestId".to_owned(), Value::from(id)),
                                        ("reason".to_owned(), Value::String("request timed out".to_owned())),
                                    ]),
                                ),
                            ).await;
                        }
                        anyhow::bail!("MCP request timed out after 60000ms")
                    }
                }
            }
        };
        if matches!(self.mode, HttpMode::Stateless { .. }) {
            normalize_tool_schemas(&mut reply);
        }
        take_jsonrpc_result(reply)
    }

    async fn notification(&self, method: &str, params: Map<String, Value>) -> anyhow::Result<()> {
        if matches!(self.mode, HttpMode::Stateless { .. }) {
            return Ok(());
        }
        let wire = json!({"jsonrpc":"2.0","method":method,"params":params});
        tokio::time::timeout(SDK_REQUEST_TIMEOUT, self.send_wire(&wire, None))
            .await
            .map_err(|_| anyhow::anyhow!("MCP notification timed out after 60000ms"))??;
        Ok(())
    }

    async fn send_wire(&self, wire: &Value, id: Option<u64>) -> anyhow::Result<Value> {
        let mut request = self
            .http
            .post(self.url.clone())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(
                reqwest::header::ACCEPT,
                "application/json, text/event-stream",
            )
            .headers(self.headers.clone())
            .body(serde_json::to_vec(wire)?);
        if let Some(session_id) = self.session_id.lock().clone() {
            request = request.header("mcp-session-id", session_id);
        }
        if let Some(protocol) = self.negotiated_protocol.lock().clone() {
            request = request.header("mcp-protocol-version", protocol);
        }
        let response = request.send().await?;
        if let Some(session) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
        {
            *self.session_id.lock() = Some(session.to_owned());
        }
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let text = response.text().await?;
        if !status.is_success() {
            let prefix = if matches!(self.mode, HttpMode::Stateless { .. }) {
                "StatelessHttpTransport"
            } else {
                "StreamableHTTPClientTransport"
            };
            let detail = text.chars().take(300).collect::<String>();
            anyhow::bail!(
                "{prefix}: server returned {}{}",
                status.as_u16(),
                if detail.is_empty() {
                    String::new()
                } else {
                    format!(": {detail}")
                }
            );
        }
        if id.is_none() {
            return Ok(Value::Null);
        }
        if text.trim().is_empty() {
            return futures::future::pending::<anyhow::Result<Value>>().await;
        }
        match parse_http_reply(&text, &content_type, id.expect("request id"))? {
            Some(reply) => Ok(reply),
            None => futures::future::pending::<anyhow::Result<Value>>().await,
        }
    }

    async fn start_sse_notifications(&self) {
        let Some(session_id) = self.session_id.lock().clone() else {
            return;
        };
        let http = self.http.clone();
        let url = self.url.clone();
        let headers = self.headers.clone();
        let protocol = self.negotiated_protocol.lock().clone();
        let signals = Arc::clone(&self.signals);
        let task = tokio::spawn(async move {
            let closed = signals.closed_signal();
            loop {
                let mut request = http
                    .get(url.clone())
                    .headers(headers.clone())
                    .header(reqwest::header::ACCEPT, "text/event-stream")
                    .header("mcp-session-id", &session_id);
                if let Some(protocol) = &protocol {
                    request = request.header("mcp-protocol-version", protocol);
                }
                let response = tokio::select! {
                    () = closed.cancelled() => return,
                    response = request.send() => response,
                };
                let Ok(response) = response else {
                    if wait_sse_retry(&closed).await {
                        return;
                    }
                    continue;
                };
                if !response.status().is_success() {
                    if wait_sse_retry(&closed).await {
                        return;
                    }
                    continue;
                }
                let mut stream = response.bytes_stream();
                let mut buffer = Vec::new();
                loop {
                    let chunk = tokio::select! {
                        () = closed.cancelled() => return,
                        chunk = stream.next() => chunk,
                    };
                    let Some(Ok(chunk)) = chunk else {
                        break;
                    };
                    buffer.extend_from_slice(&chunk);
                    consume_sse_notifications(&mut buffer, &signals);
                }
                if wait_sse_retry(&closed).await {
                    return;
                }
            }
        });
        *self.sse_task.lock().await = Some(task);
    }
}

#[async_trait]
impl McpClient for HttpMcpClient {
    async fn connect(&self) -> anyhow::Result<()> {
        if let HttpMode::Stateless { .. } = self.mode {
            return Ok(());
        }
        let result = self
            .request(
                "initialize",
                Map::from_iter([
                    (
                        "protocolVersion".to_owned(),
                        Value::String(LATEST_PROTOCOL_VERSION.to_owned()),
                    ),
                    ("capabilities".to_owned(), json!({})),
                    (
                        "clientInfo".to_owned(),
                        json!({"name":"seekdeep-mcp-client","version":"0.0.1"}),
                    ),
                ]),
                None,
            )
            .await?;
        let list_changed =
            result.pointer("/capabilities/tools/listChanged") == Some(&Value::Bool(true));
        let version = validate_initialize(&result)?;
        *self.negotiated_protocol.lock() = Some(version);
        self.notification("notifications/initialized", Map::new())
            .await?;
        if list_changed {
            self.start_sse_notifications().await;
        }
        Ok(())
    }

    async fn list_tools(&self, cursor: Option<&str>) -> anyhow::Result<McpToolPage> {
        let params = cursor.map_or_else(Map::new, |cursor| {
            Map::from_iter([("cursor".to_owned(), Value::String(cursor.to_owned()))])
        });
        Ok(serde_json::from_value(
            self.request("tools/list", params, None).await?,
        )?)
    }

    async fn call_tool(
        &self,
        raw_name: &str,
        arguments: Map<String, Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<Value> {
        self.request(
            "tools/call",
            Map::from_iter([
                ("name".to_owned(), Value::String(raw_name.to_owned())),
                ("arguments".to_owned(), Value::Object(arguments)),
            ]),
            Some(signal),
        )
        .await
    }

    async fn close(&self) -> anyhow::Result<()> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.signals.close();
        if let Some(task) = self.sse_task.lock().await.take() {
            let _ = task.await;
        }
        if matches!(self.mode, HttpMode::Streamable) && self.session_id.lock().is_some() {
            let mut request = self
                .http
                .delete(self.url.clone())
                .headers(self.headers.clone());
            if let Some(session) = self.session_id.lock().clone() {
                request = request.header("mcp-session-id", session);
            }
            if let Some(protocol) = self.negotiated_protocol.lock().clone() {
                request = request.header("mcp-protocol-version", protocol);
            }
            let _ = request.send().await;
        }
        Ok(())
    }

    fn closed_signal(&self) -> AbortSignal {
        self.signals.closed_signal()
    }

    fn list_change_generation(&self) -> u64 {
        self.signals.list_change_generation()
    }

    async fn wait_list_change(&self, after: u64) {
        self.signals.wait_list_change(after).await;
    }
}

fn validate_initialize(result: &Value) -> anyhow::Result<String> {
    let version = result
        .get("protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("MCP initialize response omitted protocolVersion"))?;
    anyhow::ensure!(
        SUPPORTED_PROTOCOL_VERSIONS.contains(&version),
        "MCP server selected unsupported protocol version {version}"
    );
    Ok(version.to_owned())
}

fn take_jsonrpc_result(mut reply: Value) -> anyhow::Result<Value> {
    if let Some(error) = reply.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("MCP request failed");
        anyhow::bail!("{message}");
    }
    reply
        .get_mut("result")
        .map(Value::take)
        .ok_or_else(|| anyhow::anyhow!("MCP response omitted result"))
}

fn parse_http_reply(text: &str, content_type: &str, id: u64) -> anyhow::Result<Option<Value>> {
    if content_type.contains("text/event-stream") || text.trim_start().starts_with("event:") {
        let normalized = text.replace("\r\n", "\n");
        for event in normalized.split("\n\n") {
            let data = event
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim_start)
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(&data)?;
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return Ok(Some(value));
            }
        }
        return Ok(None);
    }
    let value: Value = serde_json::from_str(text)?;
    Ok((value.get("id").and_then(Value::as_u64) == Some(id)).then_some(value))
}

async fn wait_sse_retry(closed: &AbortSignal) -> bool {
    tokio::select! {
        () = closed.cancelled() => true,
        () = tokio::time::sleep(Duration::from_millis(100)) => false,
    }
}

fn consume_sse_notifications(buffer: &mut Vec<u8>, signals: &McpClientSignals) {
    while let Some(end) = sse_event_end(buffer) {
        let event = buffer.drain(..end).collect::<Vec<_>>();
        let text = String::from_utf8_lossy(&event);
        let data = text
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        if value.get("method").and_then(Value::as_str) == Some(TOOL_LIST_CHANGED_NOTIFICATION) {
            signals.tools_changed();
        }
    }
}

fn sse_event_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .or_else(|| {
            buffer
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|index| index + 2)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_sse_notifications_accept_lf_crlf_and_split_frames() {
        let signals = McpClientSignals::default();
        let mut buffer = b"event: message\r\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\r\n"
            .to_vec();
        consume_sse_notifications(&mut buffer, &signals);
        assert_eq!(signals.list_change_generation(), 0);
        buffer.extend_from_slice(b"\r\n");
        consume_sse_notifications(&mut buffer, &signals);
        assert_eq!(signals.list_change_generation(), 1);
        buffer.extend_from_slice(
            b"data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/tools/list_changed\"}\n\n",
        );
        consume_sse_notifications(&mut buffer, &signals);
        assert_eq!(signals.list_change_generation(), 2);
    }
}
