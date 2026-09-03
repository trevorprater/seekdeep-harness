//! Keyless Loader, real Bash, stream result, and compressed persistence parity.

#![cfg(not(windows))]

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_agent::AGENTS;
use seekdeep_agent_loop::{Config as AgentLoopConfig, PLUGIN_INJECT};
use seekdeep_cordis::{Context, Plugin};
use seekdeep_llm::{
    AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LLM, LlmAdapter,
    StreamChunk, TokenUsage,
};
use seekdeep_loader::PluginCatalog;
use seekdeep_loader_smoke::{FixtureTurnOptions, run_fixture_turn};
use seekdeep_session_persistence::SESSION_PERSISTENCE;
use serde_json::{Value, json};

#[derive(Debug)]
struct KeylessAdapter {
    requests: Arc<Mutex<Vec<GenerateOptions>>>,
}

#[async_trait]
impl LlmAdapter for KeylessAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        let tool_text = options.messages.last().and_then(|message| {
            message.content().iter().find_map(|block| match block {
                ContentBlock::ToolResult { content, .. } => Some(
                    content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<String>(),
                ),
                _ => None,
            })
        });
        self.requests.lock().push(options);
        let chunks = match tool_text {
            None => {
                let arguments = json!({
                    "command": "printf CLI_TOOL_ROUND_TRIP",
                    "description": "Prove the CLI tool round trip."
                })
                .to_string();
                vec![
                    StreamChunk::BlockStart {
                        index: 0,
                        block_type: "tool-call".to_owned(),
                    },
                    StreamChunk::ToolCallDelta {
                        index: 0,
                        id: CallId::new("cli-smoke-call"),
                        name: Some("bash".to_owned()),
                        arguments_delta: arguments.clone(),
                    },
                    StreamChunk::BlockEnd {
                        index: 0,
                        block: ContentBlock::ToolCall {
                            id: CallId::new("cli-smoke-call"),
                            name: "bash".to_owned(),
                            arguments,
                        },
                    },
                    StreamChunk::Usage {
                        usage: TokenUsage {
                            input_tokens: 11,
                            output_tokens: 3,
                            cache_read_tokens: Some(2),
                            cache_write_tokens: None,
                            reasoning_tokens: None,
                        },
                    },
                    StreamChunk::Finish {
                        reason: FinishReason::ToolCalls,
                        replay_state: None,
                    },
                ]
            }
            Some(tool_text) => {
                let reply = format!("CLI tool round trip complete: {}", tool_text.trim());
                vec![
                    StreamChunk::BlockStart {
                        index: 0,
                        block_type: "text".to_owned(),
                    },
                    StreamChunk::TextDelta {
                        index: 0,
                        text: reply.clone(),
                    },
                    StreamChunk::BlockEnd {
                        index: 0,
                        block: ContentBlock::Text { text: reply },
                    },
                    StreamChunk::Usage {
                        usage: TokenUsage {
                            input_tokens: 7,
                            output_tokens: 5,
                            cache_read_tokens: None,
                            cache_write_tokens: None,
                            reasoning_tokens: Some(1),
                        },
                    },
                    StreamChunk::Finish {
                        reason: FinishReason::Stop,
                        replay_state: None,
                    },
                ]
            }
        };
        AdapterStream::new(stream::iter(chunks.into_iter().map(Ok)))
    }
}

