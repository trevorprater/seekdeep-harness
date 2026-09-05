//! Persistent state, framing, exit status, timeout reset, config, and lifecycle parity.

use std::sync::Arc;

use async_trait::async_trait;
use futures::{FutureExt, future::Shared};
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_agent_loop_testkit::{
    AgentLoopTestDependencies, AgentLoopTestDependenciesOptions, mount_agent_loop_test_dependencies,
};
use seekdeep_cordis::Context;
use seekdeep_core::session::SessionId;
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
use seekdeep_tool_bash_persistent::{Config, apply, plugin};
use seekdeep_tools::ToolExecutionInput;
use serde_json::json;

type Done = Shared<futures::future::BoxFuture<'static, TerminalResult<TerminalSendResult>>>;

#[derive(Debug)]
struct Operation {
    done: Done,
    delta: Mutex<Option<String>>,
}

impl TerminalSendOperation for Operation {
    fn done(&self) -> futures::future::BoxFuture<'static, TerminalResult<TerminalSendResult>> {
        Box::pin(self.done.clone())
    }

    fn read_output(&self) -> TerminalSendRead {
        TerminalSendRead {
            delta: self.delta.lock().take().unwrap_or_default(),
            truncated: false,
        }
    }

    fn cancel(&self) -> bool {
        false
    }
}

#[derive(Debug)]
struct ShellState {
    scrollback: String,
    environment: Option<String>,
    cwd: String,
    closed: Vec<String>,
    sends: usize,
}

#[derive(Debug)]
struct ShellSession {
    state: Arc<Mutex<ShellState>>,
}

fn marker(text: &str, prefix: &str) -> String {
    let start = text.find(prefix).expect("marker start");
    let body = start + prefix.len();
    let end = text[body..].find("__").expect("marker end") + body + 2;
    text[start..end].to_owned()
}

impl ShellSession {
    fn operation(
        result: impl std::future::Future<Output = TerminalResult<TerminalSendResult>> + Send + 'static,
        delta: String,
    ) -> TerminalSendOperationRef {
        Arc::new(Operation {
            done: result.boxed().shared(),
            delta: Mutex::new(Some(delta)),
        })
    }
}

#[async_trait]
impl TerminalBackendSession for ShellSession {
    fn motd(&self) -> String {
        "__DSH_PERSISTENT_BASH_PROMPT__ ".to_owned()
    }

    fn pid(&self) -> Option<ProcessId> {
        Some(ProcessId::new(42))
    }

    fn start_send(&self, request: TerminalSendRequest) -> TerminalResult<TerminalSendOperationRef> {
        self.state.lock().sends += 1;
        if request.text.starts_with("stty -echo") {
            return Ok(Self::operation(
                async {
                    Ok(TerminalSendResult {
                        viewport: "__DSH_PERSISTENT_BASH_PROMPT__ ".to_owned(),
                        wait_reason: TerminalWaitReason::StdinRead,
                        session_status: TerminalSessionStatus::Running,
                        truncated: false,
                    })
                },
                String::new(),
            ));
        }
        let start = marker(&request.text, "__DSH_PERSISTENT_BASH_START_");
        let end = {
            let begin = request
                .text
                .find("__DSH_PERSISTENT_BASH_END_")
                .expect("end marker");
            let finish = request.text[begin..].find(':').unwrap() + begin;
            request.text[begin..=finish].to_owned()
        };
        let output = if request.text.contains("export FOO=bar") {
            self.state.lock().environment = Some("bar".to_owned());
            String::new()
        } else if request.text.contains("echo $FOO") {
            self.state.lock().environment.clone().unwrap_or_default()
        } else if request.text.contains("cd /tmp") {
            "/tmp".clone_into(&mut self.state.lock().cwd);
            String::new()
        } else if request.text.contains("eval -- $'pwd'") {
            self.state.lock().cwd.clone()
        } else if request.text.contains("eval -- $'false'") {
            let rendered = format!("{start}\n{end}7\n__DSH_PERSISTENT_BASH_PROMPT__ ");
            self.state.lock().scrollback.push_str(&rendered);
            return Ok(Self::operation(
                async move {
                    Ok(TerminalSendResult {
                        viewport: rendered,
                        wait_reason: TerminalWaitReason::StdinRead,
                        session_status: TerminalSessionStatus::Running,
                        truncated: false,
                    })
                },
                String::new(),
            ));
        } else if request.text.contains("eval -- $'hang'") {
            let partial = format!("{start}\npartial output\n");
            self.state.lock().scrollback.push_str(&partial);
            let signal = request.signal.unwrap();
            return Ok(Self::operation(
                async move {
                    signal.cancelled().await;
                    Ok(TerminalSendResult {
                        viewport: "partial output".to_owned(),
                        wait_reason: TerminalWaitReason::StdinRead,
                        session_status: TerminalSessionStatus::Running,
                        truncated: false,
                    })
                },
                "partial output".to_owned(),
            ));
        } else {
            "hello".to_owned()
        };
        let rendered = format!(
            "{start}\n{output}{}{end}0\n__DSH_PERSISTENT_BASH_PROMPT__ ",
            if output.is_empty() { "" } else { "\n" }
        );
        self.state.lock().scrollback.push_str(&rendered);
        Ok(Self::operation(
            async move {
                Ok(TerminalSendResult {
                    viewport: rendered,
                    wait_reason: TerminalWaitReason::StdinRead,
                    session_status: TerminalSessionStatus::Running,
                    truncated: false,
                })
            },
            String::new(),
        ))
    }

