//! Full agent-loop integration parity for foreground and detached Bash calls.

#![cfg(not(windows))]

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use futures::stream;
use seekdeep_agent::AgentOptions;
use seekdeep_agent_loop::{AgentLoopServices, DefaultAgentDriver, LoopAgent};
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{Session, SessionEvent, SessionId},
    session_store::CreateSessionOptions,
};
use seekdeep_jobs_local::{Config as JobsConfig, LocalJobRegistry};
use seekdeep_llm::{
    AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter, Message,
    MessageSource, StreamChunk, UserMessage,
};
use seekdeep_session_persistence::SESSION_PERSISTENCE;
use seekdeep_session_persistence_jsonl::{JsonlCompression, JsonlConfig};
use seekdeep_shell_env::ShellEnvConfig;
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use serde_json::{Value, json};
use tempfile::TempDir;

#[derive(Debug)]
struct ScriptedAdapter {
    responses: Mutex<VecDeque<Vec<StreamChunk>>>,
    requests: Mutex<Vec<Vec<Message>>>,
}

impl ScriptedAdapter {
    fn new(responses: impl IntoIterator<Item = Vec<StreamChunk>>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl LlmAdapter for ScriptedAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests
            .lock()
            .expect("request mutex")
            .push(options.messages);
        let response = self
            .responses
            .lock()
            .expect("response mutex")
            .pop_front()
            .expect("model requested more responses than supplied");
        AdapterStream::new(stream::iter(response.into_iter().map(Ok)))
    }
}

fn tool_response(
    call_id: &str,
    name: &str,
    arguments: &Value,
    text: Option<&str>,
) -> Vec<StreamChunk> {
    let mut chunks = Vec::new();
    if let Some(text) = text {
        chunks.push(StreamChunk::TextDelta {
            index: 0,
            text: text.to_owned(),
        });
    }
    chunks.extend([
        StreamChunk::BlockEnd {
            index: u64::from(text.is_some()),
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
    ]);
    chunks
}

fn text_response(text: &str) -> Vec<StreamChunk> {
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

struct Harness {
    context: Context,
    session: Arc<Session>,
    loop_agent: LoopAgent,
    adapter: Arc<ScriptedAdapter>,
    dependencies: seekdeep_agent_loop_testkit::AgentLoopTestDependencies,
    _driver: Arc<DefaultAgentDriver>,
    _spill: TempDir,
    persistence_root: Option<PathBuf>,
}

async fn harness(
    id: &str,
    responses: Vec<Vec<StreamChunk>>,
    persistence_root: Option<PathBuf>,
    seekdeep_home: Option<String>,
) -> Harness {
    let context = Context::new();
    let dependencies = seekdeep_agent_loop_testkit::mount_agent_loop_test_dependencies(
        &context,
        seekdeep_agent_loop_testkit::AgentLoopTestDependenciesOptions::default(),
    )
    .expect("agent-loop dependencies");
    if let Some(root) = persistence_root.as_ref() {
        let persistence = seekdeep_session_persistence_jsonl::install(
            &context,
            JsonlConfig {
                root: root.clone(),
                compression: JsonlCompression::None,
                ..JsonlConfig::new(root)
            },
        )
        .expect("persistence");
        persistence
            .await_settled()
            .await
            .expect("persistence active");
    }
    let spill = tempfile::tempdir().expect("spill");
    LocalSubprocessRuntime::install_runtime(
        &context,
        Arc::new(LocalSubprocessRuntime::with_spill_dir(spill.path())),
    )
    .expect("subprocess");
    seekdeep_shell_env::apply(&context, &ShellEnvConfig { seekdeep_home }).expect("shell env");
    seekdeep_bash_local::apply(
        &context,
        seekdeep_bash_local::Config {
            timeout_ms: 10_000.0,
            grace_ms: 200.0,
            ..seekdeep_bash_local::Config::default()
        },
    )
    .await
    .expect("bash provider");
    LocalJobRegistry::new(&context, JobsConfig::default()).expect("jobs");
    seekdeep_tool_jobs::apply(&context, &seekdeep_tool_jobs::Config::default()).expect("tool jobs");
    seekdeep_tool_bash::apply(&context, seekdeep_tool_bash::Config::default()).expect("tool bash");

    let adapter = ScriptedAdapter::new(responses);
    dependencies
        .llm
        .register_adapter(&["mock".to_owned()], adapter.clone())
        .expect("mock adapter");
    let session = dependencies
        .sessions
        .create(
            &context,
            Some(SessionId::new(id)),
            CreateSessionOptions::default(),
        )
        .expect("session");
    let (loop_agent, driver) = LoopAgent::new_default(
        &context,
        &session,
        AgentOptions {
            provider: Some("mock".into()),
            model: Some("mock".into()),
            max_tokens: None,
            subagent_depth: None,
        },
        None,
        AgentLoopServices {
            llm: dependencies.llm.clone(),
            system_prompt: dependencies.system_prompt.clone(),
            tools: dependencies.tools.clone(),
            max_parallel_tool_calls: 10,
        },
    )
    .expect("loop agent");
    dependencies
        .agents
        .register(&context, &loop_agent.agent, None)
        .expect("register agent");
    Harness {
        context,
        session,
        loop_agent,
        adapter,
        dependencies,
        _driver: driver,
        _spill: spill,
        persistence_root,
    }
}

async fn run(harness: &Harness, task: &str) {
    harness
        .loop_agent
        .agent
        .followup(UserMessage::new(
            vec![ContentBlock::Text {
                text: task.to_owned(),
            }],
            MessageSource::user(),
        ))
        .expect("followup");
    harness
        .loop_agent
        .agent
        .when_idle()
        .expect("idle waiter")
        .await
        .unwrap();
}

fn event<'a>(events: &'a [SessionEvent], event_type: &str) -> &'a SessionEvent {
    events
        .iter()
        .find(|event| event.event_type == event_type)
        .unwrap_or_else(|| panic!("no {event_type} event in session log"))
}

