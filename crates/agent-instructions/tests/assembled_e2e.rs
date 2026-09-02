//! Keyless assembled-loop coverage for baseline and dynamic instructions.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream;
use seekdeep_agent::AgentOptions;
use seekdeep_agent_instructions::{Config as InstructionConfig, apply as apply_instructions};
use seekdeep_agent_loop::{AgentLoopServices, DefaultAgentDriver, LoopAgent};
use seekdeep_core::{
    session::SessionId,
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_fs_local::{Config as LocalFsConfig, LocalFileSystem};
use seekdeep_llm::{
    AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, LlmRuntime,
    MessageSource, StreamChunk, UserMessage,
};
use seekdeep_system_prompt::{SystemPromptConfig, install as install_system_prompt};
use seekdeep_tool_fs::{Config as ToolFsConfig, apply as apply_fs_tools};
use seekdeep_tools::{ToolRuntimeConfig, install as install_tools};

const PROBE: &str = "banana-271828";
const NESTED_PROBE: &str = "papaya-314159";
const UPDATED_PROBE: &str = "guava-161803";

#[derive(Debug)]
struct InstructionAwareAdapter;

impl InstructionAwareAdapter {
    fn visible_text(options: &GenerateOptions) -> String {
        options
            .messages
            .iter()
            .flat_map(seekdeep_llm::Message::content)
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.as_str()),
                ContentBlock::ToolResult { content, .. } => content.iter().find_map(|block| {
                    if let ContentBlock::Text { text } = block {
                        Some(text.as_str())
                    } else {
                        None
                    }
                }),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn has_tool_result(options: &GenerateOptions) -> bool {
        options.messages.iter().any(|message| {
            message
                .content()
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
        })
    }

    fn requested_read_path(options: &GenerateOptions) -> Option<&'static str> {
        let prompt = options.messages.iter().rev().find_map(|message| {
            (message.source().kind == "user").then(|| {
                message
                    .content()
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>()
            })
        })?;
        if !prompt.to_ascii_lowercase().contains("read tool") || Self::has_tool_result(options) {
            return None;
        }
        if prompt.contains("pkg/deep/file.txt") {
            Some("pkg/deep/file.txt")
        } else if prompt.contains("trigger.txt") {
            Some("trigger.txt")
        } else {
            None
        }
    }

    fn text_response(text: &str) -> AdapterStream {
        AdapterStream::new(stream::iter(
            vec![
                StreamChunk::BlockStart {
                    index: 0,
                    block_type: "text".to_owned(),
                },
                StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::Text {
                        text: text.to_owned(),
                    },
                },
                StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                },
            ]
            .into_iter()
            .map(Ok),
        ))
    }

    fn read_response(path: &str) -> AdapterStream {
        let call = CallId::new(format!("read-{path}"));
        AdapterStream::new(stream::iter(
            vec![
                StreamChunk::BlockStart {
                    index: 0,
                    block_type: "tool-call".to_owned(),
                },
                StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::ToolCall {
                        id: call,
                        name: "read".to_owned(),
                        arguments: serde_json::json!({"file_path": path}).to_string(),
                    },
                },
                StreamChunk::Finish {
                    reason: FinishReason::ToolCalls,
                    replay_state: None,
                },
            ]
            .into_iter()
            .map(Ok),
        ))
    }
}

#[async_trait]
impl LlmAdapter for InstructionAwareAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        if let Some(path) = Self::requested_read_path(&options) {
            return Self::read_response(path);
        }
        let visible = Self::visible_text(&options);
        let reply = if visible.contains(UPDATED_PROBE) {
            UPDATED_PROBE
        } else if visible.contains(NESTED_PROBE) {
            NESTED_PROBE
        } else if visible.contains(PROBE) {
            PROBE
        } else {
            "missing instruction probe"
        };
        Self::text_response(reply)
    }
}

