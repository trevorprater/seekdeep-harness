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
    task::JoinHandle,
};

const APPLICATION_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
const SERVER_ABORT_TIMEOUT: Duration = Duration::from_secs(2);

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
) -> anyhow::Result<(String, JoinHandle<anyhow::Result<()>>)> {
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

async fn abort_server(mut server: JoinHandle<anyhow::Result<()>>) -> anyhow::Result<()> {
    server.abort();
    match tokio::time::timeout(SERVER_ABORT_TIMEOUT, &mut server).await {
        Ok(Err(error)) if error.is_cancelled() => Ok(()),
        Ok(Err(error)) => Err(error.into()),
        Ok(Ok(result)) => result,
        Err(_) => anyhow::bail!("loopback server did not join after abort"),
    }
}

async fn finish_server(mut server: JoinHandle<anyhow::Result<()>>) -> anyhow::Result<()> {
    match tokio::time::timeout(SERVER_JOIN_TIMEOUT, &mut server).await {
        Ok(joined) => joined?,
        Err(_) => {
            server.abort();
            match tokio::time::timeout(SERVER_ABORT_TIMEOUT, &mut server).await {
                Ok(Err(error)) if error.is_cancelled() => {}
                Ok(Err(error)) => return Err(error.into()),
                Ok(Ok(result)) => result?,
                Err(_) => anyhow::bail!("loopback server did not join after abort"),
            }
            anyhow::bail!("loopback server did not finish within its bound")
        }
    }
}

fn with_cleanup_error(
    primary: anyhow::Error,
    label: &str,
    cleanup: anyhow::Result<()>,
) -> anyhow::Error {
    match cleanup {
        Ok(()) => primary,
        Err(cleanup) => anyhow::anyhow!("{primary:#}\n{label}: {cleanup:#}"),
    }
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
    let application = match tokio::time::timeout(
        APPLICATION_TIMEOUT,
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
    {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!("headless application boot timed out")),
    };
    let application = match application {
        Ok(application) => application,
        Err(error) => {
            return Err(with_cleanup_error(
                error,
                "loopback cleanup failed",
                abort_server(server).await,
            ));
        }
    };

    let run_result = tokio::time::timeout(
        APPLICATION_TIMEOUT,
        application.run("prove the assembled path"),
    )
    .await
    .map_err(|_| anyhow::anyhow!("headless application run timed out"));
    let shutdown_result = tokio::time::timeout(APPLICATION_TIMEOUT, async {
        let (first, second) = tokio::join!(application.shutdown(), application.shutdown());
        first?;
        second?;
        Ok::<(), anyhow::Error>(())
    })
    .await
    .unwrap_or_else(|_| Err(anyhow::anyhow!("headless application shutdown timed out")));
    let server_result = finish_server(server).await;

    let result = run_result?;
    shutdown_result?;
    server_result?;
    let session_id = result
        .session_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("headless result omitted its session id"))?;
    let captured = requests.lock().clone();

    let cold_root = Context::new();
    let cold_sessions = SessionStore::install(&cold_root)?;
    let cold =
        JsonlSessionPersistence::new(cold_sessions, JsonlConfig::new(home.join("sessions")))?;
    let inspection =
        match tokio::time::timeout(APPLICATION_TIMEOUT, cold.inspect(&session_id, None)).await {
            Ok(result) => result,
            Err(_) => Err(anyhow::anyhow!("cold session inspection timed out")),
        };
    let cold_shutdown = cold_root.fiber().dispose().await;
    let inspection = inspection?;
    cold_shutdown?;

    assert_eq!(result.exit_code, 0, "{}", result.stderr);
    assert_eq!(result.stdout, "HEADLESS_APP_ROUND_TRIP\n");
    assert!(result.stderr.is_empty());
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
    Ok(())
}
