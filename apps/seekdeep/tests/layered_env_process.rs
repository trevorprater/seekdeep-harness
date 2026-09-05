//! Built-binary acceptance of launcher-owned layered environment behavior.

use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fmt::Write as _,
    fs,
    path::Path,
    process::{Output, Stdio},
    time::Duration,
};

use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    process::{Child, Command},
    sync::oneshot,
    task::JoinHandle,
};

const PROCESS_TIMEOUT: Duration = Duration::from_secs(20);
const SERVER_TIMEOUT: Duration = Duration::from_secs(10);
const ANSWER: &str = "LAYERED_ENV_PROCESS_OK";
const HEADLESS_HELP: &str = concat!(
    "Usage: seekdeep --profile headless [options] [task...]\n\n",
    "Answer one task, print the final assistant message, and exit.\n\n",
    "Arguments:\n",
    "  task        the task text; multiple words are joined by spaces\n\n",
    "Options:\n",
    "  -h, --help  show this help\n\n",
    "Examples:\n",
    "  seekdeep --profile headless \"run the tests\"     answer one task and exit\n\n",
);

type PipeReader = JoinHandle<std::io::Result<Vec<u8>>>;

#[derive(Debug)]
struct CapturedRequest {
    path: String,
    headers: BTreeMap<String, String>,
    body: Value,
}

#[derive(Debug)]
struct LoopbackServer {
    base_url: String,
    task: Option<JoinHandle<anyhow::Result<Vec<CapturedRequest>>>>,
    stop: Option<oneshot::Sender<()>>,
}

impl LoopbackServer {
    async fn start() -> anyhow::Result<Self> {
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
                // Session-title requests share the provider endpoint with the task.
                if is_title_request(&request) {
                    write_answer(&mut stream, "Environment process proof").await?;
                    continue;
                }
                captured.push(request);
                write_answer(&mut stream, ANSWER).await?;
            }
            Ok(captured)
        });
        Ok(Self {
            base_url: format!("http://{address}"),
            task: Some(task),
            stop: Some(stop),
        })
    }

    async fn finish(mut self) -> anyhow::Result<Vec<CapturedRequest>> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let mut task = self
            .task
            .take()
            .ok_or_else(|| anyhow::anyhow!("loopback server was already joined"))?;
        if let Ok(result) = tokio::time::timeout(SERVER_TIMEOUT, &mut task).await {
            result?
        } else {
            task.abort();
            let _ = tokio::time::timeout(SERVER_TIMEOUT, task).await;
            anyhow::bail!("loopback server did not finish");
        }
    }
}

impl Drop for LoopbackServer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn read_request(stream: &mut TcpStream) -> anyhow::Result<CapturedRequest> {
    let mut bytes = Vec::new();
    let body_start = loop {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0, "request closed before its headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(boundary) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break boundary + 4;
        }
    };

    let head = std::str::from_utf8(&bytes[..body_start])?;
    let mut lines = head.split("\r\n");
    let path = lines
        .next()
        .and_then(|line| line.split_ascii_whitespace().nth(1))
        .ok_or_else(|| anyhow::anyhow!("request path missing"))?
        .to_owned();
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("malformed request header"))?;
        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    while bytes.len() < body_start + content_length {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0, "request closed before its body");
        bytes.extend_from_slice(&chunk[..count]);
    }

    Ok(CapturedRequest {
        path,
        headers,
        body: serde_json::from_slice(&bytes[body_start..body_start + content_length])?,
    })
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

async fn write_answer(stream: &mut TcpStream, answer: &str) -> anyhow::Result<()> {
    let events = [
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
            "choices": [{"delta": {"content": answer}}]
        }),
        serde_json::json!({
            "choices": [{"delta": {}, "finish_reason": "stop"}]
        }),
    ];
    let mut body = String::new();
    for event in events {
        writeln!(body, "data: {event}\n").expect("writing to a String is infallible");
    }
    body.push_str("data: [DONE]\n\n");
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