    fn read(&self, _request: TerminalReadRequest) -> TerminalResult<TerminalReadResult> {
        let text = self.state.lock().scrollback.clone();
        let lines = text.split('\n').count() as u64;
        Ok(TerminalReadResult {
            text,
            total_lines: lines,
            line_begin: 0,
            line_end: lines,
            truncated: false,
        })
    }

    async fn signal(&self, _signal: TerminalSignal) -> TerminalResult<TerminalSignalResult> {
        Ok(TerminalSignalResult::delivered(ProcessGroupId::new(1)))
    }

    fn status(&self) -> TerminalSessionStatus {
        TerminalSessionStatus::Running
    }

    async fn close(&self, reason: &str) -> TerminalResult<()> {
        self.state.lock().closed.push(reason.to_owned());
        Ok(())
    }
}

#[derive(Debug)]
struct Backend {
    sessions: Arc<Mutex<Vec<Arc<Mutex<ShellState>>>>>,
}

#[async_trait]
impl TerminalBackend for Backend {
    fn backend_type(&self) -> &'static str {
        "stub"
    }

    async fn spawn(
        &self,
        spec: TerminalBackendSpawnSpec,
    ) -> TerminalResult<TerminalBackendSessionRef> {
        let state = Arc::new(Mutex::new(ShellState {
            scrollback: "__DSH_PERSISTENT_BASH_PROMPT__ ".to_owned(),
            environment: None,
            cwd: spec.cwd.unwrap_or_else(|| "/workspace".to_owned()),
            closed: Vec::new(),
            sends: 0,
        }));
        self.sessions.lock().push(state.clone());
        Ok(Arc::new(ShellSession { state }))
    }
}

struct Harness {
    dependencies: AgentLoopTestDependencies,
    owner: Arc<Agent>,
    sessions: Arc<Mutex<Vec<Arc<Mutex<ShellState>>>>>,
}

impl Harness {
    async fn new(config: Config) -> (Self, Arc<seekdeep_cordis::PluginFiber>) {
        let context = Context::new();
        let dependencies = mount_agent_loop_test_dependencies(
            &context,
            AgentLoopTestDependenciesOptions::default(),
        )
        .unwrap();
        let terminals = TerminalSessionService::install(&context).await.unwrap();
        let sessions = Arc::new(Mutex::new(Vec::new()));
        let backend: TerminalBackendRef = Arc::new(Backend {
            sessions: Arc::clone(&sessions),
        });
        terminals.register_backend(&context, &backend).unwrap();
        let session = dependencies
            .sessions
            .create(
                &context,
                Some(SessionId::new("owner")),
                seekdeep_core::session_store::CreateSessionOptions {
                    cwd: Some("/workspace".to_owned()),
                    ..seekdeep_core::session_store::CreateSessionOptions::default()
                },
            )
            .unwrap();
        let scope = create_scope(&context, ScopeKey::new(), None).unwrap();
        let scope_key = scope_of(&scope.context).unwrap();
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
        let owner = Arc::new(Agent::new(
            session.id().clone(),
            AgentOptions::default(),
            session,
            inbox,
            scope.context,
            scope_key,
        ));
        dependencies
            .agents
            .register(&context, &owner, None)
            .unwrap();
        let mounted = context
            .plugin(plugin(), serde_json::to_value(config).unwrap())
            .unwrap();
        mounted.await_settled().await.unwrap();
        (
            Self {
                dependencies,
                owner,
                sessions,
            },
            mounted,
        )
    }

