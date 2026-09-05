//! Credential-free one-shot Agent, tool, persistence, and teardown acceptance.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_agent::{
    AgentFactory, AgentHandle, AgentRegistry, CreateAgentOptions, ModelSelection,
    ResumeAgentOptions,
};
use seekdeep_agent_loop::{
    AgentLoop, AgentLoopServices, DEFAULT_MAX_PARALLEL_TOOL_CALLS, install_request_invariant,
};
use seekdeep_cordis::Context;
use seekdeep_core::{session::SessionEvent, session_store::SessionStore};
use seekdeep_headless::HeadlessRunner;
use seekdeep_llm::{
    AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, LlmRuntime,
    ModelId, ProviderId, StreamChunk,
};
use seekdeep_session_persistence::{SESSION_PERSISTENCE, SessionPersistence};
use seekdeep_session_persistence_jsonl::{JsonlConfig, JsonlSessionPersistence, scan_zstd_frames};
use seekdeep_system_prompt::{SystemPromptConfig, install as install_system_prompt};
use seekdeep_tool_todo::{Config as TodoConfig, apply as install_todo};
use seekdeep_tools::{ToolRuntimeConfig, install as install_tools};
use serde_json::Value;

#[derive(Debug)]
struct ToolThenAnswerAdapter {
    called: AtomicBool,
    requests: Arc<Mutex<Vec<GenerateOptions>>>,
}

#[derive(Debug)]
struct FailingFactory;

#[async_trait]
impl AgentFactory for FailingFactory {
    async fn create_agent(
        &self,
        _owner_context: &Context,
        _options: CreateAgentOptions,
    ) -> anyhow::Result<AgentHandle> {
        anyhow::bail!("factory exploded")
    }

    async fn resume(
        &self,
        _owner_context: &Context,
        _options: ResumeAgentOptions,
    ) -> anyhow::Result<AgentHandle> {
        anyhow::bail!("not used")
    }
}

#[async_trait]
impl LlmAdapter for ToolThenAnswerAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests.lock().push(options);
        let first = !self.called.swap(true, Ordering::AcqRel);
        if first {
            return AdapterStream::new(stream::iter([
                Ok(StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::ToolCall {
                        id: CallId::new("headless-tool-call"),
                        name: "todo_write".to_owned(),
                        arguments: serde_json::json!({
                            "todos": [{
                                "content": "prove the headless path",
                                "status": "completed"
                            }]
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
                text: "HEADLESS_TOOL_ROUND_TRIP".to_owned(),
            }),
            Ok(StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            }),
        ]))
    }
}

fn ordered_subsequence(events: &[String], expected: &[&str]) -> bool {
    let mut cursor = 0;
    for event in events {
        if expected
            .get(cursor)
            .is_some_and(|expected| event == expected)
        {
            cursor += 1;
        }
    }
    cursor == expected.len()
}

fn assert_live_events(events: &[SessionEvent]) -> anyhow::Result<()> {
    let event_types = events
        .iter()
        .map(|event| event.event_type.clone())
        .collect::<Vec<_>>();
    assert!(
        ordered_subsequence(
            &event_types,
            &[
                "agent/inbox/spliced",
                "turn/start",
                "agent/inbox/spliced",
                "step/start",
                "user/message",
                "request/header",
                "request/context",
                "assistant/message",
                "tool/call",
                "todo/write",
                "tool/result",
                "step/end",
                "step/start",
                "assistant/message",
                "step/end",
                "turn/end",
            ],
        ),
        "unexpected event sequence: {event_types:?}"
    );
    let todo = events
        .iter()
        .find(|event| event.event_type == "todo/write")
        .ok_or_else(|| anyhow::anyhow!("todo event missing"))?;
    assert_eq!(
        todo.data
            .pointer("/todos/0/content")
            .and_then(Value::as_str),
        Some("prove the headless path")
    );
    let call = events
        .iter()
        .find(|event| event.event_type == "tool/call")
        .ok_or_else(|| anyhow::anyhow!("tool call missing"))?;
    let result = events
        .iter()
        .find(|event| event.event_type == "tool/result")
        .ok_or_else(|| anyhow::anyhow!("tool result missing"))?;
    assert_eq!(result.source_event_seqs.as_deref(), Some(&[call.seq][..]));
    assert_eq!(
        events
            .last()
            .and_then(|event| event.data.pointer("/reason/kind"))
            .and_then(Value::as_str),
        Some("completed")
    );
    Ok(())
}

fn assert_requests(requests: &[GenerateOptions], workspace: &Path) {
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .tools
            .as_ref()
            .is_some_and(|schemas| schemas.iter().any(|schema| schema.name == "todo_write"))
    );
    assert!(
        requests[1]
            .messages
            .iter()
            .any(|message| message.source().kind == "tool")
    );
    let expected_persona = format!(
        "You are a coding agent powered by the model model. Your working directory is {}.",
        workspace.display()
    );
    assert!(
        requests[0]
            .system
            .as_deref()
            .is_some_and(|system| system.contains(&expected_persona))
    );
}

fn test_paths() -> anyhow::Result<(tempfile::TempDir, std::path::PathBuf, std::path::PathBuf)> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let persistence = temporary.path().join("sessions");
    Ok((temporary, workspace, persistence))
}

