//! Real Rust-worker mirrors of the headless Code Mode E2E contracts.

#![cfg(not(windows))]

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::stream;
use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentHandle, AgentOptions, AgentRegistry, CreateAgentOptions, Inbox,
    NoopInboxNotifications,
};
use seekdeep_agent_loop::{AgentLoop, AgentLoopServices, DEFAULT_MAX_PARALLEL_TOOL_CALLS};
use seekdeep_code_runtime_worker_thread::{
    WorkerThreadCodeRuntimeConfig, install as install_worker,
};
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{Session, SessionHeader, SessionId},
    session_store::SessionStore,
};
use seekdeep_jobs::{JobRegistry as _, JobStatus};
use seekdeep_jobs_local::{Config as JobsConfig, LocalJobRegistry};
use seekdeep_llm::{
    AbortSignal, AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, HarnessError,
    LlmAdapter, LlmRuntime, Message, MessageSource, StreamChunk, UserMessage,
};
use seekdeep_scope::ScopeKey;
use seekdeep_system_prompt::{SYSTEM_PROMPT, SystemPromptConfig};
use seekdeep_tools::{
    RUN_CODE_NAME, ToolDefinition, ToolExecutionInput, ToolExecutionResult, ToolOutputDefinition,
    ToolPresentationMode, ToolRuntime, ToolRuntimeConfig, assert_supported_json_schema,
};
use serde_json::{Map, Value, json};

struct CodeHarness {
    context: Context,
    agents: Arc<AgentRegistry>,
    tools: Arc<ToolRuntime>,
    next_call: AtomicUsize,
}

impl CodeHarness {
    fn new() -> anyhow::Result<Self> {
        let context = Context::new();
        let agents = Arc::new(AgentRegistry::new(context.clone()));
        agents.provide(&context)?;
        let prompt = seekdeep_system_prompt::install(&context, SystemPromptConfig::default())?;
        let tools = seekdeep_tools::install(
            &context,
            &prompt,
            ToolRuntimeConfig {
                mode: ToolPresentationMode::Code,
                ..ToolRuntimeConfig::default()
            },
        )?;
        install_worker(
            &context,
            &WorkerThreadCodeRuntimeConfig {
                compute_ms: Some(5_000.0),
                max_wall_ms: Some(20_000.0),
                max_output_bytes: Some(1_000_000.0),
                max_old_generation_size_mb: Some(64.0),
            },
        )?;
        Ok(Self {
            context,
            agents,
            tools,
            next_call: AtomicUsize::new(1),
        })
    }

    async fn mount_bash(&self, cwd: &Path) -> anyhow::Result<Arc<LocalJobRegistry>> {
        let jobs = LocalJobRegistry::new(&self.context, JobsConfig::default())?;
        seekdeep_tool_jobs::apply(&self.context, &seekdeep_tool_jobs::Config::default())?;
        self.mount_shell(cwd).await?;
        Ok(jobs)
    }

    async fn mount_shell(&self, cwd: &Path) -> anyhow::Result<()> {
        seekdeep_subprocess_local::LocalSubprocessRuntime::install(&self.context)?;
        seekdeep_shell_env::apply(
            &self.context,
            &seekdeep_shell_env::ShellEnvConfig::default(),
        )?;
        seekdeep_bash_local::apply(
            &self.context,
            seekdeep_bash_local::Config {
                cwd: Some(cwd.to_string_lossy().into_owned()),
                timeout_ms: 30_000.0,
                grace_ms: 200.0,
                ..seekdeep_bash_local::Config::default()
            },
        )
        .await?;
        seekdeep_tool_bash::apply(&self.context, seekdeep_tool_bash::Config::default())?;
        Ok(())
    }