struct Harness {
    context: seekdeep_cordis::Context,
    agent: LoopAgent,
    _driver: Arc<DefaultAgentDriver>,
    root: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        std::fs::write(
            root.path().join("AGENTS.md"),
            format!(
                "If the user asks for the workspace context handshake, reply with exactly this string and nothing else: {PROBE}.\n"
            ),
        )
        .unwrap();
        let home = root.path().join(".seekdeep");
        std::fs::create_dir(&home).unwrap();
        let context = seekdeep_cordis::Context::new();
        let sessions = SessionStore::install(&context).unwrap();
        let llm = LlmRuntime::install(&context).unwrap();
        llm.register_adapter(
            &["instruction-fixture".to_owned()],
            Arc::new(InstructionAwareAdapter),
        )
        .unwrap();
        let system_prompt = install_system_prompt(
            &context,
            SystemPromptConfig {
                persona: "Answer the user exactly and concisely.".to_owned(),
                ..SystemPromptConfig::default()
            },
        )
        .unwrap();
        let tools = install_tools(&context, &system_prompt, ToolRuntimeConfig::default()).unwrap();
        LocalFileSystem::install(
            &context,
            LocalFsConfig {
                cwd: Some("/".to_owned()),
                ..LocalFsConfig::default()
            },
        )
        .unwrap();
        apply_fs_tools(&context, &ToolFsConfig::default()).unwrap();
        apply_instructions(
            &context,
            &InstructionConfig {
                seekdeep_home: Some(home.to_string_lossy().into_owned()),
                max_bytes: 65_536,
                ..InstructionConfig::default()
            },
        )
        .unwrap();
        let session = sessions
            .create(
                &context,
                Some(SessionId::new("agent-instructions-e2e")),
                CreateSessionOptions {
                    cwd: Some(root.path().to_string_lossy().into_owned()),
                    ..CreateSessionOptions::default()
                },
            )
            .unwrap();
        let (agent, driver) = LoopAgent::new_default(
            &context,
            &session,
            AgentOptions {
                provider: Some("instruction-fixture".into()),
                model: Some("instruction-fixture".into()),
                ..AgentOptions::default()
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
            agent,
            _driver: driver,
            root,
        }
    }

    async fn prompt(&self, text: &str) {
        self.agent
            .agent
            .followup(UserMessage::new(
                vec![ContentBlock::Text {
                    text: text.to_owned(),
                }],
                MessageSource::user(),
            ))
            .unwrap();
        self.agent.agent.when_idle().unwrap().await.unwrap();
    }

    fn final_text(&self) -> String {
        self.agent
            .agent
            .session()
            .events()
            .into_iter()
            .rev()
            .find(|event| event.event_type == "assistant/message")
            .and_then(|event| event.data.get("message").cloned())
            .and_then(|message| serde_json::from_value::<seekdeep_llm::Message>(message).ok())
            .map(|message| {
                message
                    .content()
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>()
            })
            .unwrap_or_default()
    }

    async fn dispose(self) {
        self.context.fiber().dispose().await.unwrap();
    }
}

#[tokio::test]
async fn assembled_loop_injects_workspace_baseline() {
    let harness = Harness::new();
    harness.prompt("Workspace context handshake?").await;
    assert!(harness.final_text().contains(PROBE));
    harness.dispose().await;
}

#[tokio::test]
async fn real_read_tool_discovers_nested_instructions() {
    let harness = Harness::new();
    std::fs::create_dir_all(harness.root.path().join("pkg/deep")).unwrap();
    std::fs::write(
        harness.root.path().join("pkg/AGENTS.md"),
        format!(
            "If the user asks for the nested instruction handshake, reply with exactly this string and nothing else: {NESTED_PROBE}.\n"
        ),
    )
    .unwrap();
    std::fs::write(
        harness.root.path().join("pkg/deep/file.txt"),
        "This file exists only to trigger nested workspace instructions.\n",
    )
    .unwrap();
    harness
        .prompt(
            "Use the read tool to inspect pkg/deep/file.txt. After reading it, answer: nested instruction handshake?",
        )
        .await;
    assert!(harness.final_text().contains(NESTED_PROBE));
    harness.dispose().await;
}

#[tokio::test]
async fn real_file_touch_appends_changed_baseline_without_rewriting_prefix() {
    let harness = Harness::new();
    std::fs::write(
        harness.root.path().join("trigger.txt"),
        "This file triggers workspace instruction reconciliation.\n",
    )
    .unwrap();
    harness.prompt("Workspace context handshake?").await;
    std::fs::write(
        harness.root.path().join("AGENTS.md"),
        format!(
            "The old workspace handshake no longer applies. If the user asks for the updated workspace context handshake, reply with exactly this string and nothing else: {UPDATED_PROBE}.\n"
        ),
    )
    .unwrap();
    harness
        .prompt(
            "You must use the read tool to inspect trigger.txt. After reading it, answer: updated workspace context handshake?",
        )
        .await;
    let update = harness
        .agent
        .agent
        .session()
        .events()
        .into_iter()
        .find(|event| {
            event.event_type == "user/message"
                && event.data["source"]["kind"] == "agent-instructions"
                && event.data["source"]["baseline"] != true
        })
        .unwrap();
    assert_eq!(update.data["source"]["changes"][0]["action"], "replace");
    assert_eq!(update.data["source"]["changes"][0]["path"], "AGENTS.md");
    assert_eq!(
        update.data["source"]["changes"][0]["scope"],
        seekdeep_agent_instructions::candidate_scope_key(".", "AGENTS.md")
    );
    let message: seekdeep_llm::Message = serde_json::from_value(update.data.clone()).unwrap();
    let text = message
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert!(text.contains("Updated instructions from: AGENTS.md"));
    assert!(harness.final_text().contains(UPDATED_PROBE));
    harness.dispose().await;
}
