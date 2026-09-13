//! Loopback-only minimal Responses SSE server consumed by real Codex tests.

use std::{collections::VecDeque, sync::Arc};

use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::JoinHandle,
};

#[derive(Clone)]
pub(crate) enum Behavior {
    Complete {
        text: String,
    },
    AdvertisedFunctionCall {
        choices: Vec<(String, Map<String, Value>)>,
    },
    Hold,
}

#[derive(Clone, Debug)]
pub(crate) struct RecordedRequest {
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) authorization: Option<String>,
    pub(crate) body: Map<String, Value>,
}

pub(crate) struct ResponsesFixture {
    pub(crate) base_url: String,
    pub(crate) requests: Arc<Mutex<Vec<RecordedRequest>>>,
    started: Arc<Notify>,
    accept: JoinHandle<()>,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl ResponsesFixture {
    pub(crate) async fn start(script: Vec<Behavior>) -> anyhow::Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let behaviors = Arc::new(Mutex::new(VecDeque::from(script)));
        let started = Arc::new(Notify::new());
        let connections = Arc::new(Mutex::new(Vec::new()));
        let accept_requests = Arc::clone(&requests);
        let accept_behaviors = Arc::clone(&behaviors);
        let accept_started = Arc::clone(&started);
        let accept_connections = Arc::clone(&connections);
        let accept = tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                let requests = Arc::clone(&accept_requests);
                let behaviors = Arc::clone(&accept_behaviors);
                let started = Arc::clone(&accept_started);
                accept_connections.lock().push(tokio::spawn(async move {
                    let _ = handle(socket, requests, behaviors, started).await;
                }));
            }
        });
        Ok(Self {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            requests,
            started,
            accept,
            connections,
        })
    }

    pub(crate) async fn wait_started(&self) {
        if !self.requests.lock().is_empty() {
            return;
        }
        self.started.notified().await;
    }

    pub(crate) fn close(self) {
        self.accept.abort();
        for connection in std::mem::take(&mut *self.connections.lock()) {
            connection.abort();
        }
    }
}

async fn handle(
    mut socket: TcpStream,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    behaviors: Arc<Mutex<VecDeque<Behavior>>>,
    started: Arc<Notify>,
) -> anyhow::Result<()> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        let read = socket.read_buf(&mut bytes).await?;
        anyhow::ensure!(read > 0, "fixture HTTP request closed before headers");
        anyhow::ensure!(bytes.len() <= 2 * 1024 * 1024, "fixture request too large");
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec())?;
    let mut lines = headers.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default().to_owned();
    let path = request_parts.next().unwrap_or_default().to_owned();
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
    while bytes.len() < header_end + content_length {
        anyhow::ensure!(
            socket.read_buf(&mut bytes).await? > 0,
            "fixture body truncated"
        );
    }
    let Value::Object(body) =
        serde_json::from_slice(&bytes[header_end..header_end + content_length])?
    else {
        anyhow::bail!("fixture body is not an object");
    };
    requests.lock().push(RecordedRequest {
        method,
        path,
        authorization,
        body: body.clone(),
    });
    started.notify_waiters();
    let Some(behavior) = behaviors.lock().pop_front() else {
        write_json_error(&mut socket, 500, "fixture script exhausted").await?;
        return Ok(());
    };
    let events = match behavior {
        Behavior::Complete { text } => complete_events(&text),
        Behavior::AdvertisedFunctionCall { choices } => {
            let advertised = advertised_function_names(&body);
            let Some((name, arguments)) = choices
                .into_iter()
                .find(|(name, _)| advertised.contains(name))
            else {
                write_json_error(&mut socket, 500, "no fixture function call was advertised")
                    .await?;
                return Ok(());
            };
            function_call_events(&name, &arguments)
        }
        Behavior::Hold => {
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
                )
                .await?;
            std::future::pending::<()>().await;
            unreachable!()
        }
    };
    socket
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\nX-Request-Id: req_fixture\r\n\r\n",
        )
        .await?;
    for event in events {
        socket
            .write_all(format!("data: {event}\n\n").as_bytes())
            .await?;
    }
    socket.write_all(b"data: [DONE]\n\n").await?;
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
    Ok(())
}

