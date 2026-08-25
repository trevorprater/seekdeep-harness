//! In-process composition and real binary/Loader ACP acceptance.

use std::sync::Arc;
use std::{
    collections::BTreeMap,
    pin::Pin,
    process::{ExitStatus, Stdio},
    task::{Context as TaskContext, Poll},
};

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_acp::{AcpClient, AcpRuntime, PermissionPolicy};
use seekdeep_acp_demo::{Config, apply_with_runtime};
use seekdeep_cordis::Context;
use seekdeep_llm::{AdapterStream, FinishReason, GenerateOptions, LlmAdapter, StreamChunk};
use serde_json::json;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _, ReadBuf},
    net::{TcpListener, TcpStream},
};

#[derive(Debug)]
struct MockAdapter;

#[async_trait]
impl LlmAdapter for MockAdapter {
    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        AdapterStream::new(stream::iter([
            Ok(StreamChunk::TextDelta {
                index: 0,
                text: "ACP RUST OK".to_owned(),
            }),
            Ok(StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            }),
        ]))
    }
}

fn config(root: &std::path::Path) -> Config {
    serde_json::from_value(json!({
        "provider":"mock",
        "model":"mock",
        "persistenceRoot":root,
        "persistenceCompression":"none",
        "workspaceContext":false,
        "skills":{"enabled":false},
        "toolBash":false,
        "toolJobs":false,
        "goals":false
    }))
    .unwrap()
}

#[derive(Clone, Debug)]
struct CapturedHttpRequest {
    headers: BTreeMap<String, String>,
    body: serde_json::Value,
}

struct DeepSeekFixture {
    url: String,
    requests: Arc<Mutex<Vec<CapturedHttpRequest>>>,
    task: tokio::task::AbortHandle,
}

struct CapturingReader<R> {
    inner: R,
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl<R> CapturingReader<R> {
    fn new(inner: R, bytes: Arc<Mutex<Vec<u8>>>) -> Self {
        Self { inner, bytes }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for CapturingReader<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let previous = buffer.filled().len();
        match Pin::new(&mut this.inner).poll_read(context, buffer) {
            Poll::Ready(Ok(())) => {
                this.bytes
                    .lock()
                    .extend_from_slice(&buffer.filled()[previous..]);
                Poll::Ready(Ok(()))
            }
            outcome => outcome,
        }
    }
}

impl DeepSeekFixture {
    async fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let capture = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let capture = capture.clone();
                tokio::spawn(async move {
                    let _ = respond_deepseek(stream, capture).await;
                });
            }
        })
        .abort_handle();
        Self {
            url: format!("http://{address}"),
            requests,
            task,
        }
    }
}

impl Drop for DeepSeekFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn respond_deepseek(
    mut stream: TcpStream,
    requests: Arc<Mutex<Vec<CapturedHttpRequest>>>,
) -> anyhow::Result<()> {
    let request = read_http_request(&mut stream).await?;
    requests.lock().push(request);
    let body = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"ACP BUILT OK\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

async fn read_http_request(stream: &mut TcpStream) -> anyhow::Result<CapturedHttpRequest> {
    let mut bytes = Vec::new();
    let boundary = loop {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0, "request closed before headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let head = std::str::from_utf8(&bytes[..boundary])?;
    let headers = head
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect::<BTreeMap<_, _>>();
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
    Ok(CapturedHttpRequest {
        headers,
        body: serde_json::from_slice(&bytes[boundary..boundary + length])?,
    })
}

fn find_zstd_log(root: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|name| name.ends_with(".jsonl.zstd"))
            {
                return Some(path);
            }
        }
    }
    None
}

fn provider_first_deepseek_config(url: &str) -> String {
    format!(
        concat!(
            "- id: provider\n",
            "  name: seekdeep-llm-deepseek\n",
            "  config:\n",
            "    baseURL: {}\n",
            "- id: acp\n",
            "  name: seekdeep-acp-demo\n",
            "  config:\n",
            "    provider: deepseek-official\n",
            "    model: deepseek-chat\n",
            "    persona: test agent\n",
            "    workspaceContext: false\n",
            "    skills: {{ enabled: false }}\n",
            "    toolBash: false\n",
            "    toolJobs: false\n",
            "    goals: false\n",
        ),
        serde_json::to_string(url).unwrap(),
    )
}

fn assert_default_zstd_log(log: &std::path::Path, session: &seekdeep_acp::AcpSessionId) {
    let compressed = std::fs::read(log).unwrap();
    assert_eq!(&compressed[..4], &[0x28, 0xb5, 0x2f, 0xfd]);
    let plain = zstd::stream::decode_all(compressed.as_slice()).unwrap();
    let header: serde_json::Value = serde_json::from_slice(
        plain
            .split(|byte| *byte == b'\n')
            .next()
            .expect("session header line"),
    )
    .unwrap();
    assert_eq!(header["type"], "session");
    assert_eq!(header["id"], session.as_str());
}