fn last_event<'a>(events: &'a [SessionEvent], event_type: &str) -> &'a SessionEvent {
    events
        .iter()
        .rev()
        .find(|event| event.event_type == event_type)
        .unwrap_or_else(|| panic!("no {event_type} event in session log"))
}

fn result_text(event: &SessionEvent) -> String {
    event
        .data
        .pointer("/message/content/0/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|block| {
            (block.get("type").and_then(Value::as_str) == Some("text"))
                .then(|| block.get("text").and_then(Value::as_str))
                .flatten()
        })
        .collect()
}

#[tokio::test]
async fn foreground_call_and_nonzero_exit_round_trip_through_the_agent_log_and_history() {
    let foreground = harness(
        "it-bash-fg",
        vec![
            tool_response(
                "call-1",
                "bash",
                &json!({"command":"echo integration-ok", "description":"test command"}),
                Some("Running it."),
            ),
            text_response("The command printed integration-ok."),
        ],
        None,
        None,
    )
    .await;
    run(&foreground, "run echo integration-ok").await;
    let events = foreground.session.events();
    assert_eq!(event(&events, "tool/call").data["name"], "bash");
    let result = event(&events, "tool/result");
    assert_eq!(
        result.data.pointer("/message/content/0/isError"),
        Some(&Value::Bool(false))
    );
    assert_eq!(result_text(result), "integration-ok\n");
    {
        let requests = foreground.adapter.requests.lock().expect("requests");
        assert_eq!(requests.len(), 2);
        assert!(
            requests[1]
                .iter()
                .flat_map(Message::content)
                .any(|block| { matches!(block, ContentBlock::ToolResult { .. }) })
        );
    }

    let failed = harness(
        "it-bash-exit",
        vec![
            tool_response(
                "call-1",
                "bash",
                &json!({"command":"exit 9", "description":"test command"}),
                None,
            ),
            text_response("It failed with code 9."),
        ],
        None,
        None,
    )
    .await;
    run(&failed, "run exit 9").await;
    let events = failed.session.events();
    let result = event(&events, "tool/result");
    assert_eq!(
        result.data.pointer("/message/content/0/isError"),
        Some(&Value::Bool(false))
    );
    assert!(result_text(result).contains("[exit code: 9]"));
}

