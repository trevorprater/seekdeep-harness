//! Codex bridge payload, decision, lifecycle, and durable-event parity.

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
    CancelOptions, Inbox, InboxTarget, MaintenanceReservation, NoopInboxNotifications,
    PreStepDecision,
};
use seekdeep_agent_loop::{AgentPreStepEvent, AgentTurnStoppingEvent};
use seekdeep_core::session::{AppendOptions, Session, SessionId};
use seekdeep_hooks_codex::{Config, apply, plugin};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock, MessageSource, UserMessage};
use seekdeep_scope::ScopeKey;
use seekdeep_shell::{
    CollectedOutput, ShellExecRequest, ShellExecSpec, ShellExecutor, ShellProcessHandle,
    ShellRunResult, ShellService,
};
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
        let behavior = self.behaviors.lock().pop_front().expect("hook behavior");
        match behavior {
            Behavior::Result(result) => Ok(result),
            Behavior::WaitForAbort => {
                spec.signal.as_ref().expect("hook signal").cancelled().await;
                Ok(result(None, "", "aborted"))
            }
        }
    }

    fn start(&self, _spec: ShellExecSpec) -> anyhow::Result<ShellProcessHandle> {
        anyhow::bail!("background start is not used")
    }
}

fn output(text: &str) -> CollectedOutput {
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
        stdout: output(stdout),
        stderr: output(stderr),
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
        model: "codex-model".to_owned(),
        default_timeout_ms: 600_000.0,
        stderr_summary_max_chars: 500.0,
    }
}

#[derive(Deserialize)]
struct BashArgs {
    command: String,
}

fn tool(body_calls: Arc<AtomicUsize>) -> seekdeep_tools::ToolDefinition {
    define_content_tool_fixture(ContentToolFixtureOptions::new(
        "BashOutput",
        "fixture",
        json!({"command":{"type":"string","required":true}}),
        Arc::new(move |args: BashArgs, _| {
            body_calls.fetch_add(1, Ordering::AcqRel);
            Box::pin(async move { Ok(vec![ContentBlock::Text { text: args.command }]) })
        }),
    ))
    .unwrap()
}

fn runtime(context: &seekdeep_cordis::Context) -> Arc<ToolRuntime> {
    let prompt = install_prompt(context, SystemPromptConfig::default()).unwrap();
    install_tools(context, &prompt, ToolRuntimeConfig::default()).unwrap()
}

#[tokio::test]
async fn pre_tool_regex_denies_and_records_exact_payload_without_newline() {
    let file = write_config(&json!({
        "PreToolUse":[{"matcher":"Bash","hooks":[{"command":"pre-hook"}]}]
    }));
    let shell = FakeShell::new([Behavior::Result(result(Some(2), "", "blocked"))]);
    let context = seekdeep_cordis::Context::new();
    ShellService::new(shell.clone()).provide(&context).unwrap();
    let tools = runtime(&context);
    let calls = Arc::new(AtomicUsize::new(0));
    tools.register(&context, tool(calls.clone())).unwrap();
    apply(&context, &config(file.path())).unwrap();
    let owner = agent("pre", Some("/work"));
    let result = tools
        .execute(
            ToolExecutionInput::new(
                CallId::new("call-1"),
                "BashOutput",
                json!({"command":"echo hi"}),
                AbortSignal::default(),
            )
            .with_agent(owner.clone()),
        )
        .await;
    assert!(result.is_error());
    assert_eq!(calls.load(Ordering::Acquire), 0);
    let spec = &shell.specs()[0];
    assert_eq!(spec.workdir, PathBuf::from("/work"));
    let stdin = spec.stdin.as_deref().unwrap();
    assert!(!stdin.ends_with('\n'));
    let payload: Value = serde_json::from_str(stdin).unwrap();
    assert_eq!(payload["hook_event_name"], "PreToolUse");
    assert_eq!(payload["tool_name"], "BashOutput");
    assert_eq!(payload["tool_input"]["command"], "echo hi");
    assert_eq!(payload["tool_use_id"], "call-1");
    assert_eq!(payload["turn_id"], "1");
    let events = owner.session().events();
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "hook/invoked")
    );
    let result_event = events
        .iter()
        .find(|event| event.event_type == "hook/result")
        .unwrap();
    assert_eq!(result_event.data["decision"], "block");
    assert_eq!(result_event.data["stderrSummary"], "blocked");
}