    fn input(
        &self,
        code: &str,
        signal: AbortSignal,
        agent: Option<Arc<Agent>>,
    ) -> ToolExecutionInput {
        let ordinal = self.next_call.fetch_add(1, Ordering::AcqRel);
        let input = ToolExecutionInput::new(
            CallId::new(format!("headless-code-{ordinal}")),
            RUN_CODE_NAME,
            json!({"code": code, "description": "Run the E2E program"}),
            signal,
        );
        match agent {
            Some(agent) => input.with_agent(agent),
            None => input,
        }
    }

    async fn run(
        &self,
        code: &str,
        signal: AbortSignal,
        agent: Option<Arc<Agent>>,
    ) -> ToolExecutionResult {
        self.tools.execute(self.input(code, signal, agent)).await
    }

    fn agent(&self, id: &str, cwd: Option<&Path>) -> anyhow::Result<Arc<Agent>> {
        let id = SessionId::new(id);
        let mut header = SessionHeader::new(id.clone());
        header.cwd = cwd.map(|cwd| cwd.to_string_lossy().into_owned());
        let session = Session::create(&id, None, Some(header))?;
        let inbox = Arc::new(Inbox::new(
            session.clone(),
            Arc::new(NoopInboxNotifications),
        )?);
        let agent = Arc::new(Agent::new(
            id,
            AgentOptions::default(),
            session,
            inbox,
            self.context.clone(),
            ScopeKey::new(),
        ));
        self.agents.register(&self.context, &agent, None)?;
        Ok(agent)
    }

    async fn dispose(self) -> anyhow::Result<()> {
        self.agents.dispose_initiators().await;
        self.context.root_fiber().dispose().await
    }

    async fn mount_loop(
        &self,
        cwd: &Path,
        code: &str,
        required: Vec<String>,
        answer: &str,
    ) -> anyhow::Result<LoopFixture> {
        let sessions = SessionStore::install(&self.context)?;
        let llm = LlmRuntime::install(&self.context)?;
        let requests = Arc::new(Mutex::new(Vec::new()));
        llm.register_adapter(
            &["mock".to_owned()],
            Arc::new(CodeModeAdapter {
                called: AtomicBool::new(false),
                code: code.to_owned(),
                required,
                answer: answer.to_owned(),
                requests: requests.clone(),
            }),
        )?;
        seekdeep_agent_loop::install_request_invariant(&self.context, &llm, sessions.clone())?;
        let loop_ = AgentLoop::new(
            self.context.clone(),
            sessions,
            (*self.agents).clone(),
            AgentLoopServices {
                llm,
                system_prompt: self.context.get(SYSTEM_PROMPT).expect("system prompt"),
                tools: self.tools.clone(),
                max_parallel_tool_calls: DEFAULT_MAX_PARALLEL_TOOL_CALLS,
            },
        )?;
        self.agents.set_factory(Arc::new(loop_.clone()))?;
        let mut options = CreateAgentOptions::new(SessionId::new("code-mode-loop"));
        options.meta.cwd = Some(cwd.to_string_lossy().into_owned());
        options.agent_options = AgentOptions {
            provider: Some("mock".into()),
            model: Some("mock".into()),
            ..AgentOptions::default()
        };
        let handle = self.agents.create(options).await?;
        Ok(LoopFixture {
            handle,
            loop_,
            requests,
        })
    }
}

struct CodeModeAdapter {
    called: AtomicBool,
    code: String,
    required: Vec<String>,
    answer: String,
    requests: Arc<Mutex<Vec<GenerateOptions>>>,
}

#[async_trait]
impl LlmAdapter for CodeModeAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        let visible = serde_json::to_string(&options.messages).expect("model messages serialize");
        self.requests.lock().push(options);
        let chunks = if self.called.swap(true, Ordering::AcqRel) {
            let answer = if self
                .required
                .iter()
                .all(|required| visible.contains(required))
            {
                self.answer.clone()
            } else {
                "missing required model context".to_owned()
            };
            vec![
                StreamChunk::TextDelta {
                    index: 0,
                    text: answer,
                },
                StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                },
            ]
        } else {
            vec![
                StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::ToolCall {
                        id: CallId::new("outer-code"),
                        name: RUN_CODE_NAME.to_owned(),
                        arguments: json!({
                            "code": self.code,
                            "description": "Execute the requested Code Mode program"
                        })
                        .to_string(),
                    },
                },
                StreamChunk::Finish {
                    reason: FinishReason::ToolCalls,
                    replay_state: None,
                },
            ]
        };
        AdapterStream::new(stream::iter(chunks.into_iter().map(Ok)))
    }
}

