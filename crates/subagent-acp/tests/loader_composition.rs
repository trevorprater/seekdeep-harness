//! Real parent `AgentLoop` and Loader composition through a fresh ACP child process.

#![cfg(unix)]

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use futures::stream;
use seekdeep_agent::{AgentOptions, CreateAgentOptions};
use seekdeep_agent_loop::{
    AgentLoop, AgentLoopServices, DEFAULT_MAX_PARALLEL_TOOL_CALLS, install_request_invariant,
};
use seekdeep_cordis::Context;
use seekdeep_core::session::SessionId;
use seekdeep_llm::{
    AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, MessageSource,
    ModelId, ProviderId, StreamChunk, UserMessage,
};
use seekdeep_loader::PluginCatalog;
use seekdeep_session_persistence::SESSION_PERSISTENCE;
use seekdeep_session_persistence_jsonl::{JsonlCompression, JsonlConfig};
use serde_json::{Map, json};

const FIXTURE: &str = env!("CARGO_BIN_EXE_seekdeep-acp-server-fixture");

#[derive(Debug)]
struct DelegateAdapter(AtomicBool);

#[async_trait]
impl LlmAdapter for DelegateAdapter {
    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        if !self.0.swap(true, Ordering::AcqRel) {
            return AdapterStream::new(stream::iter([
                Ok(StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::ToolCall {
                        id: CallId::new("delegate-acp"),
                        name: "subagent".to_owned(),
                        arguments: json!({
                            "description":"report child cwd",
                            "prompt":"Report process and session cwd."
                        })
                        .to_string(),
                    },
                }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::ToolCalls,
                    replay_state: None,
                }),
            ]));
        }
        AdapterStream::new(stream::iter([
            Ok(StreamChunk::TextDelta {
                index: 0,
                text: "parent complete".to_owned(),
            }),
            Ok(StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            }),
        ]))
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn loader_inherits_parent_workspace_into_process_and_remote_session() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let workspace = std::fs::canonicalize(workspace).unwrap();
    let context = Context::new();
    let dependencies = seekdeep_agent_loop_testkit::mount_agent_loop_test_dependencies(
        &context,
        seekdeep_agent_loop_testkit::AgentLoopTestDependenciesOptions::default(),
    )
    .unwrap();
    dependencies
        .llm
        .register_adapter(
            &["parent".to_owned()],
            Arc::new(DelegateAdapter(AtomicBool::new(false))),
        )
        .unwrap();
    let persistence = seekdeep_session_persistence_jsonl::install(
        &context,
        JsonlConfig {
            root: workspace.join(".sessions"),
            pack_chunks: false,
            compression: JsonlCompression::None,
            write_batch_max_delay_ms: 1,
            prepared_session_cache_size: 5,
        },
    )
    .unwrap();
    persistence.await_settled().await.unwrap();
    install_request_invariant(
        &context,
        &dependencies.llm,
        Arc::clone(&dependencies.sessions),
    )
    .unwrap();
    let loop_ = AgentLoop::new(
        context.clone(),
        Arc::clone(&dependencies.sessions),
        (*dependencies.agents).clone(),
        AgentLoopServices {
            llm: Arc::clone(&dependencies.llm),
            system_prompt: Arc::clone(&dependencies.system_prompt),
            tools: Arc::clone(&dependencies.tools),
            max_parallel_tool_calls: DEFAULT_MAX_PARALLEL_TOOL_CALLS,
        },
    )
    .unwrap();
    loop_
        .set_persistence(context.get(SESSION_PERSISTENCE).unwrap().persistence())
        .unwrap();
    dependencies
        .agents
        .set_factory(Arc::new(loop_.clone()))
        .unwrap();
    seekdeep_subprocess_local::LocalSubprocessRuntime::install(&context).unwrap();

    let catalog = PluginCatalog::new();
    catalog
        .register_named("subagents", seekdeep_subagent::plugin())
        .unwrap();
    catalog
        .register_named("acp", seekdeep_subagent_acp::plugin())
        .unwrap();
    catalog
        .register_named("tool", seekdeep_tool_subagent::plugin())
        .unwrap();
    let composition = catalog
        .load_yaml(
            &context,
            &format!(
                concat!(
                    "- id: subagents\n",
                    "  name: subagents\n",
                    "- id: acp\n",
                    "  name: acp\n",
                    "  config:\n",
                    "    command: {}\n",
                    "    permission: reject\n",
                    "    env:\n",
                    "      SEEKDEEP_ACP_FIXTURE_MODE: cwd\n",
                    "    disposeEofGraceMs: 1000\n",
                    "    disposeGraceMs: 300\n",
                    "- id: tool\n",
                    "  name: tool\n",
                    "  config:\n",
                    "    provider: acp\n",
                    "    enableRunInBackground: false\n",
                    "    maxDepth: provider-managed\n",
                ),
                serde_json::to_string(FIXTURE).unwrap()
            ),
        )
        .await
        .unwrap();
    let mut options = CreateAgentOptions::new(SessionId::new("acp-loader-parent"));
    options.meta.cwd = Some(workspace.to_string_lossy().into_owned());
    options.agent_options = AgentOptions {
        provider: Some(ProviderId::new("parent")),
        model: Some(ModelId::new("model")),
        max_tokens: None,
        subagent_depth: None,
    };
    let parent = dependencies.agents.create(options).await.unwrap();
    parent
        .agent
        .followup(UserMessage::new(
            vec![ContentBlock::Text {
                text: "Delegate once.".to_owned(),
            }],
            MessageSource {
                kind: "user".to_owned(),
                fields: Map::new(),
            },
        ))
        .unwrap();
    parent.agent.when_idle().unwrap().await.unwrap();
    let expected = format!("{}\n{}", workspace.display(), workspace.display());
    let events = parent.agent.session().events();
    let result = events
        .iter()
        .find(|event| event.event_type == "tool/result")
        .expect("tool result");
    assert_eq!(
        result
            .data
            .pointer("/message/content/0/content/0/text")
            .and_then(|value| value.as_str()),
        Some(expected.as_str())
    );
    parent.dispose().await.unwrap();
    composition.dispose().await.unwrap();
    loop_.dispose().await.unwrap();
    persistence.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
    let logs = walk_jsonl(&workspace.join(".sessions"));
    assert_eq!(logs.len(), 1);
    let log = std::fs::read_to_string(&logs[0]).unwrap();
    assert!(log.contains("\"type\":\"tool/result\""));
    let encoded = serde_json::to_string(&expected).unwrap();
    assert!(log.contains(encoded.trim_matches('"')));
}

fn walk_jsonl(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    fn visit(path: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                visit(&path, files);
            } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    visit(root, &mut files);
    files.sort();
    files
}
