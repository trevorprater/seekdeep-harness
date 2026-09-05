//! Bounded retry through the real Rust `DeepSeek` HTTP/SSE adapter.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use parking_lot::Mutex;
use seekdeep_agent::{AgentOptions, AgentRegistry};
use seekdeep_agent_loop::{AgentLoopServices, LoopAgent};
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionId};
use seekdeep_llm::{ContentBlock, LlmRuntime, MessageSource, UserMessage};
use seekdeep_llm_deepseek::{DeepSeekConfig, install as install_deepseek};
use seekdeep_llm_retry::{RetryConfig, RetryId, RetryInternals, install_with_internals};
use seekdeep_system_prompt::{SystemPrompt, SystemPromptConfig};
use seekdeep_tools::{ToolRuntime, ToolRuntimeConfig};
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSource, SEEKDEEP_LAUNCH_ENVIRONMENT,
    create_launch_environment_snapshot,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Behavior {
    Reset,
    PartialDisconnect,
    PartialEof,
    Empty,
    Stall,
    Success,
}

#[derive(Clone, Debug)]
struct CapturedRequest {
    body: Value,
    behavior: Behavior,
}

struct MockServer {
    url: String,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
    task: tokio::task::AbortHandle,
}

impl MockServer {
    async fn start(sequence: Vec<Behavior>) -> Self {
        Self::start_on(TcpListener::bind(("127.0.0.1", 0)).await.unwrap(), sequence)
    }

    fn start_on(listener: TcpListener, sequence: Vec<Behavior>) -> Self {
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let capture = requests.clone();
        let sequence = Arc::new(Mutex::new(VecDeque::from(sequence)));
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let behavior = sequence.lock().pop_front().unwrap_or(Behavior::Success);
                let capture = capture.clone();
                tokio::spawn(async move {
                    let _ = serve(stream, behavior, capture).await;
                });
            }
        });
        Self {
            url: format!("http://{address}"),
            requests,
            task: task.abort_handle(),
        }
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    mut stream: TcpStream,
    behavior: Behavior,
    requests: Arc<Mutex<Vec<CapturedRequest>>>,
) -> anyhow::Result<()> {
    let body = read_request(&mut stream).await?;
    requests.lock().push(CapturedRequest { body, behavior });
    match behavior {
        Behavior::Reset => Ok(()),
        Behavior::PartialDisconnect => {
            let body = "data: {\"choices\":[{\"delta\":{\"content\":\"discard me\"}}]}\n\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len() + 100
            );
            stream.write_all(response.as_bytes()).await?;
            stream.shutdown().await?;
            Ok(())
        }
        Behavior::PartialEof => write_sse(&mut stream, partial_eof()).await,
        Behavior::Empty => write_sse(&mut stream, empty_completion()).await,
        Behavior::Success => write_sse(&mut stream, success_completion()).await,
        Behavior::Stall => {
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: keep-alive\r\n\r\n",
                )
                .await?;
            stream.flush().await?;
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(())
        }
    }
}

async fn read_request(stream: &mut TcpStream) -> anyhow::Result<Value> {
    let mut bytes = Vec::new();
    let boundary = loop {
        let mut chunk = [0_u8; 4_096];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0, "request closed before headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..boundary]);
    let length = headers
        .lines()
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
        })
        .unwrap_or(0);
    while bytes.len() < boundary + length {
        let mut chunk = [0_u8; 4_096];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0, "request closed before body");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(serde_json::from_slice(&bytes[boundary..boundary + length])?)
}