struct LoopFixture {
    handle: AgentHandle,
    loop_: AgentLoop,
    requests: Arc<Mutex<Vec<GenerateOptions>>>,
}

impl LoopFixture {
    async fn prompt(&self, text: &str) -> anyhow::Result<()> {
        self.handle.agent.followup(UserMessage::new(
            vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            MessageSource::user(),
        ))?;
        self.handle.agent.when_idle()?.await
    }

    fn final_text(&self) -> String {
        self.handle
            .agent
            .session()
            .events()
            .into_iter()
            .rev()
            .find(|event| event.event_type == "assistant/message")
            .and_then(|event| event.data.get("message").cloned())
            .and_then(|message| serde_json::from_value::<Message>(message).ok())
            .map(|message| {
                message
                    .content()
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn dispose(self) -> anyhow::Result<()> {
        self.handle.dispose().await?;
        self.loop_.dispose().await
    }
}

fn value_tool(name: &str, schema: Value, value: Value) -> anyhow::Result<ToolDefinition> {
    Ok(ToolDefinition::new(
        name,
        format!("{name} fixture"),
        Map::from_iter([("type".to_owned(), json!("object"))]),
        ToolOutputDefinition::new(
            Arc::new(assert_supported_json_schema(schema)?),
            Arc::new(|_, value| {
                Ok(value.as_str().map_or_else(Vec::new, |text| {
                    vec![ContentBlock::Text {
                        text: text.to_owned(),
                    }]
                }))
            }),
        ),
        Arc::new(move |_, _| {
            let value = value.clone();
            Box::pin(async move { Ok(value) })
        }),
    ))
}

fn completion(result: ToolExecutionResult) -> anyhow::Result<Value> {
    match result {
        ToolExecutionResult::Success(success) => {
            let Value::Object(mut value) = success.value else {
                anyhow::bail!("run_code returned a non-object value");
            };
            value
                .remove("result")
                .ok_or_else(|| anyhow::anyhow!("run_code did not return a completion"))
        }
        ToolExecutionResult::Failure(failure) => Err(anyhow::anyhow!(failure.error.message)),
    }
}

#[tokio::test]
async fn large_intermediate_value_and_typed_failure_cross_the_real_worker() -> anyhow::Result<()> {
    let harness = CodeHarness::new()?;
    harness.tools.register(
        &harness.context,
        value_tool(
            "large_value",
            json!({"type":"string"}),
            json!("x".repeat(100_000)),
        )?,
    )?;
    let mut failure = value_tool("always_fail", json!({"type":"null"}), Value::Null)?;
    failure.execute = Arc::new(|_, _| {
        Box::pin(async {
            Err(HarnessError::new("expected failure", "EXPECTED_INTERNAL_CODE").into())
        })
    });
    harness.tools.register(&harness.context, failure)?;
    let value = completion(
        harness
            .run(
                r"
const large = await tools.large_value({});
let failure;
try { await tools.always_fail({}); } catch (error) {
  failure = {
    typed: error instanceof ToolCallError,
    name: error.name,
    toolName: error.toolName,
    message: error.message,
    exposesCode: 'code' in error,
    exposesContent: 'content' in error,
    exposesInfo: 'info' in error,
  };
}
return { length: large.length, failure };
",
                AbortSignal::default(),
                None,
            )
            .await,
    )?;
    assert_eq!(
        value,
        json!({
            "length": 100_000,
            "failure": {
                "typed": true,
                "name": "ToolCallError",
                "toolName": "always_fail",
                "message": "expected failure",
                "exposesCode": false,
                "exposesContent": false,
                "exposesInfo": false
            }
        })
    );
    harness.dispose().await
}

#[tokio::test]
async fn background_job_survives_outer_completion_and_polls_by_its_typed_id() -> anyhow::Result<()>
{
    let temporary = tempfile::tempdir()?;
    let harness = CodeHarness::new()?;
    harness.mount_bash(temporary.path()).await?;
    let job_id = completion(
        harness
            .run(
                r#"const started = await tools.bash({ command: "sleep 0.2; printf 'background-complete\n'", description: 'Run completion marker in background', run_in_background: true }); return started.jobId;"#,
                AbortSignal::default(),
                None,
            )
            .await,
    )?;
    assert_eq!(job_id, "bash-1");
    let output = completion(
        harness
            .run(
                &format!("return await tools.job_output({{ job_id: {job_id}, wait: true, timeout_ms: 5000 }});"),
                AbortSignal::default(),
                None,
            )
            .await,
    )?;
    assert!(
        output["text"]
            .as_str()
            .unwrap()
            .contains("background-complete")
    );
    assert_eq!(output["job"]["id"], job_id);
    assert_eq!(output["job"]["kind"], "bash");
    assert_eq!(output["job"]["status"], "completed");
    harness.dispose().await
}

#[tokio::test]
async fn background_preabort_spawns_nothing_and_postpublication_abort_leaves_job_kill_owner()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let harness = CodeHarness::new()?;
    let jobs = harness.mount_bash(temporary.path()).await?;
    let pre = AbortSignal::default();
    pre.abort();
    let pre_result = harness
        .run(
            "return await tools.bash({ command: 'sleep 10', description: 'Must never start', run_in_background: true });",
            pre,
            None,
        )
        .await;
    assert!(pre_result.is_error());
    assert!(jobs.list(None).is_empty());

    let after_publication = AbortSignal::default();
    let tools = harness.tools.clone();
    let input = harness.input(
        "const started = await tools.bash({ command: 'sleep 10', description: 'Wait for explicit job kill', run_in_background: true }); console.log(started.jobId); await new Promise(() => {});",
        after_publication.clone(),
        None,
    );
    let running = tokio::spawn(async move { tools.execute(input).await });
    let job = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(job) = jobs.list(None).into_iter().next() {
                break job;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    assert_eq!(job.id.as_str(), "bash-1");
    assert_eq!(job.status, JobStatus::Running);
    after_publication.abort();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), running)
            .await??
            .is_error()
    );
    assert_eq!(jobs.get(&job.id, None)?.status, JobStatus::Running);

