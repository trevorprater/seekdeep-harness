//! Real registry integration, rendering, schemas, background jobs, and revocation.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_agent_loop_testkit::{
    AgentLoopTestDependencies, AgentLoopTestDependenciesOptions, mount_agent_loop_test_dependencies,
};
use seekdeep_cordis::Context;
use seekdeep_core::session::SessionId;
use seekdeep_jobs::{JobRegistry, JobStatus};
use seekdeep_jobs_local::{Config as JobsConfig, LocalJobRegistry};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock};
use seekdeep_scope::{ScopeKey, create_scope, scope_of};
use seekdeep_subprocess::{ProcessGroupId, ProcessId};
use seekdeep_terminal::{
    TerminalBackend, TerminalBackendRef, TerminalBackendSession, TerminalBackendSessionRef,
    TerminalBackendSpawnSpec, TerminalReadRequest, TerminalReadResult, TerminalResult,
    TerminalSendOperation, TerminalSendOperationRef, TerminalSendRead, TerminalSendRequest,
    TerminalSendResult, TerminalSessionService, TerminalSessionStatus, TerminalSignal,
    TerminalSignalResult, TerminalWaitReason,
};
use seekdeep_tool_terminal::{
    Config, DEFAULT_MAX_RESULT_BYTES, MIN_MAX_RESULT_BYTES, apply,
    render::{bound_terminal_text, render_list, render_read, render_send, render_send_read},
};
use seekdeep_tools::ToolExecutionInput;
use serde_json::{Value, json};

#[derive(Debug)]
struct Operation {
    result: TerminalSendResult,
    delta: Mutex<Option<String>>,
    cancelled: AtomicBool,
}

impl TerminalSendOperation for Operation {
    fn done(&self) -> futures::future::BoxFuture<'static, TerminalResult<TerminalSendResult>> {
        let result = self.result.clone();
        Box::pin(async move { Ok(result) })
    }

    fn read_output(&self) -> TerminalSendRead {
        TerminalSendRead {
            delta: self.delta.lock().take().unwrap_or_default(),
            truncated: false,
        }
    }

    fn cancel(&self) -> bool {
        !self.cancelled.swap(true, Ordering::AcqRel)
    }
}

#[derive(Debug)]
struct Session {
    closed: AtomicBool,
}

#[async_trait]
impl TerminalBackendSession for Session {
    fn motd(&self) -> String {
        "stub prompt".to_owned()
    }

    fn pid(&self) -> Option<ProcessId> {
        Some(ProcessId::new(42))
    }

    fn start_send(&self, request: TerminalSendRequest) -> TerminalResult<TerminalSendOperationRef> {
        Ok(Arc::new(Operation {
            result: TerminalSendResult {
                viewport: format!("command output: {}", request.text),
                wait_reason: TerminalWaitReason::StdinRead,
                session_status: TerminalSessionStatus::Running,
                truncated: false,
            },
            delta: Mutex::new(Some("live output".to_owned())),
            cancelled: AtomicBool::new(false),
        }))
    }

    fn read(&self, _request: TerminalReadRequest) -> TerminalResult<TerminalReadResult> {
        Ok(TerminalReadResult {
            text: "history".to_owned(),
            total_lines: 1,
            line_begin: 0,
            line_end: 1,
            truncated: false,
        })
    }

    async fn signal(&self, _signal: TerminalSignal) -> TerminalResult<TerminalSignalResult> {
        Ok(TerminalSignalResult::delivered(ProcessGroupId::new(10)))
    }

    fn status(&self) -> TerminalSessionStatus {
        TerminalSessionStatus::Running
    }

    async fn close(&self, _reason: &str) -> TerminalResult<()> {
        self.closed.store(true, Ordering::Release);
        Ok(())
    }
}

#[derive(Debug)]
struct Backend;

#[async_trait]
impl TerminalBackend for Backend {
    fn backend_type(&self) -> &'static str {
        "stub"
    }

    async fn spawn(
        &self,
        _spec: TerminalBackendSpawnSpec,
    ) -> TerminalResult<TerminalBackendSessionRef> {
        Ok(Arc::new(Session {
            closed: AtomicBool::new(false),
        }))
    }
}

struct Harness {
    context: Context,
    dependencies: AgentLoopTestDependencies,
    terminals: Arc<TerminalSessionService>,
    parent: Arc<Agent>,
}