fn assert_deepseek_request(server: &DeepSeekFixture) {
    let requests = server.requests.lock();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].headers["authorization"], "Bearer fixture-key");
    assert_eq!(requests[0].body["model"], "deepseek-chat");
}

fn assert_json_rpc_stdout(raw_output: &Arc<Mutex<Vec<u8>>>) {
    let raw_output = String::from_utf8(raw_output.lock().clone()).unwrap();
    let frames = raw_output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>();
    assert!(!frames.is_empty());
    for frame in frames {
        serde_json::from_str::<serde_json::Value>(frame)
            .unwrap_or_else(|error| panic!("non-JSON stdout frame {frame:?}: {error}"));
    }
}

async fn run_compiled_binary_expecting_exit(
    config: &std::path::Path,
    cwd: &std::path::Path,
) -> (ExitStatus, String) {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_seekdeep-acp-demo"))
            .arg("--config")
            .arg(config)
            .current_dir(cwd)
            .env("SEEKDEEP_HOME", cwd.join(".seekdeep"))
            .env("SEEKDEEP_AGENTS_HOME", cwd.join(".agents"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .expect("ACP demo did not exit within 30 seconds")
    .unwrap();
    (
        output.status,
        String::from_utf8(output.stderr).expect("ACP diagnostics are UTF-8"),
    )
}

#[tokio::test]
async fn in_process_app_mounts_spine_persistence_query_checkpoint_and_acp() {
    let root = tempfile::tempdir().unwrap();
    let context = Context::new();
    let (server_io, client_io) = tokio::io::duplex(256 * 1024);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);
    let runtime = apply_with_runtime(
        &context,
        {
            let mut config = config(root.path());
            config.goals = None;
            config
        },
        Some(AcpRuntime {
            input: Box::pin(server_read),
            output: Box::pin(server_write),
        }),
    )
    .await
    .unwrap();
    assert!(
        context
            .get(seekdeep_session_persistence::SESSION_PERSISTENCE)
            .is_some()
    );
    assert!(context.get(seekdeep_session_query::SESSION_QUERY).is_some());
    assert!(context.get(seekdeep_acp::ACP_BRIDGE).is_some());
    assert!(runtime.spine.agents.list().is_empty());
    assert!(runtime.spine.tools.get("get_goal", None).is_some());
    runtime
        .spine
        .llm
        .register_adapter(&["mock".to_owned()], Arc::new(MockAdapter))
        .unwrap();
    let transport = seekdeep_sdk_protocol::JsonRpcLineTransport::new(client_read, client_write);
    let client = AcpClient::new(&transport, PermissionPolicy::Reject);
    let updates = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&updates);
    client.on_update(Arc::new(move |update| observed.lock().push(update.clone())));
    client.start();
    client.initialize().await.unwrap();
    let session = client
        .new_session(&root.path().to_string_lossy())
        .await
        .unwrap();
    assert!(
        runtime
            .spine
            .agents
            .get(&seekdeep_core::session::SessionId::new(session.as_str()))
            .is_some()
    );
    assert_eq!(
        client
            .prompt(&session, vec![json!({"type":"text","text":"reply"})])
            .await
            .unwrap(),
        seekdeep_acp::AcpStopReason::EndTurn
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while updates.lock().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        updates.lock()[0].update.pointer("/content/text"),
        Some(&json!("ACP RUST OK"))
    );
    client.shutdown_output().await.unwrap();
    runtime.bridge.connection_closed_signal().cancelled().await;
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn compiled_binary_boots_loader_negotiates_fresh_session_and_exits_on_eof() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join("cordis.yml");
    let yaml = format!(
        concat!(
            "- id: acp\n",
            "  name: seekdeep-acp-demo\n",
            "  config:\n",
            "    provider: mock\n",
            "    model: mock\n",
            "    persistenceRoot: {}\n",
            "    persistenceCompression: none\n",
            "    workspaceContext: false\n",
            "    skills: {{ enabled: false }}\n",
            "    toolBash: false\n",
            "    toolJobs: false\n",
            "    goals: false\n",
        ),
        serde_json::to_string(&root.path().join("sessions").to_string_lossy()).unwrap()
    );
    std::fs::write(&config_path, yaml).unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_seekdeep-acp-demo"))
        .arg("--config")
        .arg(&config_path)
        .current_dir(root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = child.stdout.take().unwrap();
    let output = child.stdin.take().unwrap();
    let client = AcpClient::from_boxed(Box::pin(input), Box::pin(output), PermissionPolicy::Reject);
    client.start();
    if let Err(error) = client.initialize().await {
        let status = child.wait().await.unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let mut bytes = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut bytes)
            .await
            .unwrap();
        panic!(
            "ACP demo initialization failed ({status}): {error:#}\n{}",
            String::from_utf8_lossy(&bytes)
        );
    }
    let session = client
        .new_session(&root.path().to_string_lossy())
        .await
        .unwrap();
    assert!(!session.as_str().is_empty());
    client.shutdown_output().await.unwrap();
    let status = tokio::time::timeout(std::time::Duration::from_secs(30), child.wait())
        .await
        .unwrap()
        .unwrap();
    if !status.success() {
        let mut stderr = child.stderr.take().unwrap();
        let mut bytes = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut bytes)
            .await
            .unwrap();
        panic!("ACP demo failed: {}", String::from_utf8_lossy(&bytes));
    }
}