    let id = serde_json::to_string(job.id.as_str())?;
    let killed = completion(
        harness
            .run(
                &format!("return await tools.job_kill({{ job_id: {id}, reason: 'test owns cancellation' }});"),
                AbortSignal::default(),
                None,
            )
            .await,
    )?;
    assert_eq!(killed["outcome"], "cancellation-requested");
    assert_eq!(killed["job"]["id"], job.id.as_str());
    let settled = completion(
        harness
            .run(
                &format!("return await tools.job_output({{ job_id: {id}, wait: true, timeout_ms: 5000 }});"),
                AbortSignal::default(),
                None,
            )
            .await,
    )?;
    assert_eq!(settled["job"]["status"], "killed");
    harness.dispose().await
}

#[tokio::test]
async fn foreground_bash_remains_coupled_to_the_outer_code_signal() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let harness = CodeHarness::new()?;
    let jobs = harness.mount_bash(temporary.path()).await?;
    let signal = AbortSignal::default();
    let tools = harness.tools.clone();
    let input = harness.input(
        "return await tools.bash({ command: 'sleep 10', description: 'Run cancellable foreground command' });",
        signal.clone(),
        None,
    );
    let pending = tokio::spawn(async move { tools.execute(input).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    signal.abort();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), pending)
            .await??
            .is_error()
    );
    assert!(jobs.list(None).is_empty());
    harness.dispose().await
}

