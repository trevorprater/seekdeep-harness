//! Claude Code decision, payload, environment, subagent, and lifecycle parity.

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentCancelCause, AgentControlError, AgentController, AgentEvents, AgentOptions,
    AgentRegistry, CancelOptions, Inbox, InboxTarget, MaintenanceReservation,
    NoopInboxNotifications, PreStepDecision,
};
use seekdeep_agent_loop::{AgentPreStepEvent, AgentTurnStoppingEvent, SessionStartEvent};
use seekdeep_core::session::{AppendOptions, Session, SessionId};
use seekdeep_hooks_claude_code::{Config, apply, plugin};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock, UserMessage};
use seekdeep_scope::ScopeKey;
use seekdeep_shell::{
    CollectedOutput, ShellExecRequest, ShellExecSpec, ShellExecutor, ShellProcessHandle,
    ShellRunResult, ShellService,
};
use seekdeep_subagent::{SubagentRunEndInfo, SubagentRunId, SubagentRunInfo, SubagentStopReason};
use seekdeep_system_prompt::{SystemPromptConfig, install as install_prompt};
use seekdeep_tools::{
    ContentToolFixtureOptions, ToolExecutionInput, ToolRuntime, ToolRuntimeConfig,
    define_content_tool_fixture, install as install_tools,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Notify;

#[derive(Debug, Default)]
struct RecordingController {
    messages: Mutex<Vec<(UserMessage, InboxTarget, bool)>>,
}

impl AgentController for RecordingController {
    fn send(
        &self,
        message: UserMessage,
        target: InboxTarget,
        wakeup: bool,
    ) -> Result<(), AgentControlError> {
        self.messages.lock().push((message, target, wakeup));
        Ok(())
    }

    fn cancel(
        &self,
        _cause: AgentCancelCause,
        _options: CancelOptions,
    ) -> Result<(), AgentControlError> {
        Ok(())
    }

    fn when_idle(&self) -> futures::future::BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn begin_maintenance(&self) -> Result<MaintenanceReservation, AgentControlError> {
        Ok(MaintenanceReservation::new(
            AbortSignal::default(),
            Arc::new(|| {}),
        ))
    }
}

#[derive(Clone, Debug)]
enum Behavior {
    Result(ShellRunResult),
    WaitForAbort,
}

#[derive(Debug)]
struct FakeShell {
    behaviors: Mutex<VecDeque<Behavior>>,
    specs: Mutex<Vec<ShellExecSpec>>,
    started: Notify,
}

impl FakeShell {
    fn new(behaviors: impl IntoIterator<Item = Behavior>) -> Arc<Self> {
        Arc::new(Self {
            behaviors: Mutex::new(behaviors.into_iter().collect()),
            specs: Mutex::new(Vec::new()),
            started: Notify::new(),
        })
    }

    fn specs(&self) -> Vec<ShellExecSpec> {
        self.specs.lock().clone()
    }
}

#[async_trait]
impl ShellExecutor for FakeShell {
    fn resolve(&self, request: ShellExecRequest) -> anyhow::Result<ShellExecSpec> {
        Ok(ShellExecSpec {
            command: request.command,
            workdir: request
                .workdir
                .unwrap_or_else(|| PathBuf::from("/executor-default")),
            timeout_ms: request.timeout_ms.unwrap_or(0.0),
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
        self.started.notify_waiters();
        let behavior = self.behaviors.lock().pop_front().expect("behavior");
        match behavior {
            Behavior::Result(result) => Ok(result),
            Behavior::WaitForAbort => {
                spec.signal.as_ref().unwrap().cancelled().await;
                Ok(result(None, "", "aborted"))
            }
        }
    }

    fn start(&self, _spec: ShellExecSpec) -> anyhow::Result<ShellProcessHandle> {
        anyhow::bail!("background start is not used")
    }
}

fn collected(text: &str) -> CollectedOutput {
    CollectedOutput {
        text: text.to_owned(),
        truncated: false,
        spill_path: None,
    }
}

fn result(exit_code: Option<i32>, stdout: &str, stderr: &str) -> ShellRunResult {
    ShellRunResult {
        exit_code,
        signal: None,
        timed_out: false,
        aborted: exit_code.is_none(),
        timeout_ms: 600_000.0,
        stdout: collected(stdout),
        stderr: collected(stderr),
        sandbox: None,
    }
}

fn agent(id: &str, cwd: Option<&str>) -> Arc<Agent> {
    let mut header = seekdeep_core::session::SessionHeader::new(SessionId::new(id));
    header.cwd = cwd.map(str::to_owned);
    let session = Session::create(&header.id.clone(), None, Some(header)).unwrap();
    session
        .append("turn/start", json!({"turn":1}), AppendOptions::default())
        .unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    Arc::new(Agent::new(
        session.id().clone(),
        AgentOptions::default(),
        session,
        inbox,
        seekdeep_cordis::Context::new(),
        ScopeKey::new(),
    ))
}

fn write_config(value: &Value) -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), serde_json::to_vec(value).unwrap()).unwrap();
    file
}

