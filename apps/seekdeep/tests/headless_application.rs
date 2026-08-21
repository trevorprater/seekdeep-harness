//! Real native application assembly against a credential-free loopback
//! DeepSeek-compatible endpoint.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use parking_lot::Mutex;
use seekdeep::{DEFAULT_MODEL, DEFAULT_PROVIDER, HeadlessApplication, HeadlessBootOptions};
use seekdeep_cordis::Context;
use seekdeep_core::session_store::SessionStore;
use seekdeep_llm::{ModelId, ProviderId};
use seekdeep_llm_deepseek::DeepSeekConfig;
use seekdeep_session_persistence::SessionPersistence as _;
use seekdeep_session_persistence_jsonl::{JsonlConfig, JsonlSessionPersistence};
use seekdeep_tools::ToolPresentationMode;
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSource, create_launch_environment_snapshot,
};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
};

#[derive(Clone, Debug)]
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
        if event == "[DONE]" {
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
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
) -> anyhow::Result<(String, tokio::task::JoinHandle<anyhow::Result<()>>)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        let (mut first, _) = listener.accept().await?;
        requests.lock().push(read_request(&mut first).await?);
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
                                "id": "headless-app-tool",
                                "type": "function",
                                "function": {
                                    "name": "todo_write",
                                    "arguments": serde_json::json!({
                                        "todos": [{
                                            "content": "prove the executable assembly",
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
        requests.lock().push(read_request(&mut second).await?);
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
                    "choices": [{"delta": {"content": "HEADLESS_APP_ROUND_TRIP"}}]
                }),
                serde_json::json!({
                    "choices": [{"delta": {}, "finish_reason": "stop"}]
                }),
                Value::String("[DONE]".to_owned()),
            ],
        )
        .await
    });
    Ok((format!("http://{address}"), task))
}

#[tokio::test]
async fn boots_real_provider_runs_tool_persists_and_shuts_down() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let home = temporary.path().join("home");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let (base_url, server) = loopback_server(requests.clone()).await?;
    let environment = create_launch_environment_snapshot(&[LaunchEnvironmentLayerInput {
        source: LaunchEnvironmentSource::Process,
        path: None,
        values: BTreeMap::from([
            (
                "SEEKDEEP_HOME".to_owned(),
                home.to_string_lossy().into_owned(),
            ),
            ("DEEPSEEK_API_KEY".to_owned(), "fake-test-key".to_owned()),
        ]),
    }]);
    let application = tokio::time::timeout(
        Duration::from_secs(10),
        HeadlessApplication::boot(HeadlessBootOptions {
            seekdeep_home: home.clone(),
            cwd: workspace.clone(),
            deepseek: DeepSeekConfig {
                base_url: Some(base_url),
                ..DeepSeekConfig::default()
            },
            launch_environment: environment,
            tools_mode: ToolPresentationMode::Native,
            provider: ProviderId::new(DEFAULT_PROVIDER),
            model: ModelId::new(DEFAULT_MODEL),
        }),
    )
    .await
    .map_err(|_| anyhow::anyhow!("headless application boot timed out"))??;

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        application.run("prove the assembled path"),
    )
    .await
    .map_err(|_| anyhow::anyhow!("headless application run timed out"))?;
    assert_eq!(result.exit_code, 0, "{}", result.stderr);
    assert_eq!(result.stdout, "HEADLESS_APP_ROUND_TRIP\n");
    assert!(result.stderr.is_empty());
    let session_id = result
        .session_id
        .ok_or_else(|| anyhow::anyhow!("headless result omitted its session id"))?;

    server.await??;
    let captured = requests.lock().clone();
    assert_eq!(captured.len(), 2);
    assert!(
        captured
            .iter()
            .all(|request| request.path == "/chat/completions")
    );
    assert!(captured.iter().all(|request| {
        request.headers.get("authorization").map(String::as_str) == Some("Bearer fake-test-key")
    }));
    assert_eq!(captured[0].body["model"], DEFAULT_MODEL);
    assert!(
        captured[0]
            .body
            .to_string()
            .contains("prove the assembled path")
    );
    assert!(captured[0].body.to_string().contains("todo_write"));
    assert!(captured[1].body.to_string().contains("headless-app-tool"));

    tokio::time::timeout(Duration::from_secs(10), application.shutdown())
        .await
        .map_err(|_| anyhow::anyhow!("headless application shutdown timed out"))??;

    let cold_root = Context::new();
    let cold_sessions = SessionStore::install(&cold_root)?;
    let cold =
        JsonlSessionPersistence::new(cold_sessions, JsonlConfig::new(home.join("sessions")))?;
    let inspection = tokio::time::timeout(Duration::from_secs(10), cold.inspect(&session_id, None))
        .await
        .map_err(|_| anyhow::anyhow!("cold session inspection timed out"))??;
    assert!(
        inspection
            .events
            .iter()
            .any(|event| event.event_type == "todo/write")
    );
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
    cold_root.fiber().dispose().await?;
    Ok(())
}
