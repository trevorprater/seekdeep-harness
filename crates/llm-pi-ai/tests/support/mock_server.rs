//! Scripted local HTTP/SSE provider used by protocol integration tests.

use std::{
    collections::{HashMap, VecDeque},
    fmt::Write as _,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use parking_lot::Mutex;
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::AbortHandle,
};

/// One scripted response consumed by the next request.
#[derive(Clone, Debug, Default)]
pub(crate) struct Behavior {
    /// HTTP status; absent means 200.
    pub(crate) status: Option<u16>,
    /// Ordered SSE data payloads.
    pub(crate) events: Vec<String>,
    /// Non-stream response body.
    pub(crate) body: Option<String>,
    /// Delay between emitted SSE events.
    pub(crate) delay: Option<Duration>,
    /// Additional response headers.
    pub(crate) headers: HashMap<String, String>,
}

/// Exact request facts captured after the complete body is read.
#[derive(Clone, Debug)]
pub(crate) struct CapturedRequest {
    /// Request path including query.
    pub(crate) path: String,
    /// Parsed JSON body, absent for an empty body.
    pub(crate) body: Option<Value>,
    /// Lowercase request headers.
    pub(crate) headers: HashMap<String, String>,
}

/// Local provider stand-in with ordered scripted behaviors.
pub(crate) struct MockServer {
    /// Loopback origin.
    pub(crate) url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    closed_responses: Arc<AtomicUsize>,
    response_closed: Arc<Notify>,
    task: AbortHandle,
}

impl MockServer {
    /// Starts a server and consumes one behavior per request.
    pub(crate) async fn start(script: Vec<Behavior>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let script = Arc::new(Mutex::new(VecDeque::from(script)));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let closed_responses = Arc::new(AtomicUsize::new(0));
        let response_closed = Arc::new(Notify::new());
        let task_script = script.clone();
        let task_requests = requests.clone();
        let task_closed = closed_responses.clone();
        let task_notify = response_closed.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let behavior = task_script.lock().pop_front().unwrap_or_else(|| Behavior {
                    status: Some(500),
                    body: Some("script exhausted".to_owned()),
                    ..Behavior::default()
                });
                let requests = task_requests.clone();
                let closed = task_closed.clone();
                let notify = task_notify.clone();
                tokio::spawn(async move {
                    let _ = serve(socket, behavior, requests).await;
                    closed.fetch_add(1, Ordering::SeqCst);
                    notify.notify_waiters();
                });
            }
        })
        .abort_handle();
        Self {
            url: format!("http://{address}"),
            requests,
            closed_responses,
            response_closed,
            task,
        }
    }

    /// Captured requests in arrival order.
    pub(crate) fn requests(&self) -> Vec<CapturedRequest> {
        self.requests.lock().clone()
    }

    /// Number of response connections that ended normally or after cancellation.
    pub(crate) fn closed_responses(&self) -> usize {
        self.closed_responses.load(Ordering::SeqCst)
    }

    /// Waits until at least `expected` response connections have closed.
    pub(crate) async fn wait_for_closed(&self, expected: usize) {
        while self.closed_responses() < expected {
            self.response_closed.notified().await;
        }
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    mut socket: TcpStream,
    behavior: Behavior,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
) -> anyhow::Result<()> {
    let captured = read_request(&mut socket).await?;
    requests.lock().push(captured);
    let status = behavior.status.unwrap_or(200);
    if status != 200 {
        let body = behavior.body.unwrap_or_else(|| "{}".to_owned());
        let mut head = format!(
            "HTTP/1.1 {status} Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
            body.len()
        );
        for (name, value) in behavior.headers {
            write!(head, "{name}: {value}\r\n").unwrap();
        }
        socket
            .write_all(format!("{head}\r\n{body}").as_bytes())
            .await?;
        return Ok(());
    }
    socket
        .write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
        )
        .await?;
    for event in behavior.events {
        socket
            .write_all(format!("data: {event}\n\n").as_bytes())
            .await?;
        if let Some(delay) = behavior.delay {
            tokio::time::sleep(delay).await;
        }
    }
    Ok(())
}

async fn read_request(socket: &mut TcpStream) -> anyhow::Result<CapturedRequest> {
    let mut bytes = Vec::new();
    let boundary = loop {
        let mut buffer = [0_u8; 4096];
        let count = socket.read(&mut buffer).await?;
        anyhow::ensure!(count > 0, "request closed before headers");
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let head = std::str::from_utf8(&bytes[..boundary])?;
    let path = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_owned();
    let headers = head
        .split("\r\n")
        .skip(1)
        .filter(|line| !line.is_empty())
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect::<HashMap<_, _>>();
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_default();
    while bytes.len() < boundary + length {
        let mut buffer = [0_u8; 4096];
        let count = socket.read(&mut buffer).await?;
        anyhow::ensure!(count > 0, "request closed before body");
        bytes.extend_from_slice(&buffer[..count]);
    }
    let wire_body = &bytes[boundary..boundary + length];
    let body = if wire_body.is_empty() {
        None
    } else {
        let decoded = if headers.get("content-encoding").map(String::as_str) == Some("zstd") {
            zstd::stream::decode_all(wire_body)?
        } else {
            wire_body.to_vec()
        };
        Some(serde_json::from_slice(&decoded)?)
    };
    Ok(CapturedRequest {
        path,
        body,
        headers,
    })
}