impl Harness {
    async fn new(config: Config, jobs: bool) -> Self {
        let context = Context::new();
        let dependencies = mount_agent_loop_test_dependencies(
            &context,
            AgentLoopTestDependenciesOptions::default(),
        )
        .unwrap();
        let terminals = TerminalSessionService::install(&context).await.unwrap();
        let backend: TerminalBackendRef = Arc::new(Backend);
        terminals.register_backend(&context, &backend).unwrap();
        let session = dependencies
            .sessions
            .create(
                &context,
                Some(SessionId::new("owner")),
                seekdeep_core::session_store::CreateSessionOptions::default(),
            )
            .unwrap();
        let scope = create_scope(&context, ScopeKey::new(), None).unwrap();
        let scope_key = scope_of(&scope.context).unwrap();
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
        let parent = Arc::new(Agent::new(
            session.id().clone(),
            AgentOptions::default(),
            session,
            inbox,
            scope.context,
            scope_key,
        ));
        dependencies
            .agents
            .register(&context, &parent, None)
            .unwrap();
        if jobs {
            let registry = LocalJobRegistry::new(&context, JobsConfig::default()).unwrap();
            registry.attach_controller("tool-jobs");
        }
        apply(&context, config).unwrap();
        Self {
            context,
            dependencies,
            terminals,
            parent,
        }
    }

    async fn call(&self, name: &str, arguments: Value) -> seekdeep_tools::ToolExecutionResult {
        self.dependencies
            .tools
            .execute(
                ToolExecutionInput::new(
                    CallId::new(format!("call-{name}")),
                    name,
                    arguments,
                    AbortSignal::default(),
                )
                .with_agent(self.parent.clone()),
            )
            .await
    }
}

