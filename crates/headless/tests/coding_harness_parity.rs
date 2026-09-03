//! Deterministic model turns over the source headless coding-harness stack.

#![cfg(not(windows))]

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_agent::{AgentOptions, AgentRegistry, ModelSelection, ResumeAgentOptions};
use seekdeep_agent_loop::{
    AgentLoop, AgentLoopServices, DEFAULT_MAX_PARALLEL_TOOL_CALLS, install_request_invariant,
};
use seekdeep_bash_local::Config as BashConfig;
use seekdeep_cordis::Context;
use seekdeep_core::{session::SessionEvent, session_store::SessionStore};
use seekdeep_headless::{HeadlessRunResult, HeadlessRunner};
use seekdeep_llm::{
    AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, LlmRuntime,
    MessageSource, ModelId, ProviderId, StreamChunk, UserMessage,
};
use seekdeep_session_persistence::SESSION_PERSISTENCE;
use seekdeep_session_persistence_jsonl::JsonlConfig;
use seekdeep_shell_env::ShellEnvConfig;
use seekdeep_system_prompt::{SystemPromptConfig, install as install_system_prompt};
use seekdeep_tool_bash::Config as ToolBashConfig;
use seekdeep_tool_todo::Config as TodoConfig;
use seekdeep_tools::{ToolRuntimeConfig, install as install_tools};
use serde_json::{Value, json};

const PERSONA: &str = "You are a coding agent. Use bash for file operations with cat/grep/heredocs; check [exit code: N] markers, and report results briefly.";

#[derive(Debug)]
struct ScriptedAdapter {
    responses: Mutex<VecDeque<Vec<StreamChunk>>>,
    requests: Arc<Mutex<Vec<GenerateOptions>>>,
}

#[async_trait]
impl LlmAdapter for ScriptedAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests.lock().push(options);
        let response = self
            .responses
            .lock()
            .pop_front()
            .expect("scripted headless adapter received an unexpected request");
        AdapterStream::new(stream::iter(response.into_iter().map(Ok)))
    }
}

fn tool_response(call_id: &str, name: &str, arguments: &Value) -> Vec<StreamChunk> {
    vec![
        StreamChunk::BlockEnd {
            index: 0,
            block: ContentBlock::ToolCall {
                id: CallId::new(call_id),
                name: name.to_owned(),
                arguments: arguments.to_string(),
            },
        },
        StreamChunk::Finish {
            reason: FinishReason::ToolCalls,
            replay_state: None,
        },
    ]
}

fn answer_response(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::TextDelta {
            index: 0,
            text: text.to_owned(),
        },
        StreamChunk::Finish {
            reason: FinishReason::Stop,
            replay_state: None,
        },
    ]
}

struct CodingHarness {
    root: Context,
    agents: Arc<AgentRegistry>,
    loop_: AgentLoop,
    runner: HeadlessRunner,
    sessions: Arc<SessionStore>,
    requests: Arc<Mutex<Vec<GenerateOptions>>>,
    persistence_root: PathBuf,
}