async fn run_seekdeep(
    cwd: &Path,
    args: &[&str],
    environment: &[(&str, &OsStr)],
) -> anyhow::Result<Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_seekdeep"));
    command
        .current_dir(cwd)
        .args(args)
        .env_clear()
        .envs(environment.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let stdout = spawn_pipe_reader(child.stdout.take().expect("stdout was configured as piped"));
    let stderr = spawn_pipe_reader(child.stderr.take().expect("stderr was configured as piped"));
    let status = match tokio::time::timeout(PROCESS_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            let cleanup = force_reap(&mut child).await;
            let (stdout, stderr) = finish_pipe_readers(stdout, stderr).await?;
            anyhow::bail!(
                "waiting for seekdeep failed: {error}; cleanup: {cleanup:#?}; stdout: {:?}; stderr: {:?}",
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            );
        }
        Err(_) => {
            let cleanup = force_reap(&mut child).await;
            let (stdout, stderr) = finish_pipe_readers(stdout, stderr).await?;
            anyhow::bail!(
                "seekdeep process timed out after {PROCESS_TIMEOUT:?}; cleanup: {cleanup:#?}; stdout: {:?}; stderr: {:?}",
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            );
        }
    };
    let (stdout, stderr) = finish_pipe_readers(stdout, stderr).await?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn spawn_pipe_reader(mut pipe: impl AsyncRead + Unpin + Send + 'static) -> PipeReader {
    tokio::spawn(async move {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).await?;
        Ok(bytes)
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
    if let Ok(joined) = tokio::time::timeout(SERVER_TIMEOUT, &mut reader).await {
        Ok(joined??)
    } else {
        reader.abort();
        match tokio::time::timeout(SERVER_TIMEOUT, &mut reader).await {
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

async fn force_reap(child: &mut Child) -> anyhow::Result<std::process::ExitStatus> {
    if child.try_wait()?.is_none() {
        child.start_kill()?;
    }
    tokio::time::timeout(SERVER_TIMEOUT, child.wait())
        .await
        .map_err(|_| anyhow::anyhow!("seekdeep child did not reap after kill"))?
        .map_err(Into::into)
}

fn assert_success(output: &Output, expected_stdout: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, expected_stdout.as_bytes());
    assert_eq!(output.stderr, b"");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inherited_base_url_routes_project_over_home_credentials_and_home_fallback()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let home = temporary.path().join("home");
    let project_override = temporary.path().join("project-override");
    let project_fallback = temporary.path().join("project-fallback");
    fs::create_dir(&home)?;
    fs::create_dir(&project_override)?;
    fs::create_dir(&project_fallback)?;
    fs::write(home.join(".env"), "DEEPSEEK_API_KEY=home-layer-key\n")?;
    fs::write(
        project_override.join(".env"),
        "DEEPSEEK_API_KEY=project-layer-key\n",
    )?;
    fs::write(
        project_fallback.join(".env"),
        "AN_UNRELATED_PROJECT_VALUE=present\n",
    )?;

    let server = LoopbackServer::start().await?;
    let environment = [
        ("SEEKDEEP_HOME", home.as_os_str()),
        ("DEEPSEEK_BASE_URL", OsStr::new(&server.base_url)),
        ("SEEKDEEP_TOOLS_MODE", OsStr::new("native")),
    ];
    let project_output = run_seekdeep(
        &project_override,
        &["--profile", "headless", "project", "override"],
        &environment,
    )
    .await;
    let home_output = run_seekdeep(
        &project_fallback,
        &["--profile", "headless", "home", "fallback"],
        &environment,
    )
    .await;
    let captured = server.finish().await;

    let project_output = project_output?;
    let home_output = home_output?;
    let captured = captured?;

    assert_success(&project_output, &format!("{ANSWER}\n"));
    assert_success(&home_output, &format!("{ANSWER}\n"));
    assert_eq!(captured.len(), 2);
    assert!(
        captured
            .iter()
            .all(|request| request.body["tools"].is_array())
    );
    // Both launches reaching this otherwise undiscoverable listener proves the
    // bootstrap-only inherited base URL controlled the network destination.
    assert!(
        captured
            .iter()
            .all(|request| request.path == "/chat/completions")
    );
    assert_eq!(
        captured[0].headers.get("authorization").map(String::as_str),
        Some("Bearer project-layer-key")
    );
    assert_eq!(
        captured[1].headers.get("authorization").map(String::as_str),
        Some("Bearer home-layer-key")
    );
    assert!(captured[0].body.to_string().contains("project override"));
    assert!(captured[1].body.to_string().contains("home fallback"));
    Ok(())
}

#[tokio::test]
async fn bootstrap_only_file_value_has_one_exact_prefixed_diagnostic() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let home = temporary.path().join("home");
    let project = temporary.path().join("project");
    fs::create_dir(&home)?;
    fs::create_dir(&project)?;
    let dotenv = project.join(".env");
    fs::write(
        &dotenv,
        "DEEPSEEK_BASE_URL=http://file-must-not-control-network.invalid\n",
    )?;

    let output = run_seekdeep(
        &project,
        &["--profile", "headless", "--help"],
        &[("SEEKDEEP_HOME", home.as_os_str())],
    )
    .await?;
    let canonical_dotenv = fs::canonicalize(&project)?.join(".env");
    let expected = format!(
        "seekdeep: {} sets \"DEEPSEEK_BASE_URL\", which only the launching environment may set \
         (it decides how this process starts, where its code and instructions load from, or how it \
         reaches the network); export DEEPSEEK_BASE_URL instead of putting it in a .env file\n",
        canonical_dotenv.display()
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"");
    assert_eq!(output.stderr, expected.as_bytes());
    assert!(!expected.starts_with("seekdeep: seekdeep:"));
    Ok(())
}

#[tokio::test]
async fn unreadable_layer_warns_once_and_headless_help_continues() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let home = temporary.path().join("home");
    let project = temporary.path().join("project");
    fs::create_dir(&home)?;
    fs::create_dir(&project)?;
    fs::create_dir(home.join(".env"))?;

    let output = run_seekdeep(
        &project,
        &["--profile", "headless", "--help"],
        &[
            ("SEEKDEEP_HOME", home.as_os_str()),
            ("SEEKDEEP_TOOLS_MODE", OsStr::new("native")),
        ],
    )
    .await?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, HEADLESS_HELP.as_bytes());
    let stderr = std::str::from_utf8(&output.stderr)?;
    assert!(stderr.starts_with("seekdeep: failed to load .env: "));
    assert!(stderr.ends_with('\n'));
    assert_eq!(stderr.lines().count(), 1, "unexpected warning: {stderr:?}");
    Ok(())
}

#[tokio::test]
async fn launcher_help_and_version_bypass_a_hostile_project_layer() -> anyhow::Result<()> {
    let project = tempfile::tempdir()?;
    fs::write(
        project.path().join(".env"),
        "SEEKDEEP_HOME=/file-must-not-redirect-home\n",
    )?;

    let help = run_seekdeep(project.path(), &["--help"], &[]).await?;
    assert_eq!(help.status.code(), Some(0));
    assert!(
        help.stdout
            .starts_with(b"Usage: seekdeep [options] [command]")
    );
    assert!(help.stdout.ends_with(b"\n\n"));
    assert_eq!(help.stderr, b"");

    let version = run_seekdeep(project.path(), &["--version"], &[]).await?;
    assert_success(&version, concat!(env!("CARGO_PKG_VERSION"), "\n"));
    Ok(())
}
