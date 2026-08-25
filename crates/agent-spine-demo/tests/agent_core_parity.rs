//! Broad Agent Spine forwarding, lifecycle, and real-loop composition parity.

use std::{
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use futures::{future::BoxFuture, stream};
use parking_lot::Mutex;
use seekdeep_agent::{AgentOptions, CreateAgentOptions};
use seekdeep_agent_spine_demo::{
    Config, ConfiguredAgent, GoalConfig, OptionalFeature, SkillConfig, apply, pick_spine_config,
};
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SESSIONS},
};
use seekdeep_jobs::{JobHooks, JobOutcome, JobStart, JobTerminalStatus};
use seekdeep_llm::{
    AbortSignal, AdapterStream, CallId, ContentBlock, FinishReason, GenerateOptions, LlmAdapter,
    LlmFailure, Message, MessageSource, ResolvedRetryPolicy, StreamChunk, UserMessage,
    resolve_retry_policy,
};
use seekdeep_shell::{
    CollectedOutput, ShellExecRequest, ShellExecSpec, ShellExecutor, ShellProcess,
    ShellProcessHandle, ShellProcessRead, ShellProcessStatus, ShellRunResult, ShellService,
};
use seekdeep_skill::{SkillLookupOptions, SkillViewOptions};
use seekdeep_system_prompt::{AssembleContext, PromptContext, render_prompt};
use seekdeep_tools::ToolExecutionInput;
use serde_json::{Value, json};

fn minimal() -> Config {
    serde_json::from_value(json!({
        "workspaceContext":false,
        "skills":{"enabled":false},
        "toolBash":false,
        "toolJobs":false
    }))
    .unwrap()
}

fn filesystem_skills(root: &std::path::Path, include_default_roots: bool) -> SkillConfig {
    SkillConfig {
        enabled: Some(true),
        registry: None,
        filesystem: Some(seekdeep_skill_filesystem::Config {
            include_default_roots,
            seekdeep_home: Some(root.join(".seekdeep")),
            agents_home: Some(root.join(".agents-home")),
            watch: false,
            ..seekdeep_skill_filesystem::Config::default()
        }),
        tool: None,
    }
}

fn message_text(message: &Message) -> String {
    message
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
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

#[derive(Debug)]
struct ScriptedAdapter {
    responses: Mutex<VecDeque<Vec<StreamChunk>>>,
    requests: Mutex<Vec<GenerateOptions>>,
    policy: Option<ResolvedRetryPolicy>,
}

impl ScriptedAdapter {
    fn new(responses: impl IntoIterator<Item = Vec<StreamChunk>>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
            policy: None,
        })
    }

    fn retrying(responses: impl IntoIterator<Item = Vec<StreamChunk>>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
            policy: Some(
                resolve_retry_policy(
                    Some(&json!({
                        "mode":"normal",
                        "maxRetries":1,
                        "retryableCodes":["SERVER"],
                        "backoff":{"initialDelayMs":1,"maxDelayMs":1,"jitterRatio":0}
                    })),
                    "agent-spine test provider retryPolicy",
                )
                .unwrap(),
            ),
        })
    }
}

#[async_trait]
impl LlmAdapter for ScriptedAdapter {
    fn provider_retry_policy(&self, _provider: &str) -> Option<ResolvedRetryPolicy> {
        self.policy.clone()
    }

    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests.lock().push(options);
        let response = self
            .responses
            .lock()
            .pop_front()
            .expect("model requested more responses than supplied");
        AdapterStream::new(stream::iter(response.into_iter().map(Ok)))
    }
}

#[derive(Debug)]
struct SettledProcess;

#[async_trait]
impl ShellProcess for SettledProcess {
    fn status(&self) -> ShellProcessStatus {
        ShellProcessStatus::Completed
    }

    fn exit_code(&self) -> Option<i32> {
        Some(0)
    }

    fn signal(&self) -> Option<seekdeep_shell::ProcessSignal> {
        None
    }

    fn sandbox(&self) -> Option<seekdeep_shell::ShellSandboxInfo> {
        None
    }

    async fn done(&self) {}