fn advertised_function_names(body: &Map<String, Value>) -> std::collections::HashSet<String> {
    body.get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|tool| {
            (tool.get("type")?.as_str()? == "function")
                .then(|| tool.get("name")?.as_str().map(str::to_owned))?
        })
        .collect()
}

pub(crate) fn response_input_texts(body: &Map<String, Value>) -> Vec<String> {
    body.get("input")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .flat_map(|item| {
            item.get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter_map(|part| part.get("text").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

fn complete_events(text: &str) -> Vec<Value> {
    let completed = response_object(text);
    let message = completed["output"][0].clone();
    let part = message["content"][0].clone();
    vec![
        json!({"type":"response.created","response":merge(&completed, json!({"status":"in_progress","output":[]}))}),
        json!({"type":"response.output_item.added","output_index":0,"item":merge(&message, json!({"status":"in_progress","content":[]}))}),
        json!({"type":"response.content_part.added","item_id":message["id"],"output_index":0,"content_index":0,"part":merge(&part,json!({"text":""}))}),
        json!({"type":"response.output_text.delta","item_id":message["id"],"output_index":0,"content_index":0,"delta":text,"logprobs":[]}),
        json!({"type":"response.output_text.done","item_id":message["id"],"output_index":0,"content_index":0,"text":text,"logprobs":[]}),
        json!({"type":"response.content_part.done","item_id":message["id"],"output_index":0,"content_index":0,"part":part}),
        json!({"type":"response.output_item.done","output_index":0,"item":message}),
        json!({"type":"response.completed","response":completed}),
    ]
}

fn function_call_events(name: &str, arguments: &Map<String, Value>) -> Vec<Value> {
    let arguments = Value::Object(arguments.clone()).to_string();
    let item = json!({
        "id":"fc_fixture","type":"function_call","status":"completed",
        "name":name,"arguments":arguments,"call_id":"call_fixture"
    });
    let completed = merge(
        &response_object(""),
        json!({
            "output":[item],
            "usage":{"input_tokens":10,"input_tokens_details":{"cached_tokens":0},"output_tokens":5,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":15}
        }),
    );
    vec![
        json!({"type":"response.created","response":merge(&completed,json!({"status":"in_progress","output":[]}))}),
        json!({"type":"response.output_item.added","output_index":0,"item":merge(&item,json!({"status":"in_progress","arguments":""}))}),
        json!({"type":"response.function_call_arguments.delta","item_id":"fc_fixture","output_index":0,"delta":arguments}),
        json!({"type":"response.function_call_arguments.done","item_id":"fc_fixture","output_index":0,"arguments":arguments}),
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.completed","response":completed}),
    ]
}

fn response_object(text: &str) -> Value {
    let message = json!({
        "id":"msg_fixture","type":"message","status":"completed","role":"assistant",
        "content":[{"type":"output_text","annotations":[],"logprobs":[],"text":text}]
    });
    json!({
        "id":"resp_fixture","object":"response","created_at":1,"status":"completed",
        "background":false,"error":null,"incomplete_details":null,"instructions":null,
        "max_output_tokens":null,"max_tool_calls":null,"model":"fixture-model","output":[message],
        "parallel_tool_calls":true,"previous_response_id":null,"prompt_cache_key":null,
        "prompt_cache_retention":null,"reasoning":{"effort":null,"summary":null},
        "safety_identifier":null,"service_tier":"default","store":false,"temperature":null,
        "text":{"format":{"type":"text"},"verbosity":"medium"},"tool_choice":"auto",
        "tools":[],"top_logprobs":0,"top_p":null,"truncation":"disabled",
        "usage":{"input_tokens":10,"input_tokens_details":{"cached_tokens":0},"output_tokens":1,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":11},
        "user":null,"metadata":{}
    })
}

fn merge(base: &Value, overlay: Value) -> Value {
    let mut base = base.as_object().cloned().unwrap_or_default();
    if let Value::Object(overlay) = overlay {
        base.extend(overlay);
    }
    Value::Object(base)
}
