//! Actual-provider headless diagnostics and keep-alive/default request parity.

use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};

use parking_lot::Mutex;
use seekdeep_agent::AGENTS;
use seekdeep_agent_loop::{Config as AgentLoopConfig, PLUGIN_INJECT};
use seekdeep_cordis::{Context, Plugin};
use seekdeep_core::session::SessionEvent;
use seekdeep_llm_deepseek::{DeepSeekConfig, types::ThinkingMode};
use seekdeep_loader::{LoadedComposition, PluginCatalog};
use seekdeep_loader_smoke::{FixtureTurnOptions, FixtureTurnResult, run_fixture_turn};
use seekdeep_session_persistence::SESSION_PERSISTENCE;
use seekdeep_util::launch_environment::{
    LaunchEnvironmentLayerInput, LaunchEnvironmentSource, SEEKDEEP_LAUNCH_ENVIRONMENT,
    create_launch_environment_snapshot,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

const MISSING_FIXTURE: &str = include_str!(
    "../../../examples/headless-agent/tests/snapshots/missing-credential/stream-json.expected.jsonl"
);
const INVALID_FIXTURE: &str = include_str!(
    "../../../examples/headless-agent/tests/snapshots/invalid-credential/stream-json.expected.jsonl"
);

struct ProviderFixture {
    context: Context,
    composition: LoadedComposition,
}

impl ProviderFixture {
    async fn load(root: &Path, config: DeepSeekConfig, key: &str) -> anyhow::Result<Self> {
        let workspace = root.join("workspace");
        let home = root.join("home");
        std::fs::create_dir_all(&workspace)?;
        std::fs::create_dir_all(&home)?;
        let context = Context::new();
        context.provide(
            SEEKDEEP_LAUNCH_ENVIRONMENT,
            Arc::new(create_launch_environment_snapshot(&[
                LaunchEnvironmentLayerInput {
                    source: LaunchEnvironmentSource::Process,
                    path: None,
                    values: BTreeMap::from([
                        (
                            "SEEKDEEP_HOME".to_owned(),
                            home.to_string_lossy().into_owned(),
                        ),
                        ("DEEPSEEK_API_KEY".to_owned(), key.to_owned()),
                    ]),
                },
            ])),
        )?;
        let catalog = provider_catalog()?;
        let source = format!(
            concat!(
                "- {{ id: sessions, name: sessions }}\n",
                "- {{ id: llm, name: llm }}\n",
                "- {{ id: agents, name: agents }}\n",
                "- {{ id: prompt, name: system-prompt, config: {{ persona: 'Keyless provider snapshot.' }} }}\n",
                "- {{ id: tools, name: tools, config: {{ mode: native }} }}\n",
                "- {{ id: persistence, name: persistence, config: {{ root: {persistence}, compression: none }} }}\n",
                "- {{ id: provider, name: llm-deepseek, config: {config} }}\n",
                "- id: loop\n  name: fixture-agent-loop\n  config:\n    agents:\n",
                "      - {{ id: main, sessionId: provider-snapshot, provider: deepseek-official, model: deepseek-v4-flash, cwd: {workspace} }}\n",
            ),
            persistence = serde_json::to_string(&home.join("sessions"))?,
            config = serde_json::to_string(&config)?,
            workspace = serde_json::to_string(&workspace)?,
        );
        let composition = catalog.load_yaml(&context, &source).await?;
        Ok(Self {
            context,
            composition,
        })
    }

    async fn run(&self) -> anyhow::Result<(FixtureTurnResult, Vec<SessionEvent>)> {
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = events.clone();
        let result = run_fixture_turn(
            &self.context,
            FixtureTurnOptions {
                task: "return the deterministic response".to_owned(),
                on_event: Some(Arc::new(move |_, event| {
                    observed.lock().push(event.clone());
                })),
            },
        )
        .await?;
        let agent = self
            .context
            .get(AGENTS)
            .unwrap()
            .get(&result.session_id)
            .unwrap();
        let stored = self
            .context
            .get(SESSION_PERSISTENCE)
            .unwrap()
            .persistence()
            .inspect(&result.session_id, None)
            .await?;
        assert_eq!(stored.events, agent.session().events());
        let events = events.lock().clone();
        Ok((result, events))
    }

    async fn dispose(self) -> anyhow::Result<()> {
        self.composition.dispose().await?;
        self.context.root_fiber().dispose().await
    }
}

fn provider_catalog() -> anyhow::Result<PluginCatalog> {
    let catalog = PluginCatalog::new();
    for (name, plugin) in [
        ("sessions", seekdeep_core::session_store::plugin()),
        ("llm", seekdeep_llm::plugin()),
        ("agents", seekdeep_agent::plugin()),
        ("system-prompt", seekdeep_system_prompt::plugin()),
        ("tools", seekdeep_tools::plugin()),
        ("persistence", seekdeep_session_persistence_jsonl::plugin()),
        ("llm-deepseek", seekdeep_llm_deepseek::plugin()),
    ] {
        catalog.register_named(name, plugin)?;
    }
    catalog.register_named(
        "fixture-agent-loop",
        Plugin::new(
            "fixture-agent-loop",
            PLUGIN_INJECT.iter().copied().chain(["sessionPersistence"]),
            |context, config| {
                Box::pin(async move {
                    seekdeep_agent_loop::apply(
                        &context,
                        serde_json::from_value::<AgentLoopConfig>(config)?,
                    )
                    .await?;
                    Ok(())
                })
            },
        ),
    )?;
    Ok(catalog)
}

fn source_event_data(fixture: &str, event_type: &str) -> Value {
    fixture
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|record| record["event"]["type"] == event_type)
        .unwrap()["event"]["data"]
        .clone()
}