async fn write_sse(stream: &mut TcpStream, body: String) -> anyhow::Result<()> {
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

fn partial_eof() -> String {
    "data: {\"choices\":[{\"delta\":{\"content\":\"discarded clean eof\"}}]}\n\n".to_owned()
}

fn empty_completion() -> String {
    concat!(
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    )
    .to_owned()
}

fn success_completion() -> String {
    concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"recovered response\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    )
    .to_owned()
}

struct Harness {
    context: Context,
    session: Arc<Session>,
    agent: Arc<seekdeep_agent::Agent>,
    retry: Arc<seekdeep_cordis::PluginFiber>,
    provider: Arc<seekdeep_cordis::PluginFiber>,
    _home: tempfile::TempDir,
}

impl Harness {
    async fn start(base_url: &str, idle_ms: f64, max_retries: u64) -> Self {
        Self::start_with_backoff(base_url, idle_ms, max_retries, 10).await
    }

    async fn start_with_backoff(
        base_url: &str,
        idle_ms: f64,
        max_retries: u64,
        backoff_ms: u64,
    ) -> Self {
        let home = tempfile::tempdir().unwrap();
        let context = Context::new();
        context
            .provide(
                SEEKDEEP_LAUNCH_ENVIRONMENT,
                Arc::new(create_launch_environment_snapshot(&[
                    LaunchEnvironmentLayerInput {
                        source: LaunchEnvironmentSource::Process,
                        path: None,
                        values: BTreeMap::from([
                            (
                                "SEEKDEEP_HOME".to_owned(),
                                home.path().to_string_lossy().into_owned(),
                            ),
                            ("DEEPSEEK_API_KEY".to_owned(), "mock-key".to_owned()),
                        ]),
                    },
                ])),
            )
            .unwrap();
        let agents = Arc::new(AgentRegistry::new(context.clone()));
        agents.provide(&context).unwrap();
        let llm = LlmRuntime::install(&context).unwrap();
        let retry = install_with_internals(
            &context,
            RetryConfig::default(),
            RetryInternals::new(|| 0.5, || RetryId::new("transport-chain")),
        )
        .unwrap();
        retry.await_settled().await.unwrap();
        let provider = install_deepseek(
            &context,
            DeepSeekConfig {
                base_url: Some(base_url.to_owned()),
                stream_idle_timeout_ms: Some(idle_ms),
                retry_policy: Some(json!({
                    "mode":"normal",
                    "maxRetries":max_retries,
                    "backoff":{
                        "initialDelayMs":backoff_ms,
                        "maxDelayMs":backoff_ms,
                        "jitterRatio":0
                    }
                })),
                ..DeepSeekConfig::default()
            },
        )
        .unwrap();
        provider.await_settled().await.unwrap();
        let system_prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
        let tools = ToolRuntime::new_with_system_prompt(
            &context,
            &system_prompt,
            ToolRuntimeConfig::default(),
        )
        .unwrap();
        let session = Session::create(&SessionId::new("transport"), None, None).unwrap();
        let (loop_agent, _driver) = LoopAgent::new_default(
            &context,
            &session,
            AgentOptions {
                provider: Some("deepseek-official".into()),
                model: Some("mock-model".into()),
                max_tokens: None,
                subagent_depth: None,
            },
            None,
            AgentLoopServices {
                llm,
                system_prompt,
                tools,
                max_parallel_tool_calls: 10,
            },
        )
        .unwrap();
        Self {
            context,
            session,
            agent: loop_agent.agent,
            retry,
            provider,
            _home: home,
        }
    }

    async fn run(&self) {
        self.agent.followup(user()).unwrap();
        self.agent.when_idle().unwrap().await.unwrap();
    }

    fn retry_codes(&self) -> Vec<String> {
        self.session
            .events()
            .iter()
            .filter(|event| event.event_type == "llm/retry")
            .filter_map(|event| event.data.pointer("/failure/code").and_then(Value::as_str))
            .map(str::to_owned)
            .collect()
    }

    fn final_text(&self) -> Option<String> {
        let message = self.session.derive_messages().into_iter().last()?;
        Some(
            message
                .content()
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect(),
        )
    }

    async fn close(self) {
        self.provider.dispose().await.unwrap();
        self.retry.dispose().await.unwrap();
        self.context.fiber().dispose().await.unwrap();
    }
}

fn user() -> UserMessage {
    UserMessage::new(
        vec![ContentBlock::Text {
            text: "recover through the provider boundary".to_owned(),
        }],
        MessageSource::user(),
    )
}

#[tokio::test]
async fn refused_connection_recovers_when_endpoint_starts_during_backoff() {
    let reservation = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let harness = Harness::start_with_backoff(&format!("http://{address}"), 1_000.0, 2, 100).await;
    let session = harness.session.clone();
    let recovery = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if session
                    .events()
                    .iter()
                    .any(|event| event.event_type == "llm/retry")
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let listener = TcpListener::bind(address).await.unwrap();
        MockServer::start_on(listener, vec![Behavior::Success])
    });
    harness.run().await;
    let server = recovery.await.unwrap();
    assert_eq!(server.requests.lock().len(), 1);
    assert_eq!(harness.retry_codes(), ["TRANSPORT"]);
    assert_eq!(harness.final_text().as_deref(), Some("recovered response"));
    harness.close().await;
}

