//! Shared test fixtures for the web-search-perplexity parity suites.

#![allow(
    dead_code,
    unreachable_pub,
    clippy::needless_pass_by_value,
    clippy::format_push_string
)]

use std::{collections::BTreeMap, sync::Arc};

use parking_lot::Mutex;
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
};

/// One captured HTTP request.
#[derive(Clone, Debug)]
pub struct CapturedRequest {
    /// HTTP method.
    pub method: String,
    /// Request path (includes query).
    pub path: String,
    /// Lowercased request headers.
    pub headers: BTreeMap<String, String>,
    /// Raw request body.
    pub body: Vec<u8>,
}

/// A fixed HTTP response the mock server replays.
#[derive(Clone)]
pub struct ResponseSpec {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl ResponseSpec {
    /// A JSON response with a content-type header.
    pub fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: serde_json::to_vec(&body).expect("fixture body serializes"),
        }
    }

    /// A plain-text response.
    pub fn plain(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into().into_bytes(),
        }
    }

    /// Adds a response header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

/// A loopback HTTP server that replays the first scripted response and records requests.
pub struct MockServer {
    /// Origin URL.
    pub url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    task: tokio::task::AbortHandle,
}

impl MockServer {
    /// Starts a server replaying one response for every request.
    pub async fn start(response: ResponseSpec) -> Self {
        Self::start_script(vec![response]).await
    }

    /// Starts a server replaying the first scripted response for every request.
    pub async fn start_script(responses: Vec<ResponseSpec>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let address = listener.local_addr().expect("local addr");
        let responses = Arc::new(Mutex::new(responses));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let responses = responses.clone();
                let captured = captured.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, responses, captured).await;
                });
            }
        });
        let handle = task.abort_handle();
        drop(task);
        Self {
            url: format!("http://{address}"),
            requests,
            task: handle,
        }
    }

    /// Takes all captured requests, clearing the log.
    pub fn take_requests(&self) -> Vec<CapturedRequest> {
        std::mem::take(&mut *self.requests.lock())
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

const CRLF: &str = "\r\n";

async fn serve(
    mut stream: TcpStream,
    responses: Arc<Mutex<Vec<ResponseSpec>>>,
    captured: Arc<Mutex<Vec<CapturedRequest>>>,
) -> anyhow::Result<()> {
    let request = read_request(&mut stream).await?;
    captured.lock().push(request);
    let response = responses
        .lock()
        .first()
        .cloned()
        .unwrap_or_else(|| ResponseSpec::plain(500, ""));
    let mut head = format!(
        "HTTP/1.1 {} Test{CRLF}content-length: {}{CRLF}connection: close{CRLF}",
        response.status,
        response.body.len()
    );
    for (name, value) in response.headers {
        head.push_str(&format!("{name}: {value}{CRLF}"));
    }
    head.push_str(CRLF);
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(&response.body).await?;
    Ok(())
}

async fn read_request(stream: &mut TcpStream) -> anyhow::Result<CapturedRequest> {
    let mut bytes = Vec::new();
    let boundary = loop {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0, "request closed before headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(boundary) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break boundary + 4;
        }
    };
    let head = std::str::from_utf8(&bytes[..boundary])?;
    let mut lines = head.split(CRLF);
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = bytes[boundary..].to_vec();
    while body.len() < content_length {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..count]);
    }
    body.truncate(content_length);
    Ok(CapturedRequest {
        method,
        path,
        headers,
        body,
    })
}