#[tokio::test]
async fn compiled_binary_completes_provider_turn_and_writes_default_zstd_log() {
    let root = tempfile::tempdir().unwrap();
    let server = DeepSeekFixture::start().await;
    let config_path = root.path().join("cordis.yml");
    std::fs::write(&config_path, provider_first_deepseek_config(&server.url)).unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_seekdeep-acp-demo"))
        .arg("--config")
        .arg(&config_path)
        .current_dir(root.path())
        .env("DEEPSEEK_API_KEY", "fixture-key")
        .env("SEEKDEEP_HOME", root.path().join(".seekdeep"))
        .env("SEEKDEEP_AGENTS_HOME", root.path().join(".agents"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let raw_output = Arc::new(Mutex::new(Vec::new()));
    let input = CapturingReader::new(child.stdout.take().unwrap(), raw_output.clone());
    let output = child.stdin.take().unwrap();
    let client = AcpClient::from_boxed(Box::pin(input), Box::pin(output), PermissionPolicy::Reject);
    let updates = Arc::new(Mutex::new(Vec::new()));
    let observed = updates.clone();
    client.on_update(Arc::new(move |update| observed.lock().push(update.clone())));
    client.start();
    client.initialize().await.unwrap();
    let session = match client.new_session(&root.path().to_string_lossy()).await {
        Ok(session) => session,
        Err(error) => {
            let _ = child.start_kill();
            let status = child.wait().await.unwrap();
            let mut stderr = child.stderr.take().unwrap();
            let mut bytes = Vec::new();
            stderr.read_to_end(&mut bytes).await.unwrap();
            panic!(
                "ACP provider-backed session failed ({status}): {error:#}\n{}",
                String::from_utf8_lossy(&bytes)
            );
        }
    };
    assert_eq!(
        client
            .prompt(&session, vec![json!({"type":"text","text":"reply"})])
            .await
            .unwrap(),
        seekdeep_acp::AcpStopReason::EndTurn
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while updates.lock().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        updates.lock()[0].update.pointer("/content/text"),
        Some(&json!("ACP BUILT OK"))
    );
    let log = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(path) = find_zstd_log(&root.path().join(".sessions")) {
                return path;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_default_zstd_log(&log, &session);
    assert_deepseek_request(&server);

    client.shutdown_output().await.unwrap();
    let status = tokio::time::timeout(std::time::Duration::from_secs(30), child.wait())
        .await
        .unwrap()
        .unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut diagnostics = Vec::new();
    stderr.read_to_end(&mut diagnostics).await.unwrap();
    assert!(
        status.success(),
        "ACP demo failed: {}",
        String::from_utf8_lossy(&diagnostics)
    );
    assert!(!String::from_utf8_lossy(&diagnostics).contains("without inject"));
    assert_json_rpc_stdout(&raw_output);
}

#[tokio::test]
async fn compiled_binary_fails_loud_when_config_directory_does_not_exist() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("missing").join("cordis.yml");
    let (status, stderr) = run_compiled_binary_expecting_exit(&missing, root.path()).await;
    assert!(!status.success());
    assert!(stderr.contains("config file not found"), "{stderr}");
}

#[tokio::test]
async fn compiled_binary_fails_loud_when_config_file_is_missing() {
    let root = tempfile::tempdir().unwrap();
    let missing = root.path().join("does-not-exist.yml");
    let (status, stderr) = run_compiled_binary_expecting_exit(&missing, root.path()).await;
    assert!(!status.success());
    assert!(stderr.contains("config file not found"), "{stderr}");
}

#[test]
fn config_requires_provider_model_and_workspace_policy() {
    assert!(serde_json::from_value::<Config>(json!({})).is_err());
    assert!(
        serde_json::from_value::<Config>(json!({
            "provider":"p","model":"m"
        }))
        .is_err()
    );
}