impl CodingHarness {
    async fn new(
        workspace: &Path,
        persistence_root: PathBuf,
        responses: Vec<Vec<StreamChunk>>,
    ) -> anyhow::Result<Self> {
        let root = Context::new();
        let sessions = SessionStore::install(&root)?;
        let persistence = seekdeep_session_persistence_jsonl::install(
            &root,
            JsonlConfig::new(&persistence_root),
        )?;
        persistence.await_settled().await?;
        let persistence = root
            .get(SESSION_PERSISTENCE)
            .ok_or_else(|| anyhow::anyhow!("headless coding harness lacks persistence"))?;
        let agents = Arc::new(AgentRegistry::new(root.clone()));
        agents.provide(&root)?;
        let llm = LlmRuntime::install(&root)?;
        let requests = Arc::new(Mutex::new(Vec::new()));
        llm.register_adapter(
            &["mock".to_owned()],
            Arc::new(ScriptedAdapter {
                responses: Mutex::new(VecDeque::from(responses)),
                requests: requests.clone(),
            }),
        )?;
        let prompt = install_system_prompt(
            &root,
            SystemPromptConfig {
                persona: PERSONA.to_owned(),
                ..SystemPromptConfig::default()
            },
        )?;
        let tools = install_tools(&root, &prompt, ToolRuntimeConfig::default())?;
        seekdeep_subprocess_local::LocalSubprocessRuntime::install(&root)?;
        seekdeep_shell_env::apply(&root, &ShellEnvConfig::default())?;
        seekdeep_bash_local::apply(
            &root,
            BashConfig {
                cwd: Some(workspace.to_string_lossy().into_owned()),
                timeout_ms: 30_000.0,
                ..BashConfig::default()
            },
        )
        .await?;
        seekdeep_tool_bash::apply(
            &root,
            ToolBashConfig {
                enable_run_in_background: Some(false),
            },
        )?;
        seekdeep_tool_todo::apply(
            &root,
            TodoConfig {
                allow_parallel_in_progress: true,
            },
        )?;
        install_request_invariant(&root, &llm, sessions.clone())?;
        let loop_ = AgentLoop::new(
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
        loop_.set_persistence(persistence.persistence())?;
        agents.set_factory(Arc::new(loop_.clone()))?;
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
        Ok(Self {
            root,
            agents,
            loop_,
            runner,
            sessions,
            requests,
            persistence_root,
        })
    }

    fn events(&self, result: &HeadlessRunResult) -> Vec<SessionEvent> {
        let id = result
            .session_id
            .as_ref()
            .expect("successful headless run has a session id");
        self.sessions
            .get(id)
            .expect("headless session remains live")
            .events()
    }

    async fn shutdown(self) -> anyhow::Result<()> {
        self.loop_.dispose().await?;
        self.agents.dispose_initiators().await;
        self.root.fiber().dispose().await
    }
}

fn text_results(events: &[SessionEvent]) -> Vec<&str> {
    events
        .iter()
        .filter(|event| event.event_type == "tool/result")
        .filter_map(|event| {
            event
                .data
                .pointer("/message/content/0/content")
                .and_then(Value::as_array)
        })
        .flatten()
        .filter_map(|block| {
            (block.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| block.get("text").and_then(Value::as_str))
                .flatten()
        })
        .collect()
}

#[tokio::test]
async fn real_bash_round_trip_streams_output_and_flushes_jsonl() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let harness = CodingHarness::new(
        &workspace,
        temporary.path().join("sessions"),
        vec![
            tool_response(
                "headless-bash",
                "bash",
                &json!({"command":"echo e2e-ok","description":"Print headless proof"}),
            ),
            answer_response("The exact output was e2e-ok."),
        ],
    )
    .await?;
    let result = harness
        .runner
        .run("Run echo e2e-ok with bash and report it.")
        .await;
    assert_eq!(result.exit_code, 0, "{}", result.stderr);
    assert!(result.stdout.contains("e2e-ok"));
    let events = harness.events(&result);
    assert!(
        events
            .iter()
            .any(|event| { event.event_type == "tool/call" && event.data["name"] == "bash" })
    );
    assert!(
        text_results(&events)
            .iter()
            .any(|text| text.contains("e2e-ok"))
    );
    assert_eq!(harness.requests.lock().len(), 2);
    assert!(harness.persistence_root.exists());
    harness.shutdown().await
}