#[tokio::test]
async fn versioned_cordis_dto_ids_cross_code_mode_for_running_waiting_and_removed_plugins()
-> anyhow::Result<()> {
    let harness = CodeHarness::new()?;
    let _runner =
        seekdeep_cordis_host_runner::DynamicCordisRunner::install(&harness.context, 5_000);
    seekdeep_tool_cordis::apply(&harness.context)?;
    let agent = harness.agent("code-mode-cordis", None)?;
    let value = completion(
        harness
            .run(
                r#"
const activeDefinition = await tools.cordis_define({
  plugin: { kind: 'new', idPrefix: 'active' },
  name: 'active-code-mode-plugin', purpose: 'prove an active Host half',
  code: { host: "return { name: 'active-code-mode-plugin', apply(ctx) {} }" },
});
const active = await tools.cordis_run({ pluginId: activeDefinition.pluginId, packageId: activeDefinition.packageId, mode: 'run' });
const pendingDefinition = await tools.cordis_define({
  plugin: { kind: 'new', idPrefix: 'queue' },
  name: 'pending-code-mode-plugin', purpose: 'prove a Host half waiting for a Service',
  code: { host: "return { name: 'pending-code-mode-plugin', inject: ['missing-code-mode-service'], apply(ctx) {} }" },
});
const pending = await tools.cordis_run({ pluginId: pendingDefinition.pluginId, packageId: pendingDefinition.packageId, mode: 'run' });
const before = await tools.cordis_inspect_self({});
const removed = await tools.cordis_undefine({ pluginId: active.pluginId });
const after = await tools.cordis_inspect_self({});
await tools.cordis_undefine({ pluginId: pending.pluginId });
return {
  active: { pluginId: active.pluginId, packageId: active.packageId, pluginRunId: active.pluginRunId, status: active.host.status },
  pending: { pluginId: pending.pluginId, packageId: pending.packageId, pluginRunId: pending.pluginRunId, status: pending.host.status, waitingFor: pending.host.waitingFor },
  removed,
  beforeContainsId: before.plugins.some(plugin => plugin.pluginId === active.pluginId),
  afterContainsId: after.plugins.some(plugin => plugin.pluginId === active.pluginId),
};
"#,
                AbortSignal::default(),
                Some(agent),
            )
            .await,
    )?;
    assert_eq!(
        value,
        json!({
            "active": {"pluginId":"active-1","packageId":"pkg-1","pluginRunId":"run-1","status":"running"},
            "pending": {"pluginId":"queue-2","packageId":"pkg-2","pluginRunId":"run-2","status":"waiting","waitingFor":["missing-code-mode-service"]},
            "removed": {"pluginId":"active-1","wasRunning":true},
            "beforeContainsId": true,
            "afterContainsId": false
        })
    );
    harness.dispose().await
}