fn catalog(requests: Arc<Mutex<Vec<GenerateOptions>>>) -> anyhow::Result<PluginCatalog> {
    let catalog = PluginCatalog::new();
    for (name, plugin) in [
        ("sessions", seekdeep_core::session_store::plugin()),
        ("llm", seekdeep_llm::plugin()),
        ("agents", seekdeep_agent::plugin()),
        ("system-prompt", seekdeep_system_prompt::plugin()),
        ("tools", seekdeep_tools::plugin()),
        ("persistence", seekdeep_session_persistence_jsonl::plugin()),
        ("subprocess-local", seekdeep_subprocess_local::plugin()),
        ("shell-env", seekdeep_shell_env::plugin()),
        ("bash-local", seekdeep_bash_local::plugin()),
        ("tool-bash", seekdeep_tool_bash::plugin()),
    ] {
        catalog.register_named(name, plugin)?;
    }
    catalog.register_named(
        "cli-mock-llm",
        Plugin::new("cli-mock-llm", ["llm"], move |context, _| {
            let requests = requests.clone();
            Box::pin(async move {
                let llm = context
                    .get(LLM)
                    .ok_or_else(|| anyhow::anyhow!("cli-mock-llm requires llm"))?;
                llm.register_adapter(
                    &["cli-mock".to_owned()],
                    Arc::new(KeylessAdapter { requests }),
                )?;
                Ok(())
            })
        }),
    )?;
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

fn config(workspace: &Path, persistence: &Path) -> String {
    let workspace = serde_json::to_string(&workspace.to_string_lossy()).unwrap();
    let persistence = serde_json::to_string(&persistence.to_string_lossy()).unwrap();
    format!(
        concat!(
            "- {{ id: sessions, name: sessions }}\n",
            "- {{ id: llm, name: llm }}\n",
            "- {{ id: cli-mock, name: cli-mock-llm }}\n",
            "- {{ id: agents, name: agents }}\n",
            "- {{ id: prompt, name: system-prompt, config: {{ persona: 'Keyless headless-agent smoke.' }} }}\n",
            "- {{ id: tools, name: tools, config: {{ mode: native }} }}\n",
            "- {{ id: persistence, name: persistence, config: {{ root: {persistence} }} }}\n",
            "- {{ id: subprocess, name: subprocess-local }}\n",
            "- {{ id: shell-env, name: shell-env }}\n",
            "- {{ id: bash, name: bash-local, config: {{ cwd: {workspace}, timeoutMs: 30000 }} }}\n",
            "- {{ id: tool-bash, name: tool-bash, config: {{ enableRunInBackground: false }} }}\n",
            "- id: agent-loop\n",
            "  name: fixture-agent-loop\n",
            "  config:\n",
            "    agents:\n",
            "      - {{ id: main, sessionId: keyless-headless-smoke, provider: cli-mock, model: cli-mock, cwd: {workspace} }}\n",
        ),
        persistence = persistence,
        workspace = workspace,
    )
}

#[tokio::test]
async fn loader_bash_turn_streams_exact_usage_and_flushes_a_zstd_session() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let persistence_root = temporary.path().join("sessions");
    let config_path = temporary.path().join("cordis.yml");
    std::fs::write(&config_path, config(&workspace, &persistence_root))?;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let context = Context::new();
    let composition = catalog(requests.clone())?
        .load_file(&context, &config_path)
        .await?;
    let observed = Arc::new(Mutex::new(Vec::new()));
    let sink = observed.clone();
    let result = run_fixture_turn(
        &context,
        FixtureTurnOptions {
            task: "prove the tool path".to_owned(),
            on_event: Some(Arc::new(move |session_id, event| {
                sink.lock().push(json!({
                    "type": "session_event",
                    "sessionId": session_id,
                    "event": event
                }));
            })),
        },
    )
    .await?;
    assert!(result.output.contains("CLI_TOOL_ROUND_TRIP"));
    assert_eq!(
        result.usage,
        Some(TokenUsage {
            input_tokens: 18,
            output_tokens: 8,
            cache_read_tokens: Some(2),
            cache_write_tokens: None,
            reasoning_tokens: Some(1),
        })
    );
    {
        let stream = observed.lock();
        assert!(stream.iter().any(|record| {
            record["event"]["type"] == "tool/call" && record["event"]["data"]["name"] == "bash"
        }));
        assert!(stream.iter().any(|record| {
            record["event"]["type"] == "tool/result"
                && record.to_string().contains("CLI_TOOL_ROUND_TRIP")
        }));
        assert!(
            stream
                .iter()
                .all(|record| record["sessionId"] == result.session_id.as_str())
        );
    }
    assert_eq!(requests.lock().len(), 2);

    let agents = context.get(AGENTS).expect("Loader publishes agents");
    let agent = agents
        .get(&result.session_id)
        .expect("configured root agent");
    let persistence = context
        .get(SESSION_PERSISTENCE)
        .expect("Loader publishes persistence")
        .persistence();
    let location = persistence
        .locate(agent.session().header())
        .expect("JSONL location");
    let compressed = std::fs::read(location.path)?;
    assert_eq!(compressed.get(..4), Some(&[0x28, 0xb5, 0x2f, 0xfd][..]));
    let raw = persistence
        .read_raw(&result.session_id, None)
        .await?
        .expect("flushed raw session");
    let header: Value = serde_json::from_str(raw.content.lines().next().unwrap())?;
    assert_eq!(header["type"], "session");
    assert_eq!(header["id"], result.session_id.as_str());

    composition.dispose().await?;
    context.root_fiber().dispose().await
}
