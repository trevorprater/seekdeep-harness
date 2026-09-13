//! Loopback-only Anthropic Messages SSE fixture for the native Claude CLI.

use std::{fmt::Write as _, sync::Arc};

use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, oneshot},
};

#[derive(Clone, Debug)]
pub(crate) enum Behavior {
    Complete { text: String },
    Hold,
}

#[derive(Clone, Debug)]
pub(crate) struct RecordedRequest {
    pub(crate) path: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Value,
}

pub(crate) struct MessagesFixture {
    pub(crate) base_url: String,
    pub(crate) requests: Arc<Mutex<Vec<RecordedRequest>>>,
    pub(crate) started: Arc<Semaphore>,
    shutdown: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl MessagesFixture {
    pub(crate) async fn start(behavior: Behavior) -> anyhow::Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let started = Arc::new(Semaphore::new(0));
        let (shutdown, mut shutdown_rx) = oneshot::channel();
        let task_requests = Arc::clone(&requests);
        let task_started = Arc::clone(&started);
        let task = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    result = listener.accept() => result,
                    _ = &mut shutdown_rx => return,
                };
                let Ok((stream, _)) = accepted else {
                    return;
                };
                let requests = Arc::clone(&task_requests);
                let started = Arc::clone(&task_started);
                let behavior = behavior.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, behavior, requests, started).await;
                });
            }
        });
        Ok(Self {
            base_url: format!("http://127.0.0.1:{port}"),
            requests,
            started,
            shutdown: Some(shutdown),
            task,
        })
    }

    pub(crate) async fn close(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let _ = self.task.await;
    }
}

async fn serve(
    mut stream: TcpStream,
    behavior: Behavior,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    started: Arc<Semaphore>,
) -> anyhow::Result<()> {
    let mut bytes = Vec::new();
    let mut scratch = [0_u8; 4096];
    let header_end;
    loop {
        let count = stream.read(&mut scratch).await?;
        if count == 0 {
            return Ok(());
        }
        bytes.extend_from_slice(&scratch[..count]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            header_end = index + 4;
            break;
        }
    }
    let header = String::from_utf8_lossy(&bytes[..header_end]);
    let mut lines = header.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_owned();
    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        })
        .collect::<Vec<_>>();
    let content_length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        let count = stream.read(&mut scratch).await?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&scratch[..count]);
    }
    let body = serde_json::from_slice(&bytes[header_end..header_end + content_length])?;
    requests.lock().push(RecordedRequest {
        path,
        headers,
        body,
    });
    started.add_permits(1);
    match behavior {
        Behavior::Complete { text } => complete(&mut stream, &text).await?,
        Behavior::Hold => {
            let mut byte = [0_u8; 1];
            let _ = stream.read(&mut byte).await;
        }
    }
    Ok(())
}

async fn complete(stream: &mut TcpStream, text: &str) -> anyhow::Result<()> {
    let events = [
        (
            "message_start",
            json!({
                "type":"message_start",
                "message":{
                    "id":"msg_seekdeep_fixture","type":"message","role":"assistant",
                    "model":"fixture-model","content":[],"stop_reason":null,
                    "stop_sequence":null,
                    "usage":{"input_tokens":7,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}
                }
            }),
        ),
        (
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        ),
        (
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
        ),
        (
            "content_block_stop",
            json!({"type":"content_block_stop","index":0}),
        ),
        (
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":1}}),
        ),
        ("message_stop", json!({"type":"message_stop"})),
    ];
    let mut body = String::new();
    for (event, payload) in events {
        write!(&mut body, "event: {event}\ndata: {payload}\n\n")
            .expect("writing to a String is infallible");
    }
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}