#[tokio::test]
async fn first_turn_receives_session_identity_and_jsonl_target_before_lazy_materialization() {
    let root = tempfile::tempdir().expect("persistence root");
    let seekdeep_home = root.path().join("seekdeep-home");
    let command = "printf '%s\\n%s\\n%s\\n%s\\n' \"$SEEKDEEP_HOME\" \"$SEEKDEEP_SHELL\" \"$SEEKDEEP_SESSION_ID\" \"$SEEKDEEP_SESSION_JSONL\"; if [ -e \"$SEEKDEEP_SESSION_JSONL\" ]; then printf 'present\\n'; else printf 'absent\\n'; fi";
    let harness = harness(
        "session-env-id",
        vec![
            tool_response(
                "call-1",
                "bash",
                &json!({"command":command, "description":"inspect session environment"}),
                None,
            ),
            text_response("Session environment inspected."),
        ],
        Some(root.path().to_owned()),
        Some(seekdeep_home.to_string_lossy().into_owned()),
    )
    .await;
    let persistence = harness
        .context
        .get(SESSION_PERSISTENCE)
        .expect("persistence service");
    let location = persistence
        .persistence()
        .locate(harness.session.header())
        .expect("jsonl location");
    assert!(!location.path.exists());
    run(&harness, "inspect the current session").await;
    let result = event(&harness.session.events(), "tool/result").clone();
    assert_eq!(
        result_text(&result),
        format!(
            "{}\n1\nsession-env-id\n{}\nabsent\n",
            seekdeep_home.to_string_lossy(),
            location.path.to_string_lossy()
        )
    );
    harness
        .dependencies
        .sessions
        .flush(&harness.session)
        .await
        .expect("flush");
    assert!(location.path.exists());
    assert_eq!(harness.persistence_root.as_deref(), Some(root.path()));
}

#[tokio::test]
async fn background_completion_wakes_idle_agent_and_job_output_collects_the_same_job() {
    let directory = tempfile::tempdir().expect("sentinel directory");
    let sentinel = directory.path().join("release");
    let command = format!(
        "while [ ! -f {} ]; do sleep 0.02; done; echo bg-ok",
        serde_json::to_string(&sentinel.to_string_lossy()).unwrap()
    );
    let harness = harness(
        "it-bash-bg",
        vec![
            tool_response(
                "call-1",
                "bash",
                &json!({
                    "command":command,
                    "description":"test command",
                    "run_in_background":true
                }),
                None,
            ),
            text_response("Started it in the background."),
            tool_response("call-2", "job_output", &json!({"job_id":"bash-1"}), None),
            text_response("Background job finished."),
        ],
        None,
        None,
    )
    .await;
    run(&harness, "run echo bg-ok in the background").await;
    let initial = harness.session.events();
    assert_eq!(
        result_text(event(&initial, "tool/result")),
        "started background job bash-1"
    );
    assert!(!initial.iter().any(|event| {
        event.event_type == "user/message"
            && event.data.pointer("/source/kind") == Some(&json!("plugin"))
    }));

    std::fs::write(&sentinel, "").expect("release job");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let events = harness.session.events();
            let notice = events.iter().any(|event| {
                event.event_type == "user/message"
                    && event.data.pointer("/source/kind") == Some(&json!("plugin"))
            });
            let output = events
                .iter()
                .rev()
                .find(|event| event.event_type == "tool/result")
                .map(result_text)
                .unwrap_or_default();
            if notice && output.contains("bg-ok") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("background wake");
    let events = harness.session.events();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "turn/start")
            .count(),
        2
    );
    let notice = events
        .iter()
        .find(|event| {
            event.event_type == "user/message"
                && event.data.pointer("/source/kind") == Some(&json!("plugin"))
        })
        .expect("completion notice");
    assert_eq!(
        notice.data.pointer("/source/plugin"),
        Some(&json!("tool-jobs"))
    );
    assert_eq!(notice.data.pointer("/source/form"), Some(&json!("notice")));
    let final_result = last_event(&events, "tool/result");
    assert!(result_text(final_result).contains("bg-ok"));
    assert!(result_text(final_result).contains("[status: completed, exit code: 0]"));
}