async fn credential_failure(key: &str, expected: &str) -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let fixture = ProviderFixture::load(
        temporary.path(),
        DeepSeekConfig {
            base_url: Some(format!("http://{}", listener.local_addr()?)),
            ..DeepSeekConfig::default()
        },
        key,
    )
    .await?;
    let outcome = fixture.run().await;
    fixture.dispose().await?;
    let (result, events) = outcome?;
    assert_eq!(result.output, "");
    assert!(result.usage.is_none());
    assert_eq!(serde_json::to_value(&result)?["type"], "result");
    let end = events
        .iter()
        .find(|event| event.event_type == "turn/end")
        .unwrap();
    assert_eq!(
        end.data["reason"],
        source_event_data(expected, "turn/end")["reason"]
    );
    let header = events
        .iter()
        .find(|event| event.event_type == "request/header")
        .unwrap();
    let expected_header = source_event_data(expected, "request/header");
    assert_eq!(
        header.data["header"]["config"],
        expected_header["header"]["config"]
    );
    assert_eq!(
        header.data["header"]["adapterDefaults"],
        expected_header["header"]["adapterDefaults"]
    );
    let stream = serde_json::to_string(&events)?;
    assert!(!stream.contains("pasted-from-a-chat-window"));
    assert!(!stream.contains("ByteString"));
    assert!(!stream.contains("as a last resort"));
    assert!(
        tokio::time::timeout(Duration::from_millis(25), listener.accept())
            .await
            .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn missing_credential_is_durable_actionable_and_never_dials_http() -> anyhow::Result<()> {
    credential_failure("", MISSING_FIXTURE).await
}

#[tokio::test]
async fn invalid_credential_is_redacted_before_http_or_transport_errors() -> anyhow::Result<()> {
    credential_failure("sk-😀pasted-from-a-chat-window", INVALID_FIXTURE).await
}

struct ServerTask(JoinHandle<anyhow::Result<Value>>);

impl Drop for ServerTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn read_request(stream: &mut TcpStream) -> anyhow::Result<Value> {
    let mut bytes = Vec::new();
    let boundary = loop {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0, "request closed before its headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(offset) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break offset + 4;
        }
    };
    let header = std::str::from_utf8(&bytes[..boundary])?;
    let length = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .ok_or_else(|| anyhow::anyhow!("request lacks content-length"))?;
    while bytes.len() < boundary + length {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).await?;
        anyhow::ensure!(count > 0, "request closed before its body");
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(serde_json::from_slice(&bytes[boundary..boundary + length])?)
}

async fn defaults_server() -> anyhow::Result<(String, ServerTask)> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let request = read_request(&mut stream).await?;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n",
            )
            .await?;
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(60)).await;
            stream.write_all(b": keep-alive\n\n").await?;
            stream.flush().await?;
        }
        tokio::time::sleep(Duration::from_millis(60)).await;
        stream.write_all(concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"DEFAULTS_OK\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n\n",
            "data: [DONE]\n\n",
        ).as_bytes()).await?;
        Ok(request)
    });
    Ok((format!("http://{address}"), ServerTask(task)))
}

#[tokio::test]
async fn keep_alive_comments_extend_idle_reads_and_adapter_defaults_reach_wire_and_log()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (base_url, mut server) = defaults_server().await?;
    let fixture = ProviderFixture::load(
        temporary.path(),
        DeepSeekConfig {
            base_url: Some(base_url),
            thinking: Some(ThinkingMode::Disabled),
            stream_idle_timeout_ms: Some(150.0),
            ..DeepSeekConfig::default()
        },
        "snapshot-key",
    )
    .await?;
    let outcome = fixture.run().await;
    fixture.dispose().await?;
    let (result, events) = outcome?;
    let request = tokio::time::timeout(Duration::from_secs(5), &mut server.0).await???;
    assert_eq!(result.output, "DEFAULTS_OK");
    assert_eq!(request["max_tokens"], 256_000);
    let header = events
        .iter()
        .find(|event| event.event_type == "request/header")
        .unwrap();
    assert_eq!(
        header.data["header"]["config"],
        json!({
            "provider": "deepseek-official", "model": "deepseek-v4-flash",
            "maxTokens": 256_000, "reasoningEffort": "off"
        })
    );
    assert_eq!(
        header.data["header"]["adapterDefaults"],
        json!({
            "maxTokens": true, "reasoningEffort": true
        })
    );
    assert_eq!(events.last().unwrap().data["reason"]["kind"], "completed");
    Ok(())
}