#[tokio::test]
async fn real_tool_round_trip_flushes_and_cold_reopens() -> anyhow::Result<()> {
    let (_temporary, workspace, persistence_root) = test_paths()?;

    let root = Context::new();
    let sessions = SessionStore::install(&root)?;
    let persistence_fiber =
        seekdeep_session_persistence_jsonl::install(&root, JsonlConfig::new(&persistence_root))?;
    persistence_fiber.await_settled().await?;
    let persistence_service = root
        .get(SESSION_PERSISTENCE)
        .ok_or_else(|| anyhow::anyhow!("JSONL persistence did not activate"))?;

    let agents = Arc::new(AgentRegistry::new(root.clone()));
    agents.provide(&root)?;
    let llm = LlmRuntime::install(&root)?;
    let requests = Arc::new(Mutex::new(Vec::new()));
    llm.register_adapter(
        &["mock".to_owned()],
        Arc::new(ToolThenAnswerAdapter {
            called: AtomicBool::new(false),
            requests: requests.clone(),
        }),
    )?;
    let prompt = install_system_prompt(
        &root,
        SystemPromptConfig {
            persona: concat!(
                "You are a coding agent powered by the {{model}} model. ",
                "Your working directory is {{cwd}}."
            )
            .to_owned(),
            ..SystemPromptConfig::default()
        },
    )?;
    let tools = install_tools(&root, &prompt, ToolRuntimeConfig::default())?;
    install_todo(
        &root,
        TodoConfig {
            allow_parallel_in_progress: true,
        },
    )?;
    install_request_invariant(&root, &llm, sessions.clone())?;

    let agent_loop = AgentLoop::new(
        root.clone(),
        sessions.clone(),
        (*agents).clone(),
        AgentLoopServices {
            llm,
            system_prompt: prompt.clone(),
            tools,
            max_parallel_tool_calls: DEFAULT_MAX_PARALLEL_TOOL_CALLS,
        },
    )?;
    agent_loop.set_persistence(persistence_service.persistence())?;
    agents.set_factory(Arc::new(agent_loop.clone()))?;

    let runner = HeadlessRunner::new(
        agents.clone(),
        sessions.clone(),
        prompt,
        ModelSelection {
            provider: ProviderId::new("mock"),
            model: ModelId::new("model"),
            reasoning_effort: None,
        },
        workspace.to_string_lossy(),
    )?;
    let result = runner.run("prove the tool path").await;
    assert_eq!(result.exit_code, 0, "{}", result.stderr);
    assert_eq!(result.stdout, "HEADLESS_TOOL_ROUND_TRIP\n");
    assert!(result.stderr.is_empty());
    let session_id = result.session_id.ok_or_else(|| {
        anyhow::anyhow!("successful headless run did not retain its session identity")
    })?;

    let live = sessions
        .get(&session_id)
        .ok_or_else(|| anyhow::anyhow!("headless session is not live"))?;
    let live_events = live.events();
    assert_live_events(&live_events)?;
    {
        let requests = requests.lock();
        assert_requests(&requests, &workspace);
    }

    let location = persistence_service
        .persistence()
        .locate(live.header())
        .ok_or_else(|| anyhow::anyhow!("JSONL location missing"))?;
    assert!(location.path.exists(), "flush did not materialize JSONL");
    let compressed = std::fs::read(&location.path)?;
    assert_eq!(compressed.get(..4), Some(&[0x28, 0xb5, 0x2f, 0xfd][..]));
    assert!(!scan_zstd_frames(&compressed, None)?.frames.is_empty());

    agent_loop.dispose().await?;
    agents.dispose_initiators().await;
    persistence_fiber.dispose().await?;
    root.fiber().dispose().await?;

    let reopened_root = Context::new();
    let reopened_sessions = SessionStore::install(&reopened_root)?;
    let reopened =
        JsonlSessionPersistence::new(reopened_sessions, JsonlConfig::new(&persistence_root))?;
    let inspection = reopened.inspect(&session_id, None).await?;
    assert_eq!(inspection.events, live_events);
    reopened_root.fiber().dispose().await?;
    Ok(())
}

#[tokio::test]
async fn direct_agent_creation_failure_is_rendered_and_leaves_no_session() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let root = Context::new();
    let sessions = SessionStore::install(&root)?;
    let agents = Arc::new(AgentRegistry::new(root.clone()));
    agents.provide(&root)?;
    let factory = agents.set_factory(Arc::new(FailingFactory))?;
    let prompt = install_system_prompt(&root, SystemPromptConfig::default())?;
    let runner = HeadlessRunner::new(
        agents.clone(),
        sessions.clone(),
        prompt,
        ModelSelection {
            provider: ProviderId::new("mock"),
            model: ModelId::new("model"),
            reasoning_effort: None,
        },
        workspace.to_string_lossy(),
    )?;

    assert_eq!(
        runner.run("fail before publication").await,
        seekdeep_headless::HeadlessRunResult {
            session_id: None,
            stdout: String::new(),
            stderr: "seekdeep: factory exploded\n".to_owned(),
            exit_code: 1,
        }
    );
    assert!(sessions.list().is_empty());
    factory.dispose().await?;
    agents.dispose_initiators().await;
    root.fiber().dispose().await?;
    Ok(())
}