#[tokio::test]
async fn real_loop_offers_only_run_code_and_writes_the_composed_bash_result() -> anyhow::Result<()>
{
    let temporary = tempfile::tempdir()?;
    let harness = CodeHarness::new()?;
    harness.mount_shell(temporary.path()).await?;
    let fixture = harness
        .mount_loop(
            temporary.path(),
            r#"
const first = await tools.bash({ command: 'echo alpha-7', description: 'Read the alpha marker' });
const second = await tools.bash({ command: 'echo beta-9', description: 'Read the beta marker' });
const joined = first.stdout.text.trim() + '+' + second.stdout.text.trim();
await tools.bash({ command: "printf '%s' " + JSON.stringify(joined) + ' > combined.txt', description: 'Write the joined marker' });
return joined;
"#,
            vec!["alpha-7".to_owned(), "beta-9".to_owned()],
            "alpha-7+beta-9",
        )
        .await?;
    fixture
        .prompt("Use one run_code program to combine the two Bash outputs.")
        .await?;
    assert_eq!(fixture.final_text(), "alpha-7+beta-9");
    assert_eq!(
        std::fs::read_to_string(temporary.path().join("combined.txt"))?,
        "alpha-7+beta-9"
    );
    let events = fixture.handle.agent.session().events();
    let headers = events
        .iter()
        .filter(|event| event.event_type == "request/header")
        .collect::<Vec<_>>();
    assert!(!headers.is_empty());
    for header in headers {
        assert_eq!(
            header.data["header"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| tool["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [RUN_CODE_NAME]
        );
    }
    let calls = events
        .iter()
        .filter(|event| event.event_type == "tool/call")
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].data["name"], RUN_CODE_NAME);
    let dispatches = events
        .iter()
        .filter(|event| event.event_type == "tool/code-dispatch")
        .collect::<Vec<_>>();
    assert_eq!(dispatches.len(), 3);
    assert!(dispatches.iter().all(|event| {
        event.data["name"] == "bash" && event.data["parentCallId"] == calls[0].data["callId"]
    }));
    assert_eq!(fixture.requests.lock().len(), 2);
    fixture.dispose().await?;
    harness.dispose().await
}

#[tokio::test]
async fn nested_code_read_projects_workspace_instructions_before_the_next_model_request()
-> anyhow::Result<()> {
    const PROBE: &str = "dragonfruit-8675309";
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir(temporary.path().join(".git"))?;
    std::fs::create_dir_all(temporary.path().join("pkg/deep"))?;
    std::fs::write(
        temporary.path().join("pkg/AGENTS.md"),
        format!(
            "If asked for the Code Mode workspace handshake, reply with exactly {PROBE} and nothing else.\n"
        ),
    )?;
    std::fs::write(
        temporary.path().join("pkg/deep/task.txt"),
        "Touch this file to discover the nested instructions.\n",
    )?;
    let harness = CodeHarness::new()?;
    seekdeep_fs_local::LocalFileSystem::install(
        &harness.context,
        seekdeep_fs_local::Config {
            cwd: Some("/".to_owned()),
            ..seekdeep_fs_local::Config::default()
        },
    )?;
    seekdeep_tool_fs::apply(&harness.context, &seekdeep_tool_fs::Config::default())?;
    seekdeep_agent_instructions::apply(
        &harness.context,
        &seekdeep_agent_instructions::Config {
            seekdeep_home: Some(
                temporary
                    .path()
                    .join(".seekdeep")
                    .to_string_lossy()
                    .into_owned(),
            ),
            max_bytes: 65_536,
            ..seekdeep_agent_instructions::Config::default()
        },
    )?;
    let fixture = harness
        .mount_loop(
            temporary.path(),
            "return await tools.read({ file_path: 'pkg/deep/task.txt' });",
            vec![PROBE.to_owned()],
            PROBE,
        )
        .await?;
    fixture
        .prompt("Use one run_code program to read pkg/deep/task.txt, then answer the Code Mode workspace handshake.")
        .await?;
    let events = fixture.handle.agent.session().events();
    assert!(
        events.iter().any(|event| {
            event.event_type == "tool/code-dispatch" && event.data["name"] == "read"
        })
    );
    assert!(events.iter().any(|event| {
        event.event_type == "agent/inbox/spliced"
            && event.data["inserted"].as_array().is_some_and(|messages| {
                messages.iter().any(|message| {
                    message["source"]["kind"] == "agent-instructions"
                        && message.to_string().contains(PROBE)
                })
            })
    }));
    assert_eq!(fixture.final_text(), PROBE);
    {
        let requests = fixture.requests.lock();
        assert_eq!(requests.len(), 2);
        assert!(serde_json::to_string(&requests[1].messages)?.contains(PROBE));
    }
    fixture.dispose().await?;
    harness.dispose().await
}