#[tokio::test]
async fn reset_partial_disconnect_and_empty_completion_recover_without_failed_messages() {
    for (failed, code) in [
        (Behavior::Reset, "TRANSPORT"),
        (Behavior::PartialDisconnect, "TRANSPORT"),
        (Behavior::Empty, "EMPTY_RESPONSE"),
    ] {
        let server = MockServer::start(vec![failed, Behavior::Success]).await;
        let harness = Harness::start(&server.url, 1_000.0, 2).await;
        harness.run().await;
        assert_eq!(server.requests.lock().len(), 2);
        let bodies = server
            .requests
            .lock()
            .iter()
            .map(|request| request.body.clone())
            .collect::<Vec<_>>();
        assert_eq!(bodies[0], bodies[1]);
        assert_eq!(harness.retry_codes(), [code]);
        assert_eq!(harness.final_text().as_deref(), Some("recovered response"));
        assert_eq!(
            harness
                .session
                .events()
                .iter()
                .filter(|event| event.event_type == "assistant/message")
                .count(),
            1
        );
        harness.close().await;
    }
}

#[tokio::test]
async fn clean_partial_eof_is_stream_closed_and_not_default_retryable() {
    let server = MockServer::start(vec![Behavior::PartialEof, Behavior::Success]).await;
    let harness = Harness::start(&server.url, 1_000.0, 2).await;
    harness.run().await;
    assert_eq!(server.requests.lock().len(), 1);
    assert!(harness.retry_codes().is_empty());
    assert!(
        !harness
            .session
            .events()
            .iter()
            .any(|event| event.event_type == "assistant/message")
    );
    let end = harness.session.events().into_iter().last().unwrap();
    assert_eq!(end.event_type, "turn/end");
    assert_eq!(
        end.data
            .pointer("/reason/error/code")
            .and_then(Value::as_str),
        Some("STREAM_CLOSED")
    );
    harness.close().await;
}

#[tokio::test]
async fn stalled_body_becomes_timeout_then_succeeds() {
    let server = MockServer::start(vec![Behavior::Stall, Behavior::Success]).await;
    let harness = Harness::start(&server.url, 50.0, 2).await;
    harness.run().await;
    assert_eq!(
        server
            .requests
            .lock()
            .iter()
            .map(|request| request.behavior)
            .collect::<Vec<_>>(),
        [Behavior::Stall, Behavior::Success]
    );
    assert_eq!(harness.retry_codes(), ["TIMEOUT"]);
    assert_eq!(harness.final_text().as_deref(), Some("recovered response"));
    harness.close().await;
}

#[tokio::test]
async fn transport_budget_exhaustion_stops_after_initial_plus_two_retries() {
    let server = MockServer::start(vec![Behavior::Reset; 3]).await;
    let harness = Harness::start(&server.url, 1_000.0, 2).await;
    harness.run().await;
    assert_eq!(server.requests.lock().len(), 3);
    assert_eq!(harness.retry_codes(), ["TRANSPORT", "TRANSPORT"]);
    assert_eq!(
        harness
            .session
            .events()
            .iter()
            .filter(|event| event.event_type == "step/start")
            .count(),
        1
    );
    let end = harness.session.events().into_iter().last().unwrap();
    assert_eq!(
        end.data
            .pointer("/reason/error/code")
            .and_then(Value::as_str),
        Some("TRANSPORT")
    );
    harness.close().await;
}
