//! Loopback-only Responses-to-`DeepSeek` bridge consumed by the credentialed Codex e2e.
//!
//! Mirrors the source's `tests/deepseek-responses-bridge.ts`: the bridge answers exactly one
//! `POST /v1/responses`, forwards that task's text to the official `DeepSeek` chat endpoint under
//! the caller's bearer credential, and replays the answer as the fixture's Responses SSE events.
//! Every other route answers 404, a second task answers 409, an oversized body destroys the
//! connection, and any failure before the stream opens answers a 502 JSON error. Like the
//! fixture, the stream is close-delimited rather than chunked, which Codex consumes identically.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

use super::responses_fixture::{complete_events, response_input_texts};

/// The only upstream the bridge may spend a credential against.
pub(crate) const OFFICIAL_DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";
/// The source's request body cap; a larger body destroys the connection.
const MAX_REQUEST_BYTES: usize = 1_048_576;
/// Node's default request header cap.
const MAX_HEADER_BYTES: usize = 16 * 1024;

/// One running test-only Responses-to-DeepSeek bridge.
pub(crate) struct DeepSeekResponsesBridge {
    /// The Responses base URL Codex is configured with.
    pub(crate) base_url: String,
    completed: Arc<AtomicUsize>,
    accept: JoinHandle<()>,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

struct Shared {
    nonce: String,
    seen: AtomicUsize,
    completed: Arc<AtomicUsize>,
}

impl DeepSeekResponsesBridge {
    /// Starts the single-purpose loopback bridge for the one task that must request `nonce`.
    pub(crate) async fn start(nonce: String) -> anyhow::Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        let completed = Arc::new(AtomicUsize::new(0));
        let shared = Arc::new(Shared {
            nonce,
            seen: AtomicUsize::new(0),
            completed: Arc::clone(&completed),
        });
        let connections = Arc::new(Mutex::new(Vec::new()));
        let accept_connections = Arc::clone(&connections);
        let accept = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let shared = Arc::clone(&shared);
                accept_connections.lock().push(tokio::spawn(async move {
                    let _ = handle(socket, shared).await;
                }));
            }
        });
        Ok(Self {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            completed,
            accept,
            connections,
        })
    }

    /// How many tasks the bridge completed against `DeepSeek`.
    pub(crate) fn completed_requests(&self) -> usize {
        self.completed.load(Ordering::SeqCst)
    }

    /// Destroys every open response and closes the listener.
    pub(crate) fn close(self) {
        self.accept.abort();
        for connection in std::mem::take(&mut *self.connections.lock()) {
            connection.abort();
        }
    }
}

/// The task text the bridge forwards: the Responses input texts, else the instructions.
pub(crate) fn task_text(body: &Map<String, Value>) -> String {
    let input = response_input_texts(body).join("\n");
    if !input.trim().is_empty() {
        return input;
    }
    body.get("instructions")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_default()
}

/// Accepts only the official `DeepSeek` endpoint (trailing slashes ignored) as the upstream.
///
/// # Errors
///
/// Returns the source's refusal for any other configured base URL, an empty one included.
pub(crate) fn configured_base_url(configured: Option<&str>) -> anyhow::Result<String> {
    let configured = configured
        .unwrap_or(OFFICIAL_DEEPSEEK_BASE_URL)
        .trim_end_matches('/');
    anyhow::ensure!(
        configured == OFFICIAL_DEEPSEEK_BASE_URL,
        "Codex DeepSeek e2e requires the official DeepSeek base URL"
    );
    Ok(configured.to_owned())
}

fn deep_seek_base_url() -> anyhow::Result<String> {
    let configured = std::env::var("DEEPSEEK_BASE_URL").ok();
    configured_base_url(configured.as_deref())
}

fn bearer(authorization: Option<&str>) -> Option<&str> {
    authorization.filter(|value| value.starts_with("Bearer ") && value.len() > "Bearer ".len())
}

struct Head {
    method: String,
    path: String,
    authorization: Option<String>,
    content_length: usize,
    /// Body bytes that arrived together with the headers.
    buffered: Vec<u8>,
}

async fn read_head(socket: &mut TcpStream) -> anyhow::Result<Head> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        anyhow::ensure!(
            bytes.len() <= MAX_HEADER_BYTES,
            "bridge request headers too large"
        );
        anyhow::ensure!(
            socket.read_buf(&mut bytes).await? > 0,
            "bridge request closed before headers"
        );
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec())?;
    let mut lines = headers.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split_whitespace();
    let method = request_line.next().unwrap_or_default().to_owned();
    let path = request_line.next().unwrap_or_default().to_owned();
    let mut content_length = 0usize;
    let mut authorization = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        match name.to_ascii_lowercase().as_str() {
            "content-length" => content_length = value.trim().parse()?,
            "authorization" => authorization = Some(value.trim().to_owned()),
            _ => {}
        }
    }
    Ok(Head {
        method,
        path,
        authorization,
        content_length,
        buffered: bytes[header_end..].to_vec(),
    })
}

