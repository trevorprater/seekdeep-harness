//! Keyless provider-loop smoke through the compiled JSON-RPC application.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};

use parking_lot::Mutex;
use seekdeep_core::session::SessionId;
use seekdeep_sdk_client::{
    DeepSeekHarness, DeepSeekHarnessOptions, HarnessClientOptions, RunOptions,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
};

const CONFIG: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../examples/jsonrpc-agent/cordis.yml"
);

struct ModelFixture {
    url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl ModelFixture {
    async fn start() -> Self {
        Self::with_response("done", "length").await
    }

    async fn with_response(text: &str, finish_reason: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let text = text.to_owned();
        let finish_reason = finish_reason.to_owned();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await.unwrap();
            captured.lock().push(request);
            let body = format!(
                concat!(
                    "data: {{\"choices\":[{{\"delta\":{{\"role\":\"assistant\",\"content\":null}}}}]}}\n\n",
                    "data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\n",
                    "data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":{}}}],\"usage\":{{\"prompt_tokens\":3,\"completion_tokens\":1}}}}\n\n",
                    "data: [DONE]\n\n",
                ),
                serde_json::to_string(&text).unwrap(),
                serde_json::to_string(&finish_reason).unwrap(),
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        });
        Self {
            url: format!("http://{address}"),
            requests,
            task,
        }
    }
}

async fn read_request(stream: &mut TcpStream) -> anyhow::Result<Value> {
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
    let length = head
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    while bytes.len() < boundary + length {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0, "request closed before body");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(serde_json::from_slice(&bytes[boundary..boundary + length])?)
}

fn find_zstd_log(root: &Path) -> Option<PathBuf> {
    let mut pending = vec![root.to_owned()];
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
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".jsonl.zstd"))
            {
                return Some(path);
            }
        }
    }
    None
}

#[tokio::test]
async fn max_token_mapping_variants_preserve_reason_tools_request_and_zstd_log() {
    for value in [None, Some("true"), Some("false")] {
        let temporary = tempfile::tempdir().unwrap();
        let cwd = std::fs::canonicalize(temporary.path()).unwrap();
        let server = ModelFixture::start().await;
        let mut environment = std::env::vars().collect::<BTreeMap<_, _>>();
        environment.insert("SEEKDEEP_CORDIS_CONFIG".to_owned(), CONFIG.to_owned());
        environment.insert(
            "DEEPSEEK_API_KEY".to_owned(),
            "keyless-smoke-no-call".to_owned(),
        );
        environment.insert("DEEPSEEK_BASE_URL".to_owned(), server.url.clone());
        environment.insert(
            "SEEKDEEP_CWD".to_owned(),
            cwd.to_string_lossy().into_owned(),
        );
        environment.insert(
            "SEEKDEEP_SESSION_ROOT".to_owned(),
            cwd.join(".sessions").to_string_lossy().into_owned(),
        );
        environment.insert(
            "SEEKDEEP_HOME".to_owned(),
            cwd.join(".seekdeep").to_string_lossy().into_owned(),
        );
        environment.insert(
            "SEEKDEEP_AGENTS_HOME".to_owned(),
            cwd.join(".agents").to_string_lossy().into_owned(),
        );
        if let Some(value) = value {
            environment.insert(
                "SEEKDEEP_MAX_TOKENS_AS_SUCCESS".to_owned(),
                value.to_owned(),
            );
        }
        let mut launch = HarnessClientOptions::new(env!("CARGO_BIN_EXE_seekdeep-jsonrpc-agent"));
        launch.cwd = Some(cwd.to_string_lossy().into_owned());
        launch.env = Some(environment);
        launch.request_timeout_ms = Some(30_000.0);
        let harness = DeepSeekHarness::new(DeepSeekHarnessOptions {
            launch,
            cwd: Some(cwd.to_string_lossy().into_owned()),
            provider: Some("deepseek-official".to_owned()),
            model: Some("deepseek-v4-pro".to_owned()),
            max_tokens: Some(1_234),
        })
        .unwrap();
        let result = harness
            .run(
                "inspect tools",
                RunOptions {
                    session_id: Some(SessionId::new("main")),
                    on_notification: None,
                },
            )
            .await
            .unwrap();
        assert!(result.events.iter().any(|event| {
            event.event_type == "turn/end"
                && event.data.pointer("/reason/kind") == Some(&json!("max-tokens"))
        }));
        harness.close().await.unwrap();
        server.task.await.unwrap();
        let requests = server.requests.lock();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["max_tokens"], 1_234);
        let mut tools = requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        tools.sort_unstable();
        assert_eq!(
            tools,
            ["bash", "edit", "read", "subagent", "todo_write", "write"]
        );
        let log = find_zstd_log(&cwd.join(".sessions")).expect("zstd session log");
        let compressed = std::fs::read(log).unwrap();
        assert_eq!(&compressed[..4], &[0x28, 0xb5, 0x2f, 0xfd]);
        let plain = zstd::stream::decode_all(compressed.as_slice()).unwrap();
        let header: Value =
            serde_json::from_slice(plain.split(|byte| *byte == b'\n').next().unwrap()).unwrap();
        assert_eq!(header["type"], "session");
        assert_eq!(header["id"], "main");
    }
}

