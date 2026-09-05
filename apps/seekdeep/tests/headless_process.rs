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
    sync::oneshot,
    task::JoinHandle,
};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(20);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const TASK: &str = "prove the executable path";
const ANSWER: &str = "HEADLESS_PROCESS_ROUND_TRIP";
const BASH_MARKER: &str = "CLI_TOOL_ROUND_TRIP";

#[derive(Clone, Copy)]
enum ProviderScenario {
    Todo,
    Bash,
    Failure,
}

type PipeReader = JoinHandle<std::io::Result<Vec<u8>>>;

#[derive(Debug)]
struct CapturedRequest {
    path: String,
    headers: BTreeMap<String, String>,
    body: Value,
}

struct MockServer {
    task: JoinHandle<anyhow::Result<Vec<CapturedRequest>>>,
    stop: Option<oneshot::Sender<()>>,
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
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

fn text_events(text: &str) -> Vec<Value> {
    vec![
        serde_json::json!({"choices":[{"delta":{"content":text}}]}),
        serde_json::json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
        Value::String("[DONE]".to_owned()),
    ]
}

fn tool_events(scenario: ProviderScenario) -> Vec<Value> {
    let (tool, arguments) = match scenario {
        ProviderScenario::Todo => (
            "todo_write",
            serde_json::json!({
                "todos":[{"content":"prove the process boundary","status":"completed"}]
            }),
        ),
        ProviderScenario::Bash => (
            "bash",
            serde_json::json!({
                "command":"printf CLI_TOOL_ROUND_TRIP",
                "description":"Prove the CLI tool round trip."
            }),
        ),
        ProviderScenario::Failure => unreachable!("failure has no tool round trip"),
    };
    vec![
        serde_json::json!({"choices":[{"delta":{"tool_calls":[{
            "index":0,"id":"headless-process-tool","type":"function",
            "function":{"name":tool,"arguments":arguments.to_string()}
        }]}}]}),
        serde_json::json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]}),
        Value::String("[DONE]".to_owned()),
    ]
}

fn is_title_request(request: &CapturedRequest) -> bool {
    request.body["max_tokens"] == 64
        && request.body.get("tools").is_none()
        && request.body["messages"][0]["content"]
            .as_str()
            .is_some_and(|text| {
                text.starts_with("Create a concise title for an AI coding-assistant session")
            })
}