fn config(path: &std::path::Path) -> Config {
    Config {
        config_path: path.to_string_lossy().into_owned(),
        plugin_root: None,
        project_dir: None,
        default_timeout_ms: 600_000.0,
        stderr_summary_max_chars: 500.0,
    }
}

#[derive(Deserialize)]
struct ToolArgs {
    command: String,
    extra: String,
}

fn tool(calls: Arc<AtomicUsize>) -> seekdeep_tools::ToolDefinition {
    define_content_tool_fixture(ContentToolFixtureOptions::new(
        "Edit",
        "fixture",
        json!({
            "command":{"type":"string","required":true},
            "extra":{"type":"string","required":true}
        }),
        Arc::new(move |args: ToolArgs, _| {
            calls.fetch_add(1, Ordering::AcqRel);
            Box::pin(async move {
                Ok(vec![ContentBlock::Text {
                    text: format!("{}:{}", args.command, args.extra),
                }])
            })
        }),
    ))
    .unwrap()
}

fn runtime(context: &seekdeep_cordis::Context) -> Arc<ToolRuntime> {
    let prompt = install_prompt(context, SystemPromptConfig::default()).unwrap();
    install_tools(context, &prompt, ToolRuntimeConfig::default()).unwrap()
}

#[tokio::test]
async fn pre_tool_substitution_env_full_input_and_newline_are_exact() {
    let file = write_config(&json!({
        "PreToolUse":[{"matcher":"Edit|Write","hooks":[{
            "command":"${CLAUDE_PLUGIN_ROOT}/pre ${CLAUDE_PROJECT_DIR}"
        }]}]
    }));
    let shell = FakeShell::new([Behavior::Result(result(Some(2), "", "blocked"))]);
    let context = seekdeep_cordis::Context::new();
    ShellService::new(shell.clone()).provide(&context).unwrap();
    let tools = runtime(&context);
    let calls = Arc::new(AtomicUsize::new(0));
    tools.register(&context, tool(calls.clone())).unwrap();
    let mut bridge_config = config(file.path());
    bridge_config.plugin_root = Some("/plugin".to_owned());
    bridge_config.project_dir = Some("/configured-project".to_owned());
    apply(&context, &bridge_config).unwrap();
    let owner = agent("pre", Some("/session-work"));
    let call = json!({"command":"edit file","extra":"metadata"});
    let execution = ToolExecutionInput::new(
        CallId::new("call-1"),
        "Edit",
        call.clone(),
        AbortSignal::default(),
    )
    .with_agent(owner.clone());
    let outcome = tools.execute(execution).await;
    assert!(outcome.is_error());
    assert_eq!(calls.load(Ordering::Acquire), 0);
    let spec = &shell.specs()[0];
    assert_eq!(spec.command, "/plugin/pre /configured-project");
    assert_eq!(spec.workdir, PathBuf::from("/session-work"));
    assert_eq!(
        spec.env
            .as_ref()
            .and_then(|env| env.get("CLAUDE_PROJECT_DIR"))
            .map(String::as_str),
        Some("/configured-project")
    );
    let stdin = spec.stdin.as_deref().unwrap();
    assert!(stdin.ends_with('\n'));
    let payload: Value = serde_json::from_str(stdin.trim_end()).unwrap();
    assert_eq!(payload["tool_name"], "Edit");
    assert_eq!(payload["tool_input"], call);
    assert_eq!(payload["tool_use_id"], "call-1");
    assert_eq!(owner.session().events()[1].event_type, "hook/invoked");
}

