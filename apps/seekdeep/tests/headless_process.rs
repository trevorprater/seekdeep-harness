//! Credential-free acceptance of the shipped `seekdeep` process boundary.

use std::{collections::BTreeMap, process::Stdio, time::Duration};

use seekdeep::{DEFAULT_MODEL, DEFAULT_PROVIDER};
use seekdeep_cordis::Context;
use seekdeep_core::session_store::SessionStore;
use seekdeep_session_persistence::SessionPersistence as _;
use seekdeep_session_persistence_jsonl::{JsonlConfig, JsonlSessionPersistence};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    process::Command,
};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(20);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const TASK: &str = "prove the executable path";
const ANSWER: &str = "HEADLESS_PROCESS_ROUND_TRIP";

#[derive(Debug)]
struct CapturedRequest {
    path: String,
    headers: BTreeMap<String, String>,
    body: Value,
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
    let mut lines = head.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("request line missing"))?;
    let path = request_line
        .split_ascii_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("request path missing"))?
        .to_owned();
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("malformed request header"))?;
        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    let length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    while bytes.len() < boundary + length {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0, "request closed before body");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(CapturedRequest {
        path,
        headers,
        body: serde_json::from_slice(&bytes[boundary..boundary + length])?,
    })
}

async fn write_sse(stream: &mut TcpStream, events: &[Value]) -> anyhow::Result<()> {
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        if event.as_str() == Some("[DONE]") {
            body.push_str("[DONE]");
        } else {
            body.push_str(&event.to_string());
        }
        body.push_str("\n\n");
    }
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

async fn loopback_server() -> anyhow::Result<(
    String,
    tokio::task::JoinHandle<anyhow::Result<Vec<CapturedRequest>>>,
)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        let mut captured = Vec::new();

        let (mut first, _) = listener.accept().await?;
        captured.push(read_request(&mut first).await?);
        write_sse(
            &mut first,
            &[
                serde_json::json!({
                    "choices": [{
                        "delta": {
                            "role": "assistant",
                            "content": null,
                            "reasoning_content": ""
                        }
                    }]
                }),
                serde_json::json!({
                    "choices": [{
                        "delta": {
                            "tool_calls": [{
                                "index": 0,
                                "id": "headless-process-tool",
                                "type": "function",
                                "function": {
                                    "name": "todo_write",
                                    "arguments": serde_json::json!({
                                        "todos": [{
                                            "content": "prove the process boundary",
                                            "status": "completed"
                                        }]
                                    }).to_string()
                                }
                            }]
                        }
                    }]
                }),
                serde_json::json!({
                    "choices": [{"delta": {}, "finish_reason": "tool_calls"}]
                }),
                Value::String("[DONE]".to_owned()),
            ],
        )
        .await?;

        let (mut second, _) = listener.accept().await?;
        captured.push(read_request(&mut second).await?);
        write_sse(
            &mut second,
            &[
                serde_json::json!({
                    "choices": [{
                        "delta": {
                            "role": "assistant",
                            "content": null,
                            "reasoning_content": ""
                        }
                    }]
                }),
                serde_json::json!({
                    "choices": [{"delta": {"content": ANSWER}}]
                }),
                serde_json::json!({
                    "choices": [{"delta": {}, "finish_reason": "stop"}]
                }),
                Value::String("[DONE]".to_owned()),
            ],
        )
        .await?;

        Ok(captured)
    });
    Ok((format!("http://{address}"), task))
}