    fn read_output(&self) -> ShellProcessRead {
        ShellProcessRead::default()
    }

    fn kill(&self) -> bool {
        false
    }
}

#[derive(Debug, Default)]
struct NoopShell {
    specs: Mutex<Vec<ShellExecSpec>>,
}

#[async_trait]
impl ShellExecutor for NoopShell {
    fn resolve(&self, request: ShellExecRequest) -> anyhow::Result<ShellExecSpec> {
        Ok(ShellExecSpec {
            command: request.command,
            workdir: request.workdir.unwrap_or_else(|| PathBuf::from("/")),
            timeout_ms: request.timeout_ms.unwrap_or(1_000.0),
            stdout_max_bytes: request.stdout_max_bytes.unwrap_or(64_000.0),
            signal: request.signal,
            stdin: request.stdin,
            env: request.env,
            seekdeep_env: request.seekdeep_env,
            sandbox_policy: request.sandbox_policy,
        })
    }

    async fn run(&self, spec: ShellExecSpec) -> anyhow::Result<ShellRunResult> {
        self.specs.lock().push(spec.clone());
        Ok(ShellRunResult {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
            aborted: false,
            timeout_ms: spec.timeout_ms,
            stdout: CollectedOutput::default(),
            stderr: CollectedOutput::default(),
            sandbox: None,
        })
    }

    fn start(&self, spec: ShellExecSpec) -> anyhow::Result<ShellProcessHandle> {
        self.specs.lock().push(spec);
        Ok(Arc::new(SettledProcess))
    }
}

fn install_shell(context: &Context) -> Arc<NoopShell> {
    let shell = Arc::new(NoopShell::default());
    let executor: Arc<dyn ShellExecutor> = shell.clone();
    ShellService::new(executor).provide(context).unwrap();
    shell
}