async fn write_failure(stream: &mut TcpStream) -> anyhow::Result<()> {
    let body = r#"{"error":{"message":"CLI mock provider failed","code":"SERVER"}}"#;
    let response = format!(
        "HTTP/1.1 500 Internal Server Error\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

async fn loopback_server(
    request_count: Arc<AtomicUsize>,
    scenario: ProviderScenario,
) -> anyhow::Result<(String, MockServer)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let (stop, mut stopping) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut captured = Vec::new();
        loop {
            let mut stream = tokio::select! {
                _ = &mut stopping => break,
                accepted = listener.accept() => accepted?.0,
            };
            let request = read_request(&mut stream).await?;
            // The shipped title provider shares the endpoint with conversation turns.
            if is_title_request(&request) {
                write_sse(&mut stream, &text_events("Headless process proof")).await?;
                continue;
            }
            let request_index = captured.len();
            captured.push(request);
            request_count.fetch_add(1, Ordering::Release);
            match (scenario, request_index) {
                (ProviderScenario::Failure, 0) => write_failure(&mut stream).await?,
                (ProviderScenario::Todo | ProviderScenario::Bash, 0) => {
                    write_sse(&mut stream, &tool_events(scenario)).await?;
                }
                (ProviderScenario::Todo | ProviderScenario::Bash, 1) => {
                    write_sse(&mut stream, &text_events(ANSWER)).await?;
                }
                _ => anyhow::bail!("unexpected conversation request {}", request_index + 1),
            }
        }
        Ok(captured)
    });
    Ok((
        format!("http://{address}"),
        MockServer {
            task,
            stop: Some(stop),
        },
    ))
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
    mut server: MockServer,
    output: &std::process::Output,
) -> anyhow::Result<Vec<CapturedRequest>> {
    if let Some(stop) = server.stop.take() {
        let _ = stop.send(());
    }
    if let Ok(joined) = tokio::time::timeout(IO_TIMEOUT, &mut server.task).await {
        joined?
    } else {
        server.task.abort();
        let cleanup = tokio::time::timeout(IO_TIMEOUT, &mut server.task).await;
        anyhow::bail!(
            "loopback server did not finish; cleanup: {cleanup:#?}; process status: {:?}; stdout: {:?}; stderr: {:?}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    }
}

async fn abort_server(mut server: MockServer) -> anyhow::Result<()> {
    server.task.abort();
    match tokio::time::timeout(IO_TIMEOUT, &mut server.task).await {
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
    assert!(
        captured[0].body.to_string().contains("todo_write"),
        "first provider request: {}",
        captured[0].body
    );
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
    final_error: Option<Value>,
    tool_names: Vec<String>,
    tool_results: Vec<Value>,
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
            final_error: events
                .iter()
                .rev()
                .find(|event| event.event_type == "turn/end")
                .and_then(|event| event.data.pointer("/reason/error"))
                .cloned(),
            tool_names: events
                .iter()
                .filter(|event| event.event_type == "tool/call")
                .map(|event| event.data["name"].as_str().unwrap().to_owned())
                .collect(),
            tool_results: events
                .iter()
                .filter(|event| event.event_type == "tool/result")
                .map(|event| event.data.clone())
                .collect(),
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
    assert!(evidence.final_error.is_none());
    assert!(evidence.has_request_header);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binary_runs_tool_prints_answer_and_cold_reopens_jsonl() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let home = temporary.path().join("home");
    let request_count = Arc::new(AtomicUsize::new(0));
    let (base_url, server) = loopback_server(request_count.clone(), ProviderScenario::Todo).await?;

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
    let captured = finish_server(server, &output).await?;
    let cold = collect_cold_jsonl(&home, &workspace).await?;

    assert_process_output(&output);
    assert_requests(&captured);
    assert_cold_jsonl(&cold);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binary_executes_real_bash_and_records_its_output_before_the_answer() -> anyhow::Result<()>
{
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let home = temporary.path().join("home");
    let requests = Arc::new(AtomicUsize::new(0));
    let (base_url, server) = loopback_server(requests.clone(), ProviderScenario::Bash).await?;
    let output = match run_process(&workspace, &home, &base_url, &requests).await {
        Ok(output) => output,
        Err(primary) => {
            return match abort_server(server).await {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(anyhow::anyhow!(
                    "{primary:#}\nloopback cleanup failed: {cleanup:#}"
                )),
            };
        }
    };
    let captured = finish_server(server, &output).await?;
    let cold = collect_cold_jsonl(&home, &workspace).await?;
    assert_process_output(&output);
    assert_requests(&captured);
    assert_eq!(cold.tool_names, ["bash"]);
    assert_eq!(cold.tool_results.len(), 1);
    let result = &cold.tool_results[0]["message"]["content"][0];
    assert_eq!(result["isError"], false, "Bash tool result: {result}");
    assert!(result["content"].to_string().contains(BASH_MARKER));
    assert!(
        captured[1].body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "tool" && message["content"].to_string().contains(BASH_MARKER)
            })
    );
    assert_eq!(cold.final_reason.as_deref(), Some("completed"));
    assert!(cold.final_error.is_none());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binary_applies_and_preserves_user_profile_patch_without_cli_overlay() -> anyhow::Result<()>
{
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    let home = temporary.path().join("home");
    let profile = home.join("profiles/headless");
    std::fs::create_dir(&workspace)?;
    std::fs::create_dir_all(&profile)?;
    let patch_path = profile.join("cordis.patch.yml");
    let patch = "- id: agent-default-model\n  config:\n    provider: deepseek-official\n    model: deepseek-v4-pro\n";
    std::fs::write(&patch_path, patch)?;
    let requests = Arc::new(AtomicUsize::new(0));
    let (base_url, server) = loopback_server(requests.clone(), ProviderScenario::Todo).await?;
    let output = match run_process(&workspace, &home, &base_url, &requests).await {
        Ok(output) => output,
        Err(primary) => {
            return match abort_server(server).await {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(anyhow::anyhow!(
                    "{primary:#}\nloopback cleanup failed: {cleanup:#}"
                )),
            };
        }
    };
    let captured = finish_server(server, &output).await?;
    assert_process_output(&output);
    assert_eq!(captured.len(), 2);
    assert!(
        captured
            .iter()
            .all(|request| request.body["model"] == "deepseek-v4-pro")
    );
    assert_eq!(std::fs::read_to_string(patch_path)?, patch);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binary_prints_terminal_provider_failure_and_preserves_the_durable_error()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    let home = temporary.path().join("home");
    std::fs::create_dir(&workspace)?;
    std::fs::create_dir(&home)?;
    std::fs::write(
        home.join("settings.yaml"),
        "llm-deepseek:\n  retryPolicy:\n    mode: normal\n    maxRetries: 0\n",
    )?;
    let requests = Arc::new(AtomicUsize::new(0));
    let (base_url, server) = loopback_server(requests.clone(), ProviderScenario::Failure).await?;
    let output = match run_process(&workspace, &home, &base_url, &requests).await {
        Ok(output) => output,
        Err(primary) => {
            return match abort_server(server).await {
                Ok(()) => Err(primary),
                Err(cleanup) => Err(anyhow::anyhow!(
                    "{primary:#}\nloopback cleanup failed: {cleanup:#}"
                )),
            };
        }
    };
    let captured = finish_server(server, &output).await?;
    let cold = collect_cold_jsonl(&home, &workspace).await?;
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"\n");
    assert_eq!(
        output.stderr,
        include_bytes!(
            "../../../examples/headless-agent/tests/snapshots/headless-profile/stderr.expected.txt"
        ),
        "terminal stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(captured.len(), 1);
    assert_eq!(requests.load(Ordering::Acquire), 1);
    assert_eq!(cold.final_reason.as_deref(), Some("error"));
    let error = cold.final_error.expect("persisted terminal error");
    assert_eq!(error["code"], "SERVER");
    assert_eq!(error["message"], "CLI mock provider failed");
    Ok(())
}