#[tokio::test]
async fn ask_fails_closed_without_approval() {
    let ask = json!({
        "hookSpecificOutput":{
            "hookEventName":"PreToolUse",
            "permissionDecision":"ask",
            "permissionDecisionReason":"confirm"
        }
    })
    .to_string();
    let file = write_config(&json!({
        "PreToolUse":[{"hooks":[{"command":"ask"}]}]
    }));
    let shell = FakeShell::new([Behavior::Result(result(Some(0), &ask, ""))]);
    let context = seekdeep_cordis::Context::new();
    ShellService::new(shell).provide(&context).unwrap();
    let tools = runtime(&context);
    let calls = Arc::new(AtomicUsize::new(0));
    tools.register(&context, tool(calls.clone())).unwrap();
    apply(&context, &config(file.path())).unwrap();
    let outcome = tools
        .execute(
            ToolExecutionInput::new(
                CallId::new("ask-call"),
                "Edit",
                json!({"command":"x","extra":"y"}),
                AbortSignal::default(),
            )
            .with_agent(agent("ask", Some("/work"))),
        )
        .await;
    assert!(outcome.is_error());
    assert_eq!(calls.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn post_context_is_attached_after_the_tool_result() {
    let post = json!({
        "hookSpecificOutput":{
            "hookEventName":"PostToolUse",
            "additionalContext":"post context"
        }
    })
    .to_string();
    let file = write_config(&json!({
        "PostToolUse":[{"hooks":[{"command":"post"}]}]
    }));
    let context = seekdeep_cordis::Context::new();
    ShellService::new(FakeShell::new([Behavior::Result(result(
        Some(0),
        &post,
        "",
    ))]))
    .provide(&context)
    .unwrap();
    let tools = runtime(&context);
    tools
        .register(&context, tool(Arc::new(AtomicUsize::new(0))))
        .unwrap();
    apply(&context, &config(file.path())).unwrap();
    let outcome = tools
        .execute(
            ToolExecutionInput::new(
                CallId::new("post-call"),
                "Edit",
                json!({"command":"x","extra":"y"}),
                AbortSignal::default(),
            )
            .with_agent(agent("post", Some("/work"))),
        )
        .await;
    assert!(!outcome.is_error());
    assert_eq!(outcome.additional_contexts().len(), 1);
    assert_eq!(
        outcome.additional_contexts()[0].content(),
        [ContentBlock::Text {
            text: "post context".to_owned()
        }]
    );
}

#[tokio::test]
async fn subagent_start_stop_use_retained_child_workspace_and_exact_payloads() {
    let file = write_config(&json!({
        "SubagentStart":[{"hooks":[{"command":"start"}]}],
        "SubagentStop":[{"hooks":[{"command":"stop"}]}]
    }));
    let shell = FakeShell::new([
        Behavior::Result(result(Some(0), "", "")),
        Behavior::Result(result(Some(0), "", "")),
    ]);
    let context = seekdeep_cordis::Context::new();
    ShellService::new(shell.clone()).provide(&context).unwrap();
    let registry = Arc::new(AgentRegistry::new(context.clone()));
    registry.provide(&context).unwrap();
    apply(&context, &config(file.path())).unwrap();
    let child = agent("child", Some("/child-work"));
    let registration = registry.register(&context, &child, None).unwrap();
    let run_id = SubagentRunId::new("run-1");
    context
        .events()
        .emit(
            &context,
            "subagent/start",
            &seekdeep_cordis::EventArgs::one(SubagentRunInfo {
                run_id: run_id.clone(),
                provider: "test".to_owned(),
                id: child.id().clone(),
                local: true,
            }),
        )
        .unwrap();
    shell.started.notified().await;
    registration.dispose().await.unwrap();
    context
        .events()
        .emit(
            &context,
            "subagent/end",
            &seekdeep_cordis::EventArgs::one(SubagentRunEndInfo {
                run_id,
                provider: "test".to_owned(),
                id: child.id().clone(),
                local: true,
                stop_reason: SubagentStopReason::Completed,
                last_assistant_message: None,
            }),
        )
        .unwrap();
    while shell.specs().len() < 2 {
        tokio::task::yield_now().await;
    }
    let specs = shell.specs();
    assert_eq!(specs[0].workdir, PathBuf::from("/child-work"));
    assert_eq!(specs[1].workdir, PathBuf::from("/child-work"));
    for (spec, event) in specs.iter().zip(["SubagentStart", "SubagentStop"]) {
        let payload: Value = serde_json::from_str(spec.stdin.as_deref().unwrap().trim()).unwrap();
        assert_eq!(payload["hook_event_name"], event);
        assert_eq!(payload["agent_id"], "child");
        assert_eq!(payload["agent_type"], "general-purpose");
    }
    let stop: Value = serde_json::from_str(specs[1].stdin.as_deref().unwrap().trim()).unwrap();
    assert_eq!(stop["stop_hook_active"], false);
}

#[tokio::test]
async fn plugin_disposal_aborts_detached_subagent_hook_and_removes_listeners() {
    let file = write_config(&json!({
        "SubagentStart":[{"hooks":[{"command":"wait"}]}]
    }));
    let shell = FakeShell::new([Behavior::WaitForAbort]);
    let context = seekdeep_cordis::Context::new();
    ShellService::new(shell.clone()).provide(&context).unwrap();
    let mounted = context
        .plugin(plugin(), serde_json::to_value(config(file.path())).unwrap())
        .unwrap();
    mounted.await_settled().await.unwrap();
    context
        .events()
        .emit(
            &context,
            "subagent/start",
            &seekdeep_cordis::EventArgs::one(SubagentRunInfo {
                run_id: SubagentRunId::new("run-wait"),
                provider: "remote".to_owned(),
                id: SessionId::new("remote-child"),
                local: false,
            }),
        )
        .unwrap();
    shell.started.notified().await;
    mounted.dispose().await.unwrap();
    assert!(shell.specs()[0].signal.as_ref().unwrap().is_aborted());
}

#[tokio::test]
async fn missing_bad_regex_and_invalid_summary_config_are_contained() {
    let context = seekdeep_cordis::Context::new();
    ShellService::new(FakeShell::new([]))
        .provide(&context)
        .unwrap();
    for cap in [0.0, -1.0, 1.5] {
        let mut invalid = config(std::path::Path::new("/missing"));
        invalid.stderr_summary_max_chars = cap;
        assert!(apply(&context, &invalid).is_err());
    }
    apply(&context, &config(std::path::Path::new("/missing"))).unwrap();
    let invalid = write_config(&json!({
        "PreToolUse":[{"matcher":"[","hooks":[{"command":"bad"}]}]
    }));
    apply(&context, &config(invalid.path())).unwrap();
    let unsupported = write_config(&json!({
        "Notification":[{"matcher":"[","hooks":[{"command":"ignored"}]}],
        "PreToolUse":[{"hooks":[{"command":"valid"}]}]
    }));
    let shell = FakeShell::new([Behavior::Result(result(Some(0), "", ""))]);
    let valid_context = seekdeep_cordis::Context::new();
    ShellService::new(shell.clone())
        .provide(&valid_context)
        .unwrap();
    let tools = runtime(&valid_context);
    tools
        .register(&valid_context, tool(Arc::new(AtomicUsize::new(0))))
        .unwrap();
    apply(&valid_context, &config(unsupported.path())).unwrap();
    let outcome = tools
        .execute(ToolExecutionInput::new(
            CallId::new("valid"),
            "Edit",
            json!({"command":"x","extra":"y"}),
            AbortSignal::default(),
        ))
        .await;
    assert!(!outcome.is_error());
    assert_eq!(shell.specs().len(), 1);
}

#[tokio::test]
async fn session_start_prompt_and_stop_map_to_inject_reject_and_steer() {
    let start = json!({
        "hookSpecificOutput":{
            "hookEventName":"SessionStart",
            "additionalContext":"start context"
        }
    })
    .to_string();
    let file = write_config(&json!({
        "SessionStart":[{"hooks":[{"command":"start"}]}],
        "UserPromptSubmit":[{"hooks":[{"command":"prompt"}]}],
        "Stop":[{"hooks":[{"command":"stop"}]}]
    }));
    let shell = FakeShell::new([
        Behavior::Result(result(Some(0), &start, "")),
        Behavior::Result(result(Some(2), "", "blocked prompt")),
        Behavior::Result(result(Some(2), "", "continue work")),
    ]);
    let context = seekdeep_cordis::Context::new();
    ShellService::new(shell.clone()).provide(&context).unwrap();
    apply(&context, &config(file.path())).unwrap();
    let owner = agent("lifecycle", Some("/work"));
    let controller = Arc::new(RecordingController::default());
    owner.install_controller(controller.clone()).unwrap();
    let events = AgentEvents::new(context.clone(), owner.clone());
    events.emit(
        "agent/session-start",
        SessionStartEvent {
            source: seekdeep_agent::SessionStartSource::Startup,
        },
    );
    while controller.messages.lock().is_empty() {
        tokio::task::yield_now().await;
    }
    assert!(!controller.messages.lock()[0].2);

    let prompt = UserMessage::new(
        vec![ContentBlock::Text {
            text: "prompt".to_owned(),
        }],
        seekdeep_llm::MessageSource::user(),
    );
    let decision = events
        .waterfall(
            "agent/pre-step",
            AgentPreStepEvent {
                messages: vec![prompt.clone()],
                turn: 1,
                step: 1,
                signal: AbortSignal::default(),
            },
            || async move {
                Ok(PreStepDecision::Enter {
                    messages: vec![prompt],
                })
            },
        )
        .await
        .unwrap();
    assert_eq!(decision, PreStepDecision::Reject);

    events
        .serial(
            "agent/turn-stopping",
            AgentTurnStoppingEvent {
                turn: 1,
                signal: AbortSignal::default(),
            },
        )
        .await
        .unwrap();
    let messages = controller.messages.lock();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].1, InboxTarget::NextStep);
    assert!(messages[1].2);
    assert_eq!(
        messages[1].0.content(),
        [ContentBlock::Text {
            text: "continue work".to_owned()
        }]
    );
    assert_eq!(shell.specs().len(), 3);
}