async fn read_body(socket: &mut TcpStream, head: &mut Head) -> anyhow::Result<Vec<u8>> {
    let mut body = std::mem::take(&mut head.buffered);
    anyhow::ensure!(
        head.content_length <= MAX_REQUEST_BYTES,
        "DeepSeek bridge request exceeded its byte limit"
    );
    while body.len() < head.content_length {
        anyhow::ensure!(
            socket.read_buf(&mut body).await? > 0,
            "bridge body truncated"
        );
        anyhow::ensure!(
            body.len() <= MAX_REQUEST_BYTES,
            "DeepSeek bridge request exceeded its byte limit"
        );
    }
    body.truncate(head.content_length);
    Ok(body)
}

async fn handle(mut socket: TcpStream, shared: Arc<Shared>) -> anyhow::Result<()> {
    let mut head = read_head(&mut socket).await?;
    if head.method != "POST" || head.path != "/v1/responses" {
        return write_empty(&mut socket, 404, "Not Found").await;
    }
    if shared.seen.fetch_add(1, Ordering::SeqCst) != 0 {
        return write_empty(&mut socket, 409, "Conflict").await;
    }
    let Some(authorization) = bearer(head.authorization.as_deref()).map(str::to_owned) else {
        // The source throws "received no bearer credential"; the client sees the generic failure.
        return write_json_error(&mut socket, 502, "DeepSeek bridge request failed").await;
    };
    // An oversized or truncated body destroys the connection without an answer.
    let body = read_body(&mut socket, &mut head).await?;
    match complete(&authorization, &body, &shared.nonce).await {
        Ok(text) => {
            shared.completed.fetch_add(1, Ordering::SeqCst);
            write_events(&mut socket, &text).await
        }
        Err(_) => write_json_error(&mut socket, 502, "DeepSeek bridge request failed").await,
    }
}

async fn complete(authorization: &str, body: &[u8], nonce: &str) -> anyhow::Result<String> {
    let body = match serde_json::from_slice::<Value>(body)? {
        Value::Object(body) => body,
        _ => Map::new(),
    };
    let task = task_text(&body);
    anyhow::ensure!(
        task.contains(nonce),
        "Codex DeepSeek bridge request omitted the expected nonce"
    );
    complete_with_deepseek(authorization, &task).await
}

async fn complete_with_deepseek(authorization: &str, task: &str) -> anyhow::Result<String> {
    let base_url = deep_seek_base_url()?;
    // Node's fetch ignores the proxy environment; reqwest would honour it.
    let client = reqwest::Client::builder().no_proxy().build()?;
    let response = client
        .post(format!("{base_url}/chat/completions"))
        .header("authorization", authorization)
        .header("content-type", "application/json")
        .body(
            json!({
                "model": "deepseek-v4-flash",
                "messages": [
                    {
                        "role": "system",
                        "content": "Follow the user instruction and return only the requested nonce.",
                    },
                    {"role": "user", "content": task},
                ],
                "temperature": 0,
                "max_tokens": 64,
                "stream": false,
            })
            .to_string(),
        )
        .send()
        .await?;
    anyhow::ensure!(
        response.status().is_success(),
        "DeepSeek bridge upstream returned HTTP {}",
        response.status().as_u16()
    );
    let payload: Value = serde_json::from_slice(&response.bytes().await?)?;
    let content = payload
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .filter(|content| !content.trim().is_empty());
    let Some(content) = content else {
        anyhow::bail!("DeepSeek bridge upstream returned no text");
    };
    Ok(content.to_owned())
}

async fn write_empty(socket: &mut TcpStream, status: u16, reason: &str) -> anyhow::Result<()> {
    socket
        .write_all(
            format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await?;
    socket.shutdown().await?;
    Ok(())
}

async fn write_json_error(
    socket: &mut TcpStream,
    status: u16,
    message: &str,
) -> anyhow::Result<()> {
    let body = json!({"error":{"message":message}}).to_string();
    socket
        .write_all(
            format!(
                "HTTP/1.1 {status} Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await?;
    socket.shutdown().await?;
    Ok(())
}

async fn write_events(socket: &mut TcpStream, text: &str) -> anyhow::Result<()> {
    socket
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\nX-Request-Id: req_deepseek_e2e\r\n\r\n",
        )
        .await?;
    for event in complete_events(text) {
        socket
            .write_all(format!("data: {event}\n\n").as_bytes())
            .await?;
    }
    socket.write_all(b"data: [DONE]\n\n").await?;
    socket.shutdown().await?;
    Ok(())
}
