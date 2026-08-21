//! Credential-free acceptance of the shipped `seekdeep` process boundary.

use std::{
    collections::BTreeMap,
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use seekdeep::{DEFAULT_MODEL, DEFAULT_PROVIDER};
use seekdeep_cordis::Context;
use seekdeep_core::session_store::SessionStore;
use seekdeep_session_persistence::SessionPersistence as _;
use seekdeep_session_persistence_jsonl::{JsonlConfig, JsonlSessionPersistence};
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    process::{Child, Command},
    task::JoinHandle,
};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(20);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const TASK: &str = "prove the executable path";
const ANSWER: &str = "HEADLESS_PROCESS_ROUND_TRIP";

type PipeReader = JoinHandle<std::io::Result<Vec<u8>>>;

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

async fn loopback_server(
    request_count: Arc<AtomicUsize>,
) -> anyhow::Result<(
    String,
    tokio::task::JoinHandle<anyhow::Result<Vec<CapturedRequest>>>,
)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        let mut captured = Vec::new();

        let (mut first, _) = listener.accept().await?;
        captured.push(read_request(&mut first).await?);
        request_count.fetch_add(1, Ordering::Release);
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
        request_count.fetch_add(1, Ordering::Release);
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

async fn run_process(
    workspace: &Path,
    home: &Path,
    base_url: &str,
    request_count: &AtomicUsize,
) -> anyhow::Result<std::process::Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_seekdeep"));
    command
        .current_dir(workspace)
        .args([
            "--profile",
            "headless",
            "prove",
            "the",
            "executable",
            "path",
        ])
        .env_clear()
        .env("SEEKDEEP_HOME", home)
        .env("DEEPSEEK_API_KEY", "fake-process-key")
        .env("DEEPSEEK_BASE_URL", base_url)
        .env("SEEKDEEP_TOOLS_MODE", "native")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command.spawn()?;
    let child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("seekdeep stdout pipe missing"))?;
    let child_stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("seekdeep stderr pipe missing"))?;
    let stdout_reader = spawn_pipe_reader(child_stdout);
    let stderr_reader = spawn_pipe_reader(child_stderr);
    let status = match tokio::time::timeout(PROCESS_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            let cleanup = force_reap(&mut child).await;
            let (stdout, stderr) = finish_pipe_readers(stdout_reader, stderr_reader).await?;
            anyhow::bail!(
                "waiting for seekdeep failed after receiving {} requests: {error}; cleanup: {cleanup:#?}; stdout: {:?}; stderr: {:?}",
                request_count.load(Ordering::Acquire),
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            );
        }
        Err(_) => {
            let cleanup = force_reap(&mut child).await;
            let (stdout, stderr) = finish_pipe_readers(stdout_reader, stderr_reader).await?;
            anyhow::bail!(
                "seekdeep process timed out after receiving {} requests; cleanup: {cleanup:#?}; stdout: {:?}; stderr: {:?}",
                request_count.load(Ordering::Acquire),
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            );
        }
    };
    let (stdout, stderr) = finish_pipe_readers(stdout_reader, stderr_reader).await?;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn spawn_pipe_reader(mut pipe: impl AsyncRead + Unpin + Send + 'static) -> PipeReader {
    tokio::spawn(async move {
        let mut output = Vec::new();
        pipe.read_to_end(&mut output).await?;
        Ok(output)
    })
}

async fn finish_pipe_readers(
    stdout: PipeReader,
    stderr: PipeReader,
) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let (stdout, stderr) = tokio::join!(
        finish_pipe_reader(stdout, "stdout"),
        finish_pipe_reader(stderr, "stderr"),
    );
    Ok((stdout?, stderr?))
}

async fn finish_pipe_reader(mut reader: PipeReader, name: &str) -> anyhow::Result<Vec<u8>> {
    if let Ok(joined) = tokio::time::timeout(IO_TIMEOUT, &mut reader).await {
        Ok(joined??)
    } else {
        reader.abort();
        match tokio::time::timeout(IO_TIMEOUT, &mut reader).await {
            Ok(Err(error)) if error.is_cancelled() => {}
            Ok(Err(error)) => anyhow::bail!("seekdeep {name} reader abort failed: {error}"),
            Ok(Ok(result)) => anyhow::bail!(
                "seekdeep {name} reader completed during abort after failing to close: {result:?}"
            ),
            Err(_) => anyhow::bail!("seekdeep {name} reader did not join after abort"),
        }
        anyhow::bail!("seekdeep {name} did not close")
    }
}

async fn finish_server(
    mut server: JoinHandle<anyhow::Result<Vec<CapturedRequest>>>,
) -> anyhow::Result<Vec<CapturedRequest>> {
    if let Ok(joined) = tokio::time::timeout(IO_TIMEOUT, &mut server).await {
        joined?
    } else {
        server.abort();
        let cleanup = tokio::time::timeout(IO_TIMEOUT, &mut server).await;
        anyhow::bail!("loopback server did not finish; cleanup: {cleanup:#?}")
    }
}

