//! Shared test fixtures for the web-fetch-http parity suites.

#![allow(
    dead_code,
    unreachable_pub,
    clippy::needless_pass_by_value,
    clippy::format_push_string
)]

use std::{collections::BTreeMap, sync::Arc};

use parking_lot::Mutex;
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

/// A fixed HTTP response.
#[derive(Clone)]
pub struct ResponseSpec {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl ResponseSpec {
    /// Builds a response with the given status, optional headers, and body.
    pub fn new(status: u16, headers: Vec<(String, String)>, body: Vec<u8>) -> Self {
        Self {
            status,
            headers,
            body,
        }
    }

    /// A plain-text response.
    pub fn plain(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self::new(
            status,
            vec![("content-type".to_owned(), content_type.to_owned())],
            body.into(),
        )
    }
}

/// How one request is answered.
#[derive(Clone)]
pub enum MockResponse {
    /// Write a complete response and close.
    Respond(ResponseSpec),
    /// Write nothing and hold the connection open.
    Stall,
    /// Write headers and a partial body, then hold the connection open.
    StallAfterPartial {
        /// Status line and headers.
        head: ResponseSpec,
        /// Partial body bytes written before stalling.
        partial: Vec<u8>,
    },
}

/// Handler deciding one request's response.
pub type Handler = Arc<dyn Fn(&CapturedRequest) -> MockResponse + Send + Sync>;

/// A loopback HTTP server driven by a request handler.
pub struct MockServer {
    /// Origin URL.
    pub url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    task: tokio::task::AbortHandle,
}

impl MockServer {
    /// Starts a server answering every request via the handler.
    pub async fn start(handler: Handler) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
        let address = listener.local_addr().expect("local addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let handler = handler.clone();
                let captured = captured.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, handler, captured).await;
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
    handler: Handler,
    captured: Arc<Mutex<Vec<CapturedRequest>>>,
) -> anyhow::Result<()> {
    let request = read_request(&mut stream).await?;
    captured.lock().push(request.clone());
    match handler(&request) {
        MockResponse::Respond(spec) => write_response(&mut stream, &spec).await,
        MockResponse::Stall => {
            futures::future::pending::<()>().await;
            Ok(())
        }
        MockResponse::StallAfterPartial { head, partial } => {
            write_response_head(&mut stream, head.status, &head.headers, partial.len()).await?;
            stream.write_all(&partial).await?;
            futures::future::pending::<()>().await;
            Ok(())
        }
    }
}

async fn write_response(stream: &mut TcpStream, spec: &ResponseSpec) -> anyhow::Result<()> {
    write_response_head(stream, spec.status, &spec.headers, spec.body.len()).await?;
    stream.write_all(&spec.body).await?;
    Ok(())
}

async fn write_response_head(
    stream: &mut TcpStream,
    status: u16,
    headers: &[(String, String)],
    body_len: usize,
) -> anyhow::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status} Test{CRLF}content-length: {body_len}{CRLF}connection: close{CRLF}"
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}{CRLF}"));
    }
    head.push_str(CRLF);
    stream.write_all(head.as_bytes()).await?;
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