#[tokio::test]
async fn post_tool_context_is_attached_after_success() {
    let stdout = json!({
        "hookSpecificOutput":{
            "hookEventName":"PostToolUse",
            "additionalContext":"post context"
        }
    })
    .to_string();
    let file = write_config(&json!({
        "PostToolUse":[{"hooks":[{"command":"post-hook"}]}]
    }));
    let shell = FakeShell::new([Behavior::Result(result(Some(0), &stdout, ""))]);
    let context = seekdeep_cordis::Context::new();
    ShellService::new(shell.clone()).provide(&context).unwrap();
    let tools = runtime(&context);
    tools
        .register(&context, tool(Arc::new(AtomicUsize::new(0))))
        .unwrap();
    apply(&context, &config(file.path())).unwrap();
    let result = tools
        .execute(
            ToolExecutionInput::new(
                CallId::new("call-2"),
                "BashOutput",
                json!({"command":"echo ok"}),
                AbortSignal::default(),
            )
            .with_agent(agent("post", Some("/work"))),
        )
        .await;
    assert!(!result.is_error());
    assert_eq!(result.additional_contexts().len(), 1);
    assert_eq!(
        result.additional_contexts()[0].source(),
        &MessageSource::plugin("hooks-codex")
    );
    assert_eq!(
        result.additional_contexts()[0].content(),
        [ContentBlock::Text {
            text: "post context".to_owned()
        }]
    );
}

#[tokio::test]
async fn prompt_hook_blocks_or_appends_plain_stdout_after_downstream_messages() {
    let file = write_config(&json!({
        "UserPromptSubmit":[{"hooks":[{"command":"prompt-hook"}]}]
    }));
    let shell = FakeShell::new([
        Behavior::Result(result(Some(2), "", "blocked prompt")),
        Behavior::Result(result(Some(0), "plain context", "")),
    ]);
    let context = seekdeep_cordis::Context::new();
    ShellService::new(shell).provide(&context).unwrap();
    apply(&context, &config(file.path())).unwrap();
    let owner = agent("prompt", Some("/work"));
    let events = AgentEvents::new(context.clone(), owner.clone());
    let message = UserMessage::new(
        vec![ContentBlock::Text {
            text: "hello".to_owned(),
        }],
        MessageSource::user(),
    );
    let blocked = events
        .waterfall(
            "agent/pre-step",
            AgentPreStepEvent {
                messages: vec![message.clone()],
                turn: 1,
                step: 1,
                signal: AbortSignal::default(),
            },
            || async move {
                Ok(PreStepDecision::Enter {
                    messages: vec![message],
                })
            },
        )
        .await
        .unwrap();
    assert_eq!(blocked, PreStepDecision::Reject);

    let original = UserMessage::new(
        vec![ContentBlock::Text {
            text: "original".to_owned(),
        }],
        MessageSource::user(),
    );
    let entered = events
        .waterfall(
            "agent/pre-step",
            AgentPreStepEvent {
                messages: vec![original.clone()],
                turn: 1,
                step: 1,
                signal: AbortSignal::default(),
            },
            || async move {
                Ok(PreStepDecision::Enter {
                    messages: vec![original],
                })
            },
        )
        .await
        .unwrap();
    let PreStepDecision::Enter { messages } = entered else {
        panic!("enter decision");
    };
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[1].content(),
        [ContentBlock::Text {
            text: "plain context".to_owned()
        }]
    );
}