async fn wait_for_tool(runtime: &seekdeep_agent_spine_demo::SpineRuntime, name: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while runtime.tools.get(name, None).is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

async fn create_agent(
    runtime: &seekdeep_agent_spine_demo::SpineRuntime,
    id: &str,
    cwd: Option<&std::path::Path>,
) -> seekdeep_agent::AgentHandle {
    let mut request = CreateAgentOptions::new(SessionId::new(id));
    request.agent_options = AgentOptions {
        provider: Some("mock".into()),
        model: Some("mock".into()),
        ..AgentOptions::default()
    };
    request.meta.cwd = cwd.map(|path| path.to_string_lossy().into_owned());
    runtime.agents.create(request).await.unwrap()
}

fn install_local_fs(context: &Context, root: &std::path::Path) {
    seekdeep_fs_local::LocalFileSystem::install(
        context,
        seekdeep_fs_local::Config {
            cwd: Some(root.to_string_lossy().into_owned()),
            ..seekdeep_fs_local::Config::default()
        },
    )
    .unwrap();
}

fn open_turn_with_user(session: &Arc<Session>, text: &str) {
    session
        .append("turn/start", json!({"turn":1}), AppendOptions::default())
        .unwrap();
    session
        .append(
            "user/message",
            serde_json::to_value(UserMessage::new(
                vec![ContentBlock::Text {
                    text: text.to_owned(),
                }],
                MessageSource::user(),
            ))
            .unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .unwrap();
}

#[derive(Debug)]
struct PendingJobState {
    cancelled: AtomicBool,
    changed: tokio::sync::Notify,
}

struct PendingJobHooks(Arc<PendingJobState>);

impl JobHooks for PendingJobHooks {
    fn cancel(&self, _reason: Option<&str>) {
        self.0.cancelled.store(true, Ordering::Release);
        self.0.changed.notify_waiters();
    }

    fn done(&self) -> BoxFuture<'static, anyhow::Result<JobOutcome>> {
        let state = Arc::clone(&self.0);
        Box::pin(async move {
            loop {
                if state.cancelled.load(Ordering::Acquire) {
                    return Ok(JobOutcome {
                        status: JobTerminalStatus::Killed,
                        detail: None,
                        output: None,
                    });
                }
                state.changed.notified().await;
            }
        })
    }
}

fn pending_job(kind: &str, label: &str) -> JobStart {
    let state = Arc::new(PendingJobState {
        cancelled: AtomicBool::new(false),
        changed: tokio::sync::Notify::new(),
    });
    JobStart {
        kind: kind.to_owned(),
        label: label.to_owned(),
        output_limit_bytes: None,
        owner: None,
        run: Box::new(move || Box::new(PendingJobHooks(state))),
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload.downcast_ref::<String>().map_or_else(
        || payload.downcast_ref::<&str>().copied().unwrap_or(""),
        String::as_str,
    )
}

#[tokio::test]
async fn full_defaults_mount_every_service_and_model_facing_control() {
    let root = tempfile::tempdir().unwrap();
    let context = Context::new();
    let shell = install_shell(&context);
    let mut config = minimal();
    config.seekdeep_home = Some(root.path().join(".seekdeep").to_string_lossy().into_owned());
    config.skills = Some(filesystem_skills(root.path(), false));
    config.tool_bash = None;
    config.tool_jobs = None;
    let runtime = apply(&context, config).await.unwrap();
    wait_for_tool(&runtime, "bash").await;

    assert!(context.get(seekdeep_cordis_timer::TIMER).is_some());
    assert!(context.get(seekdeep_llm::LLM).is_some());
    assert!(context.get(SESSIONS).is_some());
    assert!(context.get(seekdeep_session_title::SESSION_TITLE).is_some());
    assert!(context.get(seekdeep_system_prompt::SYSTEM_PROMPT).is_some());
    assert!(context.get(seekdeep_tools::TOOLS).is_some());
    assert!(context.get(seekdeep_skill::SKILLS).is_some());
    assert!(context.get(seekdeep_agent::AGENTS).is_some());
    assert!(context.get(seekdeep_jobs::JOBS).is_some());
    assert!(context.get(seekdeep_invariants::INVARIANTS).is_some());
    assert!(context.get(seekdeep_agent_loop::AGENT_LOOP).is_some());
    assert!(context.get(seekdeep_shell_env::SHELL_ENV).is_some());
    assert!(context.get(seekdeep_goal::GOAL).is_none());
    for name in ["bash", "skill", "job_output", "job_list", "job_kill"] {
        assert!(runtime.tools.get(name, None).is_some(), "missing {name}");
    }
    let bash = runtime
        .tools
        .execute(ToolExecutionInput::new(
            CallId::new("shared-home"),
            "bash",
            json!({"command":"true","description":"Check the shared home"}),
            AbortSignal::default(),
        ))
        .await;
    assert!(!bash.is_error());
    let managed = shell.specs.lock()[0]
        .seekdeep_env
        .clone()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(
        managed[seekdeep_util::home_paths::SEEKDEEP_HOME_ENV],
        root.path().join(".seekdeep").to_string_lossy()
    );
    assert_eq!(managed["SEEKDEEP_SHELL"], "1");
    assert!(
        context
            .get(seekdeep_skill::SKILLS)
            .unwrap()
            .list(&SkillViewOptions::default())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(runtime.agents.list().is_empty());
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn forwards_titles_goals_persona_and_generated_agent_identity() {
    let context = Context::new();
    let mut config = minimal();
    config.persona = Some("You are main.".to_owned());
    config.session_title = Some(seekdeep_session_title::SessionTitleConfig {
        fallback_max_words: 1,
        fallback_max_bytes: 40,
        max_title_bytes: 80,
    });
    config.goals = Some(OptionalFeature::Config(GoalConfig {
        domain: Some(seekdeep_goal::Config {
            default_max_goal_rounds: Some(17),
        }),
        tool: Some(seekdeep_tool_goal::Config {
            blocked_after_consecutive_rounds: Some(5.0),
        }),
    }));
    config.agents = vec![ConfiguredAgent {
        id: "main".to_owned(),
        provider: Some("mock".to_owned()),
        model: Some("mock".to_owned()),
        ..ConfiguredAgent::default()
    }];
    let runtime = apply(&context, config).await.unwrap();
    let agent = runtime.agents.list().into_iter().next().unwrap();
    assert!(agent.id().as_str().starts_with("main-session-"));
    assert_eq!(agent.id(), agent.session().id());

    let title_session = runtime
        .sessions
        .create(
            &context,
            Some(SessionId::new("configured-title-limits")),
            CreateSessionOptions::default(),
        )
        .unwrap();
    open_turn_with_user(&title_session, "One two three four");
    let title = context.get(seekdeep_session_title::SESSION_TITLE).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while title.get(&title_session).is_none() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(title.get(&title_session).unwrap().event.title, "One");

    let goal = context
        .get(seekdeep_goal::GOAL)
        .unwrap()
        .create(
            &agent,
            &seekdeep_goal::CreateGoalRequest {
                objective: "configured".to_owned(),
                max_goal_rounds: None,
            },
        )
        .unwrap();
    assert_eq!(goal.max_goal_rounds, 17);
    for name in ["create_goal", "get_goal", "update_goal"] {
        assert!(runtime.tools.get(name, None).is_some());
    }
    let assembly = context
        .get(seekdeep_system_prompt::SYSTEM_PROMPT)
        .unwrap()
        .assemble(AssembleContext::default())
        .await
        .unwrap();
    assert_eq!(
        assembly
            .sections
            .iter()
            .find(|section| section.name == "deployment:persona")
            .unwrap()
            .text,
        "You are main."
    );
    assert!(
        assembly
            .sections
            .iter()
            .find(|section| section.name == "tool:goal")
            .unwrap()
            .text
            .contains("at least 5 consecutive goal rounds")
    );
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn forwards_job_admission_and_invariant_selection() {
    let context = Context::new();
    let mut config = minimal();
    config.jobs = Some(seekdeep_jobs_local::Config {
        max_concurrent_jobs_per_owner: Some(1),
    });
    config.tool_jobs = None;
    let runtime = apply(&context, config).await.unwrap();
    wait_for_tool(&runtime, "job_output").await;
    let jobs = context.get(seekdeep_jobs::JOBS).unwrap();
    let first = jobs.start(pending_job("probe", "hold configured slot"));
    let overflow = catch_unwind(AssertUnwindSafe(|| {
        jobs.start(pending_job("probe", "blocked configured task"));
    }))
    .unwrap_err();
    assert!(panic_message(overflow.as_ref()).contains("limit: 1"));
    jobs.kill(&first, None, Some("test complete")).unwrap();

    let nested = runtime
        .sessions
        .create(
            &context,
            Some(SessionId::new("nested-enabled")),
            CreateSessionOptions::default(),
        )
        .unwrap();
    nested
        .append("turn/start", json!({"turn":1}), AppendOptions::default())
        .unwrap();
    assert!(
        nested
            .append("turn/start", json!({"turn":2}), AppendOptions::default())
            .unwrap_err()
            .to_string()
            .contains("turn 1 is still open")
    );
    context.fiber().dispose().await.unwrap();

    for (index, invariants) in [
        seekdeep_invariants::InvariantConfig {
            enabled: false,
            ..seekdeep_invariants::InvariantConfig::default()
        },
        seekdeep_invariants::InvariantConfig {
            package_blocklist: vec!["^@seekdeep-ai/seekdeep-session$".to_owned()],
            ..seekdeep_invariants::InvariantConfig::default()
        },
    ]
    .into_iter()
    .enumerate()
    {
        let context = Context::new();
        let mut config = minimal();
        config.invariants = Some(invariants);
        let runtime = apply(&context, config).await.unwrap();
        let session = runtime
            .sessions
            .create(
                &context,
                Some(SessionId::new(format!("nested-filtered-{index}"))),
                CreateSessionOptions::default(),
            )
            .unwrap();
        session
            .append("turn/start", json!({"turn":1}), AppendOptions::default())
            .unwrap();
        session
            .append("turn/start", json!({"turn":2}), AppendOptions::default())
            .unwrap();
        context.fiber().dispose().await.unwrap();
    }
}

#[tokio::test]
async fn bundled_retry_recovers_inside_one_step_and_titles_the_session() {
    let context = Context::new();
    let runtime = apply(&context, minimal()).await.unwrap();
    let adapter = ScriptedAdapter::retrying([
        vec![StreamChunk::Finish {
            reason: FinishReason::Error {
                failure: LlmFailure {
                    message: "temporary outage".to_owned(),
                    code: "SERVER".to_owned(),
                    status: None,
                    provider_retry_after_ms: None,
                    request_id: None,
                },
            },
            replay_state: None,
        }],
        text_response("recovered by bundled policy"),
    ]);
    runtime
        .llm
        .register_adapter(&["mock".to_owned()], adapter.clone())
        .unwrap();
    let handle = create_agent(&runtime, "bundled-retry-session", None).await;
    handle
        .agent
        .followup(UserMessage::new(
            vec![ContentBlock::Text {
                text: "recover".to_owned(),
            }],
            MessageSource::user(),
        ))
        .unwrap();
    handle.agent.when_idle().unwrap().await.unwrap();

    assert_eq!(adapter.requests.lock().len(), 2);
    let retries = handle
        .agent
        .session()
        .events()
        .into_iter()
        .filter(|event| event.event_type == "llm/retry")
        .collect::<Vec<_>>();
    assert_eq!(retries.len(), 1);
    assert_eq!(retries[0].data["retry"], 1);
    assert_eq!(retries[0].data["provider"], "mock");
    assert_eq!(retries[0].data["mode"], "normal");
    assert_eq!(retries[0].data["maxRetries"], 1);
    assert_eq!(
        handle
            .agent
            .session()
            .events()
            .into_iter()
            .find(|event| event.event_type == "session/title")
            .unwrap()
            .data["title"],
        "recover"
    );
    assert_eq!(
        message_text(handle.agent.session().derive_messages().last().unwrap()),
        "recovered by bundled policy"
    );
    handle.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn workspace_instructions_precede_the_configured_skill_catalog() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    std::fs::write(
        root.path().join("AGENTS.md"),
        "workspace rule before skills",
    )
    .unwrap();
    let custom = root.path().join("custom-skills");
    std::fs::create_dir(&custom).unwrap();
    std::fs::write(
        custom.join("custom-skill.md"),
        "---\nname: custom-skill\ndescription: Custom skill\n---\n\nCustom body.\n",
    )
    .unwrap();

    let context = Context::new();
    let mut config = minimal();
    config.seekdeep_home = Some(root.path().join(".seekdeep").to_string_lossy().into_owned());
    config.workspace_context = OptionalFeature::Config(seekdeep_agent_instructions::Config {
        dsh_home: Some(root.path().join(".seekdeep").to_string_lossy().into_owned()),
        max_bytes: 65_536,
        ..seekdeep_agent_instructions::Config::default()
    });
    let mut skills = filesystem_skills(root.path(), false);
    skills.registry = Some(seekdeep_skill::Config {
        collect_cache_max_entries: Some(4),
    });
    skills.filesystem.as_mut().unwrap().custom_skill_dirs = vec![custom];
    skills.tool = Some(seekdeep_tool_skill::Config {
        catalog_description_max_length: Some(6),
    });
    config.skills = Some(skills);
    let runtime = apply(&context, config).await.unwrap();
    install_local_fs(&context, root.path());
    let adapter = ScriptedAdapter::new([text_response("first")]);
    runtime
        .llm
        .register_adapter(&["mock".to_owned()], adapter.clone())
        .unwrap();
    let handle = create_agent(&runtime, "workspace-order", Some(root.path())).await;
    let skill_names = context
        .get(seekdeep_skill::SKILLS)
        .unwrap()
        .list(&SkillViewOptions {
            lookup: SkillLookupOptions {
                cwd: Some(root.path().to_string_lossy().into_owned()),
                signal: None,
            },
            scope: None,
        })
        .await
        .unwrap()
        .into_iter()
        .map(|skill| skill.name)
        .collect::<Vec<_>>();
    assert_eq!(skill_names, ["custom-skill"]);

    handle
        .agent
        .followup(UserMessage::new(
            vec![ContentBlock::Text {
                text: "hi".to_owned(),
            }],
            MessageSource::user(),
        ))
        .unwrap();
    handle.agent.when_idle().unwrap().await.unwrap();
    {
        let requests = adapter.requests.lock();
        let first = &requests[0];
        let messages = first.messages.iter().map(message_text).collect::<Vec<_>>();
        let workspace_index = messages
            .iter()
            .position(|message| message.contains("workspace rule before skills"))
            .unwrap();
        let catalog_index = messages
            .iter()
            .position(|message| message.contains("- `custom-skill`: Cus..."))
            .unwrap();
        assert!(workspace_index < catalog_index);
        assert!(
            first
                .system
                .as_deref()
                .unwrap()
                .contains("SeekDeep Harness")
        );
        assert!(
            !first
                .system
                .as_deref()
                .unwrap()
                .contains("workspace rule before skills")
        );
    }
    handle.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn zero_workspace_budget_keeps_only_the_original_user_message() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    std::fs::write(root.path().join("AGENTS.md"), "must not be injected").unwrap();
    let context = Context::new();
    let mut config = minimal();
    config.workspace_context = OptionalFeature::Config(seekdeep_agent_instructions::Config {
        dsh_home: Some(root.path().join(".seekdeep").to_string_lossy().into_owned()),
        max_bytes: 0,
        ..seekdeep_agent_instructions::Config::default()
    });
    let runtime = apply(&context, config).await.unwrap();
    install_local_fs(&context, root.path());
    let adapter = ScriptedAdapter::new([text_response("ok")]);
    runtime
        .llm
        .register_adapter(&["mock".to_owned()], adapter.clone())
        .unwrap();
    let handle = create_agent(&runtime, "workspace-disabled", Some(root.path())).await;
    handle
        .agent
        .followup(UserMessage::new(
            vec![ContentBlock::Text {
                text: "hi".to_owned(),
            }],
            MessageSource::user(),
        ))
        .unwrap();
    handle.agent.when_idle().unwrap().await.unwrap();
    {
        let requests = adapter.requests.lock();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].messages.len(), 1);
        assert_eq!(message_text(&requests[0].messages[0]), "hi");
    }
    handle.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn project_skill_refreshes_and_progressively_loads_through_real_tools() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".git")).unwrap();
    std::fs::create_dir_all(root.path().join(".agents/skills/hot-skill")).unwrap();
    let skill_path = ".agents/skills/hot-skill/SKILL.md";
    let skill_source =
        "---\nname: hot-skill\ndescription: Hot-added skill\n---\n\nUse the freshly loaded body.\n";
    let context = Context::new();
    let mut config = minimal();
    config.seekdeep_home = Some(root.path().join(".seekdeep").to_string_lossy().into_owned());
    let mut skills = filesystem_skills(root.path(), true);
    let filesystem = skills.filesystem.as_mut().unwrap();
    filesystem.watch = true;
    filesystem.watch_stability_threshold_ms = 20;
    filesystem.watch_poll_interval_ms = 10;
    config.skills = Some(skills);
    let runtime = apply(&context, config).await.unwrap();
    install_local_fs(&context, root.path());
    seekdeep_tool_fs::apply(&context, &seekdeep_tool_fs::Config::default()).unwrap();
    let adapter = ScriptedAdapter::new([
        tool_response(
            "write-skill",
            "write",
            &json!({"file_path":skill_path,"content":skill_source}),
        ),
        tool_response("load-skill", "skill", &json!({"name":"hot-skill"})),
        text_response("SKILL_REFRESH_OK"),
    ]);
    runtime
        .llm
        .register_adapter(&["mock".to_owned()], adapter.clone())
        .unwrap();
    let handle = create_agent(&runtime, "skill-refresh", Some(root.path())).await;
    handle
        .agent
        .followup(UserMessage::new(
            vec![ContentBlock::Text {
                text: "Create and load the project skill.".to_owned(),
            }],
            MessageSource::user(),
        ))
        .unwrap();
    handle.agent.when_idle().unwrap().await.unwrap();

    {
        let requests = adapter.requests.lock();
        assert_eq!(requests.len(), 3);
        assert!(
            !requests[0]
                .messages
                .iter()
                .map(message_text)
                .collect::<Vec<_>>()
                .join("\n")
                .contains("hot-skill")
        );
        assert!(
            requests[1]
                .messages
                .iter()
                .map(message_text)
                .collect::<Vec<_>>()
                .join("\n")
                .contains("- `hot-skill`: Hot-added skill")
        );
        let loaded_request = serde_json::to_string(&requests[2].messages).unwrap();
        assert!(
            loaded_request.contains("<skill_instructions>"),
            "{loaded_request}"
        );
        assert!(loaded_request.contains("Use the freshly loaded body."));
    }
    assert!(root.path().join(skill_path).is_file());
    assert_eq!(
        message_text(handle.agent.session().derive_messages().last().unwrap()),
        "SKILL_REFRESH_OK"
    );
    handle.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn forwards_tool_configs_and_supports_foreground_only_omissions() {
    let context = Context::new();
    let _shell = install_shell(&context);
    let mut config = minimal();
    config.tool_bash = Some(OptionalFeature::Config(seekdeep_tool_bash::Config {
        enable_run_in_background: Some(false),
    }));
    config.tool_jobs = Some(OptionalFeature::Config(seekdeep_tool_jobs::Config {
        wait_timeout_ms: Some(7.0),
        max_wait_timeout_ms: Some(11.0),
        ..seekdeep_tool_jobs::Config::default()
    }));
    let runtime = apply(&context, config).await.unwrap();
    wait_for_tool(&runtime, "bash").await;
    let bash = runtime
        .tools
        .schemas(None)
        .into_iter()
        .find(|schema| schema.name == "bash")
        .unwrap();
    assert!(
        !bash
            .parameters
            .get("properties")
            .and_then(Value::as_object)
            .unwrap()
            .contains_key("run_in_background")
    );
    let jobs = context.get(seekdeep_jobs::JOBS).unwrap();
    let id = jobs.start(pending_job("probe", "config forwarding probe"));
    let result = tokio::time::timeout(
        Duration::from_millis(250),
        runtime.tools.execute(ToolExecutionInput::new(
            CallId::new("job-config-forwarding"),
            "job_output",
            json!({"job_id":id.as_str(),"wait":true}),
            AbortSignal::default(),
        )),
    )
    .await
    .expect("configured seven-millisecond wait must not use the thirty-second default");
    assert!(!result.is_error());
    jobs.kill(&id, None, Some("test complete")).unwrap();
    context.fiber().dispose().await.unwrap();

    let context = Context::new();
    let _shell = install_shell(&context);
    let mut foreground = minimal();
    foreground.tool_bash = Some(OptionalFeature::Config(seekdeep_tool_bash::Config {
        enable_run_in_background: Some(false),
    }));
    let runtime = apply(&context, foreground).await.unwrap();
    wait_for_tool(&runtime, "bash").await;
    assert_eq!(
        runtime
            .tools
            .schemas(None)
            .into_iter()
            .map(|schema| schema.name)
            .collect::<Vec<_>>(),
        ["bash"]
    );
    assert!(context.get(seekdeep_skill::SKILLS).is_none());
    assert!(context.get(seekdeep_jobs::JOBS).is_some());
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn direct_defaults_pick_config_and_identity_omissions_are_exact() {
    let root = tempfile::tempdir().unwrap();
    let context = Context::new();
    let mut defaults = minimal();
    defaults.seekdeep_home = Some(root.path().join(".seekdeep").to_string_lossy().into_owned());
    defaults.skills = None;
    let runtime = apply(&context, defaults).await.unwrap();
    assert!(context.get(seekdeep_skill::SKILLS).is_some());
    assert!(runtime.tools.get("skill", None).is_some());
    assert!(runtime.agents.list().is_empty());
    assert_eq!(
        context
            .get(seekdeep_system_prompt::SYSTEM_PROMPT)
            .unwrap()
            .assemble(AssembleContext::default())
            .await
            .unwrap()
            .sections
            .iter()
            .find(|section| section.name == "deployment:persona")
            .unwrap()
            .text,
        ""
    );
    context.fiber().dispose().await.unwrap();

    let context = Context::new();
    let _shell = install_shell(&context);
    let mut compatibility = minimal();
    compatibility.include_harness_identity = Some(false);
    compatibility.include_runtime_context = Some(false);
    compatibility.persona = Some("You are a helpful software engineer assistant.".to_owned());
    let runtime = apply(&context, compatibility).await.unwrap();
    assert!(runtime.tools.schemas(None).is_empty());
    assert!(context.get(seekdeep_shell_env::SHELL_ENV).is_none());
    let prompt = context.get(seekdeep_system_prompt::SYSTEM_PROMPT).unwrap();
    prompt
        .prompt_context(&context, PromptContext::new("policy", 0.0, "hidden policy"))
        .unwrap();
    let assembly = prompt.assemble(AssembleContext::default()).await.unwrap();
    assert!(assembly.contexts.is_empty());
    assert_eq!(
        render_prompt(&assembly).unwrap(),
        "You are a helpful software engineer assistant."
    );
    context.fiber().dispose().await.unwrap();

    let mut source = minimal();
    source.agents = vec![ConfiguredAgent {
        id: "entrypoint-only".to_owned(),
        ..ConfiguredAgent::default()
    }];
    source.max_parallel_tool_calls = Some(3);
    source.persona = Some("You are merged.".to_owned());
    source.tool_order = Some(vec!["zulu".to_owned()]);
    source.jobs = Some(seekdeep_jobs_local::Config {
        max_concurrent_jobs_per_owner: Some(4),
    });
    let selected = pick_spine_config(&source);
    assert!(selected.agents.is_empty());
    assert_eq!(selected.max_parallel_tool_calls, Some(3));
    assert_eq!(selected.persona.as_deref(), Some("You are merged."));
    assert_eq!(selected.tool_order, Some(vec!["zulu".to_owned()]));
    assert_eq!(
        selected.jobs.unwrap().max_concurrent_jobs_per_owner,
        Some(4)
    );
}

#[tokio::test]
async fn empty_goal_opt_in_uses_the_goal_domains_owner_default() {
    let context = Context::new();
    let mut config = minimal();
    config.goals = Some(OptionalFeature::Config(GoalConfig::default()));
    config.agents = vec![ConfiguredAgent {
        id: "defaulted-goal".to_owned(),
        provider: Some("mock".to_owned()),
        model: Some("mock".to_owned()),
        ..ConfiguredAgent::default()
    }];
    let runtime = apply(&context, config).await.unwrap();
    let agent = runtime.agents.list().into_iter().next().unwrap();
    let goal = context
        .get(seekdeep_goal::GOAL)
        .unwrap()
        .create(
            &agent,
            &seekdeep_goal::CreateGoalRequest {
                objective: "defaulted".to_owned(),
                max_goal_rounds: None,
            },
        )
        .unwrap();
    assert_eq!(goal.max_goal_rounds, 256);
    assert!(runtime.tools.get("get_goal", None).is_some());
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn config_rejects_invalid_optional_switches_and_duplicate_exact_sessions() {
    for value in [
        json!({"workspaceContext":true}),
        json!({"workspaceContext":false,"toolBash":true}),
        json!({"workspaceContext":false,"toolJobs":true}),
        json!({"workspaceContext":false,"goals":true}),
        json!({"workspaceContext":false,"maxParallelToolCalls":0}),
    ] {
        let context = Context::new();
        let fiber = context
            .plugin(seekdeep_agent_spine_demo::plugin(), value)
            .unwrap();
        assert!(fiber.await_settled().await.is_err());
        context.fiber().dispose().await.unwrap();
    }
    let duplicate = serde_json::from_value::<Config>(json!({
        "workspaceContext":false,
        "agents":[
            {"id":"a","sessionId":"same"},
            {"id":"b","resumeSessionId":"same"}
        ]
    }))
    .unwrap();
    let context = Context::new();
    assert!(
        apply(&context, duplicate)
            .await
            .unwrap_err()
            .to_string()
            .contains("duplicate exact session identity")
    );
    context.fiber().dispose().await.unwrap();
}