    async fn call_with_signal(
        &self,
        command: &str,
        signal: AbortSignal,
    ) -> seekdeep_tools::ToolExecutionResult {
        self.dependencies
            .tools
            .execute(
                ToolExecutionInput::new(
                    CallId::new(format!("call-{command}")),
                    "bash",
                    json!({ "command": command }),
                    signal,
                )
                .with_agent(self.owner.clone()),
            )
            .await
    }

    async fn call(&self, command: &str) -> seekdeep_tools::ToolExecutionResult {
        self.call_with_signal(command, AbortSignal::default()).await
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
async fn one_owner_reuses_shell_state_nonzero_status_and_disposal_closes_it() {
    let (harness, mounted) = Harness::new(Config {
        backend_type: Some("stub".to_owned()),
        description: Some("persistent stub".to_owned()),
        ..Config::default()
    })
    .await;
    let schema = harness.dependencies.tools.get("bash", None).unwrap();
    assert_eq!(schema.description, "persistent stub");
    assert!(matches!(
        schema.present_call.as_ref().unwrap()(&json!({ "command": "pwd" })),
        Some(seekdeep_tools::ToolCallView::Terminal(_))
    ));
    assert_eq!(text(&harness.call("export FOO=bar").await), "");
    assert_eq!(text(&harness.call("echo $FOO").await), "bar");
    assert_eq!(text(&harness.call("cd /tmp").await), "");
    assert_eq!(text(&harness.call("pwd").await), "/tmp");
    assert_eq!(text(&harness.call("false").await), "[exit code: 7]");
    assert_eq!(harness.sessions.lock().len(), 1);
    assert_eq!(harness.sessions.lock()[0].lock().sends, 6);

    mounted.dispose().await.unwrap();
    assert!(harness.dependencies.tools.get("bash", None).is_none());
    assert!(
        harness.sessions.lock()[0]
            .lock()
            .closed
            .contains(&"tool-bash-persistent disposed".to_owned())
    );
}

#[tokio::test]
async fn timeout_returns_partial_output_resets_shell_and_next_call_recovers() {
    let (harness, _mounted) = Harness::new(Config {
        backend_type: Some("stub".to_owned()),
        timeout_ms: Some(10),
        max_output_chars: Some(1_000),
        description: None,
    })
    .await;
    let timed_out = harness.call("hang").await;
    assert!(!timed_out.is_error());
    assert!(text(&timed_out).contains("timed out after 0 seconds"));
    assert!(text(&timed_out).contains("partial output"));
    assert!(text(&timed_out).contains("next bash call starts"));
    assert_eq!(text(&harness.call("pwd").await), "/workspace");
    assert_eq!(harness.sessions.lock().len(), 2);
}

#[tokio::test]
async fn agentless_blank_and_preaborted_calls_fail_without_creating_a_shell() {
    let (harness, _mounted) = Harness::new(Config {
        backend_type: Some("stub".to_owned()),
        ..Config::default()
    })
    .await;
    assert!(harness.call(" ").await.is_error());
    let aborted = AbortSignal::default();
    aborted.abort();
    assert!(harness.call_with_signal("pwd", aborted).await.is_error());
    let agentless = harness
        .dependencies
        .tools
        .execute(ToolExecutionInput::new(
            CallId::new("agentless"),
            "bash",
            json!({ "command": "pwd" }),
            AbortSignal::default(),
        ))
        .await;
    assert!(agentless.is_error());
    assert!(harness.sessions.lock().is_empty());
}

#[test]
fn direct_config_validation_rejects_empty_and_zero_values() {
    let context = Context::new();
    assert!(
        apply(
            &context,
            Config {
                backend_type: Some(String::new()),
                ..Config::default()
            }
        )
        .unwrap_err()
        .to_string()
        .contains("backendType")
    );
    assert!(
        apply(
            &context,
            Config {
                timeout_ms: Some(0),
                ..Config::default()
            }
        )
        .unwrap_err()
        .to_string()
        .contains("timeoutMs")
    );
    assert!(
        apply(
            &context,
            Config {
                max_output_chars: Some(0),
                ..Config::default()
            }
        )
        .unwrap_err()
        .to_string()
        .contains("maxOutputChars")
    );
}