fn text(result: &seekdeep_tools::ToolExecutionResult) -> String {
    result
        .content()
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn six_tools_drive_the_complete_owner_scoped_lifecycle() {
    let harness = Harness::new(Config::default(), false).await;
    let schemas = harness.dependencies.tools.schemas(None);
    for name in [
        "terminal_open",
        "terminal_send",
        "terminal_read",
        "terminal_signal",
        "terminal_close",
        "terminal_list",
    ] {
        assert!(schemas.iter().any(|schema| schema.name == name), "{name}");
    }
    let opened = harness
        .call(
            "terminal_open",
            json!({ "type": "stub", "name": "main", "cwd": "/tmp" }),
        )
        .await;
    assert!(!opened.is_error());
    assert!(text(&opened).contains("started terminal session pty-1 (main)"));
    let listed = harness.call("terminal_list", json!({})).await;
    assert!(text(&listed).contains("pty-1 (main) [stub] running pid=42"));
    let read = harness
        .call("terminal_read", json!({ "sessionId": "pty-1" }))
        .await;
    assert_eq!(text(&read), "history\n[lines: 0-1 of 1]");
    let signalled = harness
        .call(
            "terminal_signal",
            json!({ "sessionId": "pty-1", "signal": "SIGINT" }),
        )
        .await;
    assert_eq!(
        text(&signalled),
        "delivered SIGINT to foreground process group 10"
    );
    let sent = harness
        .call(
            "terminal_send",
            json!({ "sessionId": "pty-1", "text": "echo hi" }),
        )
        .await;
    assert!(text(&sent).contains("command output: echo hi"));
    assert_eq!(sent.meta().unwrap()["waitReason"], "stdin_read");
    let closed = harness
        .call("terminal_close", json!({ "sessionId": "pty-1" }))
        .await;
    assert_eq!(text(&closed), "closed terminal session pty-1");
    assert_eq!(
        text(&harness.call("terminal_list", json!({})).await),
        "(no terminal sessions)"
    );
    assert!(harness.terminals.list(&harness.parent).is_empty());
}

#[tokio::test]
async fn background_send_registers_a_collectable_job_and_disabled_config_rejects_it() {
    let harness = Harness::new(Config::default(), true).await;
    harness
        .call("terminal_open", json!({ "type": "stub" }))
        .await;
    let started = harness
        .call(
            "terminal_send",
            json!({ "sessionId": "pty-1", "text": "build", "run_in_background": true }),
        )
        .await;
    assert!(text(&started).starts_with("started background job pty-send-"));
    let id = seekdeep_jobs::JobId::new(started.value().unwrap()["jobId"].as_str().unwrap());
    let jobs = harness.context.get(seekdeep_jobs::JOBS).unwrap();
    let settled = jobs
        .wait(&id, 5_000.0, Some(&harness.parent), None)
        .await
        .unwrap();
    assert_eq!(settled.status, JobStatus::Completed);
    assert_eq!(
        jobs.read(&id, Some(&harness.parent)).unwrap().text,
        "live output"
    );

    let disabled = Harness::new(
        Config {
            enable_run_in_background: Some(false),
            max_result_bytes: None,
        },
        false,
    )
    .await;
    assert!(
        disabled
            .dependencies
            .tools
            .get("terminal_send", None)
            .unwrap()
            .parameters["properties"]
            .get("run_in_background")
            .is_none()
    );
    disabled
        .call("terminal_open", json!({ "type": "stub" }))
        .await;
    let rejected = disabled
        .call(
            "terminal_send",
            json!({ "sessionId": "pty-1", "text": "x", "run_in_background": true }),
        )
        .await;
    assert!(rejected.is_error());
}

#[test]
fn renderers_bound_utf8_and_preserve_terminal_metadata() {
    assert_eq!(bound_terminal_text("short", 64), "short");
    let bounded = bound_terminal_text(&"界".repeat(100), 64);
    assert!(bounded.len() <= 64);
    assert!(bounded.contains("[output truncated]"));
    assert_eq!(
        render_send_read(&TerminalSendRead {
            delta: "x".to_owned(),
            truncated: true
        }),
        "x\n[output truncated]"
    );
    assert_eq!(
        render_read(
            &TerminalReadResult {
                text: "history".to_owned(),
                total_lines: 1,
                line_begin: 0,
                line_end: 1,
                truncated: false,
            },
            128,
        ),
        "history\n[lines: 0-1 of 1]"
    );
    let send = render_send(
        &TerminalSendResult {
            viewport: String::new(),
            wait_reason: TerminalWaitReason::Timeout,
            session_status: TerminalSessionStatus::Running,
            truncated: false,
        },
        128,
    );
    assert!(send.contains("(no new output)\n[wait: timeout]\n[session: running]"));
    assert_eq!(render_list(&[], 64), "(no terminal sessions)");
}

#[test]
fn config_bounds_are_source_compatible() {
    assert_eq!(DEFAULT_MAX_RESULT_BYTES, 256 * 1024);
    assert_eq!(MIN_MAX_RESULT_BYTES, 64);
}

#[tokio::test]
async fn presentation_agent_requirement_and_configuration_match_the_public_contract() {
    let harness = Harness::new(Config::default(), false).await;
    let send = harness
        .dependencies
        .tools
        .get("terminal_send", None)
        .unwrap();
    assert!(matches!(
        send.present_call.as_ref().unwrap()(&json!({
            "sessionId": "pty-1",
            "text": "python3"
        })),
        Some(seekdeep_tools::ToolCallView::Terminal(_))
    ));
    assert!(matches!(
        send.present_call.as_ref().unwrap()(&json!({
            "sessionId": "pty-1",
            "text": "make",
            "run_in_background": true
        })),
        Some(seekdeep_tools::ToolCallView::Generic(_))
    ));
    let presented = send.present_result.as_ref().unwrap()(
        &json!({ "sessionId": "pty-1", "text": "x" }),
        &seekdeep_tools::ToolResult {
            content: vec![ContentBlock::Text {
                text: "output".to_owned(),
            }],
            is_error: false,
            meta: None,
        },
    );
    assert!(matches!(
        presented,
        Some(seekdeep_tools::ToolResultView::Terminal(_))
    ));

    let agentless = harness
        .dependencies
        .tools
        .execute(ToolExecutionInput::new(
            CallId::new("agentless"),
            "terminal_list",
            json!({}),
            AbortSignal::default(),
        ))
        .await;
    assert!(agentless.is_error());
    assert!(text(&agentless).contains("initiating agent"));

    let context = Context::new();
    let dependencies =
        mount_agent_loop_test_dependencies(&context, AgentLoopTestDependenciesOptions::default())
            .unwrap();
    TerminalSessionService::install(&context).await.unwrap();
    let error = apply(
        &context,
        Config {
            enable_run_in_background: None,
            max_result_bytes: Some(63),
        },
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("at least 64"));
    drop(dependencies);
}