#[tokio::test]
async fn invalid_max_token_mapping_fails_before_stdout_protocol_activation() {
    let temporary = tempfile::tempdir().unwrap();
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_seekdeep-jsonrpc-agent"))
        .arg(CONFIG)
        .current_dir(temporary.path())
        .env("DEEPSEEK_API_KEY", "keyless-smoke-no-call")
        .env("SEEKDEEP_MAX_TOKENS_AS_SUCCESS", "sometimes")
        .env("SEEKDEEP_HOME", temporary.path().join(".seekdeep"))
        .env("SEEKDEEP_AGENTS_HOME", temporary.path().join(".agents"))
        .stdin(Stdio::null())
        .output()
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("plugin tree failed to load"), "{stderr}");
    assert!(stderr.contains("sdk-jsonrpc-server"), "{stderr}");
    assert!(stderr.contains("sometimes"), "{stderr}");
}

#[tokio::test]
async fn rust_minimal_client_preserves_cli_defaults_lifecycle_and_final_stdout() {
    let temporary = tempfile::tempdir().unwrap();
    let cwd = std::fs::canonicalize(temporary.path()).unwrap();
    let server = ModelFixture::with_response("MINIMAL CLIENT OK", "stop").await;
    let sessions = cwd.join("minimal-sessions");
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_seekdeep-jsonrpc-minimal"));
    command
        .arg("Reply with the fixture response")
        .arg("--workspace")
        .arg(&cwd)
        .arg("--session-root")
        .arg(&sessions)
        .arg("--session-id")
        .arg("minimal-cli-session")
        .env("DEEPSEEK_API_KEY", "minimal-keyless")
        .env("DEEPSEEK_BASE_URL", &server.url)
        .env("SEEKDEEP_HOME", cwd.join(".seekdeep"))
        .env("SEEKDEEP_AGENTS_HOME", cwd.join(".agents"))
        .kill_on_drop(true);
    let Ok(output) =
        tokio::time::timeout(std::time::Duration::from_secs(30), command.output()).await
    else {
        server.task.abort();
        panic!("minimal client did not exit within 30 seconds");
    };
    let output = output.unwrap();
    if !server.task.is_finished() {
        server.task.abort();
    }
    let _ = server.task.await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = server.requests.lock();
    let persisted = {
        fn collect(path: &Path, output: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(path) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    collect(&path, output);
                } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
                    output.push(std::fs::read_to_string(path).unwrap());
                }
            }
        }
        let mut output = Vec::new();
        collect(&sessions, &mut output);
        output
    };
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "MINIMAL CLIENT OK\n",
        "stderr={} requests={:#?} logs={:#?}",
        String::from_utf8_lossy(&output.stderr),
        &*requests,
        persisted
    );
    assert_eq!(requests[0]["model"], "deepseek-v4-flash");
    assert!(
        requests[0]["messages"]
            .to_string()
            .contains("Reply with the fixture response")
    );
    let logs = std::fs::read_dir(&sessions).unwrap().collect::<Vec<_>>();
    assert!(!logs.is_empty());
}