async fn abort_server(
    mut server: JoinHandle<anyhow::Result<Vec<CapturedRequest>>>,
) -> anyhow::Result<()> {
    server.abort();
    match tokio::time::timeout(IO_TIMEOUT, &mut server).await {
        Ok(Err(error)) if error.is_cancelled() => Ok(()),
        Ok(Err(error)) => Err(error.into()),
        Ok(Ok(result)) => {
            result?;
            Ok(())
        }
        Err(_) => anyhow::bail!("loopback server did not join after abort"),
    }
}

async fn force_reap(child: &mut Child) -> anyhow::Result<std::process::ExitStatus> {
    if child.try_wait()?.is_none() {
        child.start_kill()?;
    }
    tokio::time::timeout(IO_TIMEOUT, child.wait())
        .await
        .map_err(|_| anyhow::anyhow!("seekdeep child did not reap after kill"))?
        .map_err(Into::into)
}

fn assert_process_output(output: &std::process::Output) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, format!("{ANSWER}\n").as_bytes());
    assert_eq!(output.stderr, b"");
}

fn assert_requests(captured: &[CapturedRequest]) {
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
}

#[derive(Debug)]
struct ColdEvidence {
    session_id: String,
    cwd: Option<String>,
    canonical_workspace: String,
    event_types: Vec<String>,
    has_task: bool,
    todo_content: Option<String>,
    has_answer: bool,
    final_reason: Option<String>,
    has_request_header: bool,
}

async fn collect_cold_jsonl(home: &Path, workspace: &Path) -> anyhow::Result<ColdEvidence> {
    let cold_root = Context::new();
    let operation = async {
        let cold_sessions = SessionStore::install(&cold_root)?;
        let cold =
            JsonlSessionPersistence::new(cold_sessions, JsonlConfig::new(home.join("sessions")))?;
        let headers = tokio::time::timeout(IO_TIMEOUT, cold.list(None))
            .await
            .map_err(|_| anyhow::anyhow!("cold session listing timed out"))??;
        anyhow::ensure!(
            headers.len() == 1,
            "expected one cold session, got {}",
            headers.len()
        );
        let header = &headers[0];
        let inspection = tokio::time::timeout(IO_TIMEOUT, cold.inspect(&header.id, None))
            .await
            .map_err(|_| anyhow::anyhow!("cold session inspection timed out"))??;
        let events = &inspection.events;
        Ok::<_, anyhow::Error>(ColdEvidence {
            session_id: header.id.to_string(),
            cwd: header.cwd.clone(),
            canonical_workspace: std::fs::canonicalize(workspace)?
                .to_string_lossy()
                .into_owned(),
            event_types: events
                .iter()
                .map(|event| event.event_type.clone())
                .collect(),
            has_task: events.iter().any(|event| {
                event.event_type == "user/message" && event.data.to_string().contains(TASK)
            }),
            todo_content: events
                .iter()
                .find(|event| event.event_type == "todo/write")
                .and_then(|event| event.data.pointer("/todos/0/content"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            has_answer: events.iter().any(|event| {
                event.event_type == "assistant/message" && event.data.to_string().contains(ANSWER)
            }),
            final_reason: events
                .iter()
                .rev()
                .find(|event| event.event_type == "turn/end")
                .and_then(|event| event.data.pointer("/reason/kind"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            has_request_header: events.iter().any(|event| {
                event.event_type == "request/header"
                    && event.data.to_string().contains(DEFAULT_PROVIDER)
                    && event.data.to_string().contains(DEFAULT_MODEL)
            }),
        })
    }
    .await;
    let cleanup = cold_root.fiber().dispose().await;
    match (operation, cleanup) {
        (Ok(evidence), Ok(())) => Ok(evidence),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(primary), Err(cleanup)) => Err(anyhow::anyhow!(
            "{primary:#}\ncold-root cleanup failed: {cleanup:#}"
        )),
    }
}

fn assert_cold_jsonl(evidence: &ColdEvidence) {
    assert!(evidence.session_id.starts_with("session-"));
    assert_eq!(
        evidence.cwd.as_deref(),
        Some(evidence.canonical_workspace.as_str())
    );
    assert!(
        ordered_subsequence(
            &evidence.event_types,
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
        "unexpected cold event sequence: {:?}",
        evidence.event_types
    );
    assert!(evidence.has_task);
    assert_eq!(
        evidence.todo_content.as_deref(),
        Some("prove the process boundary")
    );
    assert!(evidence.has_answer);
    assert_eq!(evidence.final_reason.as_deref(), Some("completed"));
    assert!(evidence.has_request_header);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binary_runs_tool_prints_answer_and_cold_reopens_jsonl() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let home = temporary.path().join("home");
    let request_count = Arc::new(AtomicUsize::new(0));
    let (base_url, server) = loopback_server(request_count.clone()).await?;

    let output = run_process(&workspace, &home, &base_url, request_count.as_ref()).await;
    let output = match output {
        Ok(output) => output,
        Err(primary) => {
            return match abort_server(server).await {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(anyhow::anyhow!(
                    "{primary:#}\nloopback-server cleanup failed: {cleanup:#}"
                )),
            };
        }
    };
    let captured = finish_server(server).await?;
    let cold = collect_cold_jsonl(&home, &workspace).await?;

    assert_process_output(&output);
    assert_requests(&captured);
    assert_cold_jsonl(&cold);
    Ok(())
}