fn ordered_subsequence(events: &[String], expected: &[&str]) -> bool {
    let mut cursor = 0;
    for event in events {
        if expected
            .get(cursor)
            .is_some_and(|expected| event == expected)
        {
            cursor += 1;
        }
    }
    cursor == expected.len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binary_runs_tool_prints_answer_and_cold_reopens_jsonl() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let home = temporary.path().join("home");
    let (base_url, server) = loopback_server().await?;

    let mut command = Command::new(env!("CARGO_BIN_EXE_seekdeep"));
    command
        .current_dir(&workspace)
        .args([
            "--profile",
            "headless",
            "prove",
            "the",
            "executable",
            "path",
        ])
        .env_clear()
        .env("SEEKDEEP_HOME", &home)
        .env("DEEPSEEK_API_KEY", "fake-process-key")
        .env("DEEPSEEK_BASE_URL", &base_url)
        .env("SEEKDEEP_TOOLS_MODE", "native")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = tokio::time::timeout(PROCESS_TIMEOUT, command.output())
        .await
        .map_err(|_| anyhow::anyhow!("seekdeep process timed out"))??;
    if output.status.code() != Some(0) {
        server.abort();
    }
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, format!("{ANSWER}\n").as_bytes());
    assert_eq!(output.stderr, b"");

    let captured = tokio::time::timeout(IO_TIMEOUT, server)
        .await
        .map_err(|_| anyhow::anyhow!("loopback server did not finish"))???;
    assert_eq!(captured.len(), 2);
    assert!(
        captured
            .iter()
            .all(|request| request.path == "/chat/completions")
    );
    assert!(captured.iter().all(|request| {
        request.headers.get("authorization").map(String::as_str) == Some("Bearer fake-process-key")
    }));
    let session_headers = captured
        .iter()
        .map(|request| {
            request
                .headers
                .get("x-seekdeep-harness-session-id")
                .map(String::as_str)
        })
        .collect::<Vec<_>>();
    assert_eq!(session_headers[0], session_headers[1]);
    assert!(session_headers[0].is_some_and(|id| id.starts_with("session-")));
    assert_eq!(captured[0].body["model"], DEFAULT_MODEL);
    assert_eq!(captured[1].body["model"], DEFAULT_MODEL);
    assert!(captured[0].body.to_string().contains(TASK));
    assert!(captured[0].body.to_string().contains("todo_write"));
    assert!(
        captured[1]
            .body
            .to_string()
            .contains("headless-process-tool")
    );

    let cold_root = Context::new();
    let cold_sessions = SessionStore::install(&cold_root)?;
    let cold =
        JsonlSessionPersistence::new(cold_sessions, JsonlConfig::new(home.join("sessions")))?;
    let headers = tokio::time::timeout(IO_TIMEOUT, cold.list(None))
        .await
        .map_err(|_| anyhow::anyhow!("cold session listing timed out"))??;
    assert_eq!(headers.len(), 1);
    let header = &headers[0];
    assert!(header.id.as_str().starts_with("session-"));
    assert_eq!(
        header.cwd.as_deref(),
        Some(workspace.to_string_lossy().as_ref())
    );
    let inspection = tokio::time::timeout(IO_TIMEOUT, cold.inspect(&header.id, None))
        .await
        .map_err(|_| anyhow::anyhow!("cold session inspection timed out"))??;
    let event_types = inspection
        .events
        .iter()
        .map(|event| event.event_type.clone())
        .collect::<Vec<_>>();
    assert!(
        ordered_subsequence(
            &event_types,
            &[
                "turn/start",
                "user/message",
                "assistant/message",
                "tool/call",
                "todo/write",
                "tool/result",
                "assistant/message",
                "turn/end",
            ],
        ),
        "unexpected cold event sequence: {event_types:?}"
    );
    assert!(inspection.events.iter().any(|event| {
        event.event_type == "user/message" && event.data.to_string().contains(TASK)
    }));
    assert_eq!(
        inspection
            .events
            .iter()
            .find(|event| event.event_type == "todo/write")
            .and_then(|event| event.data.pointer("/todos/0/content"))
            .and_then(Value::as_str),
        Some("prove the process boundary")
    );
    assert!(inspection.events.iter().any(|event| {
        event.event_type == "assistant/message" && event.data.to_string().contains(ANSWER)
    }));
    assert_eq!(
        inspection
            .events
            .iter()
            .rev()
            .find(|event| event.event_type == "turn/end")
            .and_then(|event| event.data.pointer("/reason/kind"))
            .and_then(Value::as_str),
        Some("completed")
    );
    assert!(inspection.events.iter().any(|event| {
        event.event_type == "request/header"
            && event.data.to_string().contains(DEFAULT_PROVIDER)
            && event.data.to_string().contains(DEFAULT_MODEL)
    }));
    cold_root.fiber().dispose().await?;
    Ok(())
}