#[tokio::test]
async fn missing_or_invalid_config_registers_nothing_and_plugin_disposal_removes_listeners() {
    let context = seekdeep_cordis::Context::new();
    let shell = FakeShell::new([Behavior::Result(result(Some(0), "", ""))]);
    ShellService::new(shell.clone()).provide(&context).unwrap();
    for cap in [0.0, -1.0, 1.5] {
        let mut invalid = config(std::path::Path::new("/missing"));
        invalid.stderr_summary_max_chars = cap;
        assert!(apply(&context, &invalid).is_err());
    }
    apply(
        &context,
        &Config {
            config_path: "/definitely/missing/hooks.json".to_owned(),
            model: String::new(),
            default_timeout_ms: 600_000.0,
            stderr_summary_max_chars: 500.0,
        },
    )
    .unwrap();
    assert_eq!(
        context
            .events()
            .listener_count(&context, "tools/pre-execute"),
        0
    );

    let invalid = write_config(&json!({
        "PreToolUse":[{"matcher":"[","hooks":[{"command":"bad"}]}]
    }));
    apply(&context, &config(invalid.path())).unwrap();
    assert_eq!(
        context
            .events()
            .listener_count(&context, "tools/pre-execute"),
        0
    );

    let valid = write_config(&json!({
        "PreToolUse":[{"hooks":[{"command":"ok"}]}]
    }));
    let mounted = context
        .plugin(
            plugin(),
            serde_json::to_value(config(valid.path())).unwrap(),
        )
        .unwrap();
    mounted.await_settled().await.unwrap();
    let tools = runtime(&context);
    tools
        .register(&context, tool(Arc::new(AtomicUsize::new(0))))
        .unwrap();
    let owner = agent("lifecycle", Some("/work"));
    let invoke = |call_id: &str| {
        ToolExecutionInput::new(
            CallId::new(call_id),
            "BashOutput",
            json!({"command":"echo ok"}),
            AbortSignal::default(),
        )
        .with_agent(owner.clone())
    };
    assert!(!tools.execute(invoke("before-dispose")).await.is_error());
    assert_eq!(shell.specs().len(), 1);
    mounted.dispose().await.unwrap();
    assert!(!tools.execute(invoke("after-dispose")).await.is_error());
    assert_eq!(shell.specs().len(), 1);
}

#[tokio::test]
async fn disposal_aborts_and_drains_detached_session_start_hook() {
    let file = write_config(&json!({
        "SessionStart":[{"hooks":[{"command":"start-hook"}]}]
    }));
    let shell = FakeShell::new([Behavior::WaitForAbort]);
    let context = seekdeep_cordis::Context::new();
    ShellService::new(shell.clone()).provide(&context).unwrap();
    let mounted = context
        .plugin(plugin(), serde_json::to_value(config(file.path())).unwrap())
        .unwrap();
    mounted.await_settled().await.unwrap();
    let owner = agent("start", Some("/work"));
    AgentEvents::new(context.clone(), owner).emit(
        "agent/session-start",
        seekdeep_agent_loop::SessionStartEvent {
            source: seekdeep_agent::SessionStartSource::Startup,
        },
    );
    shell.started.notified().await;
    mounted.dispose().await.unwrap();
    assert!(shell.specs()[0].signal.as_ref().unwrap().is_aborted());
}

#[tokio::test]
async fn blocking_stop_steers_with_reason_or_default() {
    for (stderr, expected) in [
        ("continue with tests", "continue with tests"),
        ("", "continue: blocked by Stop hook"),
    ] {
        let file = write_config(&json!({
            "Stop":[{"hooks":[{"command":"stop-hook"}]}]
        }));
        let shell = FakeShell::new([Behavior::Result(result(Some(2), "", stderr))]);
        let context = seekdeep_cordis::Context::new();
        ShellService::new(shell).provide(&context).unwrap();
        apply(&context, &config(file.path())).unwrap();
        let owner = agent("stop", Some("/work"));
        let controller = Arc::new(RecordingController::default());
        owner.install_controller(controller.clone()).unwrap();
        AgentEvents::new(context, owner)
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
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].1, InboxTarget::NextStep);
        assert!(messages[0].2);
        assert_eq!(
            messages[0].0.source(),
            &MessageSource::plugin("hooks-codex")
        );
        assert_eq!(
            messages[0].0.content(),
            [ContentBlock::Text {
                text: expected.to_owned()
            }]
        );
    }
}