#[tokio::test]
async fn real_bash_repairs_a_failing_program_without_touching_its_test() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let test = concat!(
        "const assert = require('node:assert');\n",
        "const { add } = require('./add.js');\n",
        "assert.strictEqual(add(2, 3), 5);\n",
        "assert.strictEqual(add(-1, 1), 0);\n",
        "console.log('PASS');\n",
    );
    std::fs::write(
        workspace.join("add.js"),
        "function add(a, b) { return a - b; }\nmodule.exports = { add };\n",
    )?;
    std::fs::write(workspace.join("add.test.js"), test)?;
    assert!(
        !std::process::Command::new("node")
            .arg("add.test.js")
            .current_dir(&workspace)
            .output()?
            .status
            .success()
    );
    let command = concat!(
        "printf '%s\\n' 'function add(a, b) { return a + b; }' ",
        "'module.exports = { add };' > add.js && node add.test.js",
    );
    let harness = CodingHarness::new(
        &workspace,
        temporary.path().join("sessions"),
        vec![
            tool_response(
                "headless-repair",
                "bash",
                &json!({"command":command,"description":"Repair and verify add function"}),
            ),
            answer_response("The repair is verified: PASS."),
        ],
    )
    .await?;
    let result = harness.runner.run("Fix add.js and run its test.").await;
    assert_eq!(result.exit_code, 0, "{}", result.stderr);
    assert_eq!(
        std::fs::read_to_string(workspace.join("add.test.js"))?,
        test
    );
    let after = std::process::Command::new("node")
        .arg("add.test.js")
        .current_dir(&workspace)
        .output()?;
    assert!(
        after.status.success(),
        "{}",
        String::from_utf8_lossy(&after.stderr)
    );
    assert!(String::from_utf8_lossy(&after.stdout).contains("PASS"));
    assert!(!std::fs::read_to_string(workspace.join("add.js"))?.contains("a - b"));
    let events = harness.events(&result);
    assert!(
        text_results(&events)
            .iter()
            .any(|text| text.contains("PASS"))
    );
    harness.shutdown().await
}

#[tokio::test]
async fn todo_turn_records_the_exact_parallel_plan() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let todos = json!([
        {"content":"inspect the failing test","status":"in_progress"},
        {"content":"watch the background build","status":"in_progress"},
        {"content":"apply the fix","status":"pending"}
    ]);
    let harness = CodingHarness::new(
        &workspace,
        temporary.path().join("sessions"),
        vec![
            tool_response("headless-todos", "todo_write", &json!({"todos":todos})),
            answer_response("DONE"),
        ],
    )
    .await?;
    let result = harness.runner.run("Record the exact parallel plan.").await;
    assert_eq!(result.exit_code, 0, "{}", result.stderr);
    let events = harness.events(&result);
    let event = events
        .iter()
        .find(|event| event.event_type == "todo/write")
        .expect("todo tool appends a durable event");
    assert_eq!(event.data["todos"], todos);
    harness.shutdown().await
}

#[tokio::test]
async fn cold_resume_rehydrates_the_prior_fact_into_the_next_model_request() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let persistence_root = temporary.path().join("sessions");
    let first = CodingHarness::new(
        &workspace,
        persistence_root.clone(),
        vec![answer_response("Remembered fact: violet compass.")],
    )
    .await?;
    let first_result = first
        .runner
        .run("Remember that the phrase is violet compass.")
        .await;
    assert_eq!(first_result.exit_code, 0, "{}", first_result.stderr);
    let session_id = first_result
        .session_id
        .clone()
        .expect("first headless run has a session id");
    first.shutdown().await?;

    let second = CodingHarness::new(
        &workspace,
        persistence_root,
        vec![answer_response("The remembered phrase is violet compass.")],
    )
    .await?;
    let mut resume = ResumeAgentOptions::new(session_id.clone());
    resume.agent_options = AgentOptions {
        provider: Some(ProviderId::new("mock")),
        model: Some(ModelId::new("model")),
        max_tokens: None,
        subagent_depth: None,
    };
    let handle = second.agents.resume(resume).await?;
    handle.agent.when_idle()?.await?;
    handle.agent.followup(UserMessage::new(
        vec![ContentBlock::Text {
            text: "What phrase did I ask you to remember?".to_owned(),
        }],
        MessageSource::user(),
    ))?;
    handle.agent.when_idle()?.await?;
    second.sessions.flush(handle.agent.session()).await?;
    {
        let requests = second.requests.lock();
        assert_eq!(requests.len(), 1);
        let history = serde_json::to_string(&requests[0].messages)?;
        assert!(history.contains("Remember that the phrase is violet compass."));
        assert!(history.contains("Remembered fact: violet compass."));
    }
    let events = handle.agent.session().events();
    assert!(events.iter().any(|event| {
        event.event_type == "assistant/message"
            && event
                .data
                .to_string()
                .contains("The remembered phrase is violet compass.")
    }));
    handle.dispose().await?;
    second.shutdown().await
}
