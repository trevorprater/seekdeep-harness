//! Typed request, session ownership, notification, shutdown, and raw transport parity.

use std::{
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context as TaskContext, Poll},
};

use async_trait::async_trait;
use futures::{future::BoxFuture, stream};
use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentCancelCause, AgentControlError, AgentController, AgentEvent, AgentFactory,
    AgentHandle, AgentRegistry, AgentStatus, AgentStatusChanged, CancelOptions, CreateAgentOptions,
    Inbox, InboxTarget, MaintenanceReservation, NoopInboxNotifications, ResumeAgentOptions,
};
use seekdeep_cordis::{Context, EventArgs};
use seekdeep_core::{
    session::{AppendOptions, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::{
    AbortSignal, AdapterStream, GenerateOptions, LlmAdapter, LlmRuntime, ModelId, ProviderId,
    UserMessage,
};
use seekdeep_scope::{ScopeKey, create_scope, scope_target};
use seekdeep_sdk_protocol::{InitializeParams, JsonRpcLineTransport, SessionPromptParams};
use seekdeep_sdk_server::{
    Config, HarnessSdkJsonRpcServer, HarnessSdkJsonRpcServerOptions, INJECT, NAME, ServerRuntime,
    apply_with_runtime, plugin, success_status,
};
use seekdeep_subagent::{SubagentRunEndInfo, SubagentRunId, SubagentStopReason};
use serde_json::{Map, Value, json};
use tokio::io::AsyncWrite;

struct FailingFlush<W> {
    inner: W,
    flushes: Arc<AtomicUsize>,
}

impl<W: AsyncWrite + Unpin> AsyncWrite for FailingFlush<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(context, buffer)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _context: &mut TaskContext<'_>,
    ) -> Poll<io::Result<()>> {
        self.flushes.fetch_add(1, Ordering::AcqRel);
        Poll::Ready(Err(io::Error::other("injected flush failure")))
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(context)
    }
}

#[derive(Default)]
struct RecordingController {
    messages: Arc<Mutex<Vec<UserMessage>>>,
}

impl AgentController for RecordingController {
    fn send(
        &self,
        message: UserMessage,
        _target: InboxTarget,
        _wakeup: bool,
    ) -> Result<(), AgentControlError> {
        self.messages.lock().push(message);
        Ok(())
    }

    fn cancel(
        &self,
        _cause: AgentCancelCause,
        _options: CancelOptions,
    ) -> Result<(), AgentControlError> {
        Ok(())
    }

    fn when_idle(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn begin_maintenance(&self) -> Result<MaintenanceReservation, AgentControlError> {
        Ok(MaintenanceReservation::new(
            AbortSignal::default(),
            Arc::new(|| {}),
        ))
    }
}

struct TestFactory {
    sessions: Arc<SessionStore>,
    agents: Arc<AgentRegistry>,
    creates: AtomicUsize,
    failures: AtomicUsize,
    options: Arc<Mutex<Vec<CreateAgentOptions>>>,
    messages: Arc<Mutex<Vec<UserMessage>>>,
    dispose_failures: Arc<AtomicUsize>,
}

#[async_trait]
impl AgentFactory for TestFactory {
    async fn create_agent(
        &self,
        owner_context: &Context,
        options: CreateAgentOptions,
    ) -> anyhow::Result<AgentHandle> {
        self.creates.fetch_add(1, Ordering::AcqRel);
        if self
            .failures
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            anyhow::bail!("injected creation failure");
        }
        self.options.lock().push(options.clone());
        let scope = Arc::new(create_scope(owner_context, ScopeKey::new(), None)?);
        let session = self.sessions.create(
            &scope.context,
            Some(options.session_id.clone()),
            CreateSessionOptions {
                seed: options.seed,
                cwd: options.meta.cwd,
                parent_session: options.meta.parent_session,
                created_at: None,
                seed_length: options.meta.seed_length,
                origin: options.meta.origin,
                delegation_depth: options.meta.delegation_depth,
                agent_preset: options.meta.agent_preset,
            },
        )?;
        let inbox = Arc::new(Inbox::new(
            session.clone(),
            Arc::new(NoopInboxNotifications),
        )?);
        let agent = Arc::new(Agent::new(
            options.session_id,
            options.agent_options,
            session,
            inbox,
            scope.context.clone(),
            seekdeep_scope::scope_of(&scope.context).expect("scope"),
        ));
        agent.install_controller(Arc::new(RecordingController {
            messages: Arc::clone(&self.messages),
        }))?;
        self.agents.register(&scope.context, &agent, None)?;
        let dispose_failures = Arc::clone(&self.dispose_failures);
        Ok(AgentHandle::new(
            agent,
            Box::new(move || {
                let scope = Arc::clone(&scope);
                let dispose_failures = Arc::clone(&dispose_failures);
                Box::pin(async move {
                    scope.dispose().await?;
                    if dispose_failures
                        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                            remaining.checked_sub(1)
                        })
                        .is_ok()
                    {
                        anyhow::bail!("injected agent disposal failure");
                    }
                    Ok(())
                })
            }),
        ))
    }

    async fn resume(
        &self,
        _owner_context: &Context,
        _options: ResumeAgentOptions,
    ) -> anyhow::Result<AgentHandle> {
        anyhow::bail!("resume is unused")
    }
}

struct EmptyAdapter;

#[async_trait]
impl LlmAdapter for EmptyAdapter {
    fn stream(&self, _options: GenerateOptions) -> AdapterStream {
        AdapterStream::new(stream::empty())
    }
}

type NotificationLog = Arc<Mutex<Vec<(String, Map<String, Value>)>>>;

struct Harness {
    context: Context,
    sessions: Arc<SessionStore>,
    agents: Arc<AgentRegistry>,
    factory: Arc<TestFactory>,
    server: Arc<HarnessSdkJsonRpcServer>,
    notifications: NotificationLog,
}

impl Harness {
    fn new(options: HarnessSdkJsonRpcServerOptions) -> Self {
        Self::build(options, true)
    }

    fn without_llm(options: HarnessSdkJsonRpcServerOptions) -> Self {
        Self::build(options, false)
    }

    fn build(options: HarnessSdkJsonRpcServerOptions, install_llm: bool) -> Self {
        let context = Context::new();
        let sessions = SessionStore::install(&context).unwrap();
        let agents = Arc::new(AgentRegistry::new(context.clone()));
        agents.provide(&context).unwrap();
        let factory = Arc::new(TestFactory {
            sessions: Arc::clone(&sessions),
            agents: Arc::clone(&agents),
            creates: AtomicUsize::new(0),
            failures: AtomicUsize::new(0),
            options: Arc::new(Mutex::new(Vec::new())),
            messages: Arc::new(Mutex::new(Vec::new())),
            dispose_failures: Arc::new(AtomicUsize::new(0)),
        });
        agents.set_factory(factory.clone()).unwrap();
        if install_llm {
            let llm = LlmRuntime::install(&context).unwrap();
            llm.register_adapter(&["mock".to_owned()], Arc::new(EmptyAdapter))
                .unwrap();
        }
        let (server_io, client_io) = tokio::io::duplex(256 * 1024);
        let (server_read, server_write) = tokio::io::split(server_io);
        let (client_read, client_write) = tokio::io::split(client_io);
        let server_transport = JsonRpcLineTransport::new(server_read, server_write);
        let client = JsonRpcLineTransport::new(client_read, client_write);
        let notifications = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&notifications);
        client.on_notification(Arc::new(move |method, params| {
            observed.lock().push((method, params));
        }));
        client.start();
        let server = HarnessSdkJsonRpcServer::new(&context, &server_transport, options).unwrap();
        server_transport.on_request({
            let server = Arc::clone(&server);
            Arc::new(move |method, params| {
                let server = Arc::clone(&server);
                Box::pin(async move { server.handle_request(&method, params).await })
            })
        });
        server_transport.start();
        Self {
            context,
            sessions,
            agents,
            factory,
            server,
            notifications,
        }
    }

    async fn initialize(&self, cwd: &str) {
        self.server
            .initialize(InitializeParams {
                cwd: cwd.to_owned(),
                provider: seekdeep_llm::ProviderId::new("mock"),
                model: seekdeep_llm::ModelId::new("model"),
                max_tokens: Some(123),
            })
            .await
            .unwrap();
    }
}

fn runtime_context() -> Context {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let factory = Arc::new(TestFactory {
        sessions,
        agents: Arc::clone(&agents),
        creates: AtomicUsize::new(0),
        failures: AtomicUsize::new(0),
        options: Arc::new(Mutex::new(Vec::new())),
        messages: Arc::new(Mutex::new(Vec::new())),
        dispose_failures: Arc::new(AtomicUsize::new(0)),
    });
    agents.set_factory(factory).unwrap();
    let llm = LlmRuntime::install(&context).unwrap();
    llm.register_adapter(&["mock".to_owned()], Arc::new(EmptyAdapter))
        .unwrap();
    context
}

fn prompt(session: &str, text: &str) -> SessionPromptParams {
    SessionPromptParams {
        session_id: SessionId::new(session),
        content_blocks: vec![seekdeep_llm::ContentBlock::Text {
            text: text.to_owned(),
        }],
    }
}

#[test]
fn plugin_shape_is_namespace_safe_and_requires_only_agents() {
    let definition = plugin();
    assert_eq!(definition.name(), NAME);
    assert_eq!(definition.inject(), INJECT);
    assert_eq!(NAME, "sdk-jsonrpc-server");
    assert_eq!(INJECT, ["agents"]);
    assert_eq!(
        success_status(
            SubagentStopReason::Completed,
            HarnessSdkJsonRpcServerOptions::default()
        ),
        seekdeep_sdk_protocol::SdkRunStatus::Ok
    );
    for reason in [
        SubagentStopReason::Aborted,
        SubagentStopReason::Error,
        SubagentStopReason::Refusal,
        SubagentStopReason::MaxTokens,
    ] {
        assert_eq!(
            success_status(reason, HarnessSdkJsonRpcServerOptions::default()),
            seekdeep_sdk_protocol::SdkRunStatus::Error
        );
    }
    assert_eq!(
        success_status(
            SubagentStopReason::MaxTokens,
            HarnessSdkJsonRpcServerOptions {
                max_tokens_as_success: true
            }
        ),
        seekdeep_sdk_protocol::SdkRunStatus::Ok
    );
}

#[tokio::test]
async fn initializes_creates_coalesces_sessions_and_rejects_stale_agents() {
    let harness = Harness::new(HarnessSdkJsonRpcServerOptions::default());
    let cwd = tempfile::tempdir().unwrap();
    harness.initialize(&cwd.path().to_string_lossy()).await;
    let (first, second, other) = tokio::join!(
        harness.server.prompt(prompt("same", "one")),
        harness.server.prompt(prompt("same", "two")),
        harness.server.prompt(prompt("other", "three")),
    );
    assert!(first.is_ok() && second.is_ok() && other.is_ok());
    assert_eq!(harness.factory.creates.load(Ordering::Acquire), 2);
    assert_eq!(harness.factory.messages.lock().len(), 3);
    {
        let options = harness.factory.options.lock();
        assert!(options.iter().all(|options| {
            options.meta.cwd.as_deref() == Some(&cwd.path().to_string_lossy())
                && options
                    .agent_options
                    .provider
                    .as_ref()
                    .map(ProviderId::as_str)
                    == Some("mock")
                && options.agent_options.model.as_ref().map(ModelId::as_str) == Some("model")
                && options.agent_options.max_tokens == Some(123)
        }));
    }

    let live = harness.agents.get(&SessionId::new("same")).unwrap();
    let scope = live.context().fiber().clone();
    scope.dispose().await.unwrap();
    let error = harness
        .server
        .prompt(prompt("same", "after-dispose"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("disposed outside the server"));
    harness.server.shutdown().await.unwrap();
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn forwards_session_status_lineage_and_local_subagent_terminal_notifications() {
    let harness = Harness::new(HarnessSdkJsonRpcServerOptions {
        max_tokens_as_success: true,
    });
    let cwd = tempfile::tempdir().unwrap();
    harness.initialize(&cwd.path().to_string_lossy()).await;
    harness
        .server
        .prompt(prompt("parent", "hello"))
        .await
        .unwrap();
    let parent = harness.agents.get(&SessionId::new("parent")).unwrap();
    parent
        .session()
        .append(
            "fixture/event",
            json!({"ok":true}),
            AppendOptions::default(),
        )
        .unwrap();
    let routed = scope_target(&harness.context, Some(parent.scope_key()));
    routed
        .events()
        .emit(
            &routed,
            "agent/status",
            &EventArgs::one(AgentEvent {
                agent: Arc::clone(&parent),
                payload: AgentStatusChanged {
                    status: AgentStatus::Running,
                },
            }),
        )
        .unwrap();
    harness
        .sessions
        .create(
            &harness.context,
            Some(SessionId::new("child")),
            CreateSessionOptions {
                parent_session: Some(SessionId::new("parent")),
                ..CreateSessionOptions::default()
            },
        )
        .unwrap();
    routed
        .events()
        .emit(
            &routed,
            "subagent/end",
            &EventArgs::one(SubagentRunEndInfo {
                run_id: SubagentRunId::new("remote-collision"),
                provider: "remote".to_owned(),
                id: SessionId::new("child"),
                local: false,
                stop_reason: SubagentStopReason::Completed,
                last_assistant_message: Some(vec![seekdeep_llm::ContentBlock::Text {
                    text: "must be ignored".to_owned(),
                }]),
            }),
        )
        .unwrap();
    routed
        .events()
        .emit(
            &routed,
            "subagent/end",
            &EventArgs::one(SubagentRunEndInfo {
                run_id: SubagentRunId::new("run"),
                provider: "local".to_owned(),
                id: SessionId::new("child"),
                local: true,
                stop_reason: SubagentStopReason::MaxTokens,
                last_assistant_message: None,
            }),
        )
        .unwrap();
    for _ in 0..20 {
        if harness.notifications.lock().len() >= 4 {
            break;
        }
        tokio::task::yield_now().await;
    }
    {
        let notifications = harness.notifications.lock();
        assert!(
            notifications
                .iter()
                .any(|(method, _)| method == "session.event")
        );
        assert!(notifications.iter().any(|(method, params)| {
            method == "session.status" && params.get("status") == Some(&json!("running"))
        }));
        assert!(notifications.iter().any(|(method, params)| {
            method == "subagent.started" && params.get("childSessionId") == Some(&json!("child"))
        }));
        assert!(notifications.iter().any(|(method, params)| {
            method == "subagent.finished" && params.get("status") == Some(&json!("ok"))
        }));
        assert_eq!(
            notifications
                .iter()
                .filter(|(method, _)| method == "subagent.finished")
                .count(),
            1
        );
    }
    harness.server.shutdown().await.unwrap();
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn raw_requests_validate_shapes_unknown_methods_and_idempotent_shutdown() {
    let harness = Harness::new(HarnessSdkJsonRpcServerOptions::default());
    let invalid = harness
        .server
        .handle_request(
            "initialize",
            Map::from_iter([
                ("cwd".to_owned(), json!(".")),
                ("provider".to_owned(), json!("mock")),
                ("model".to_owned(), json!("m")),
                ("maxTokens".to_owned(), json!(1.5)),
            ]),
        )
        .await
        .unwrap_err();
    assert!(invalid.to_string().contains("positive safe integer"));
    let unknown = harness
        .server
        .handle_request("does/not/exist", Map::new())
        .await
        .unwrap_err();
    assert_eq!(
        unknown.to_string(),
        "unknown SeekDeep Harness SDK runtime method: does/not/exist"
    );
    let (first, second) = tokio::join!(harness.server.shutdown(), harness.server.shutdown());
    assert_eq!(first.unwrap(), Map::new());
    assert_eq!(second.unwrap(), Map::new());
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn shutdown_settles_every_agent_and_aggregates_all_disposal_failures() {
    let harness = Harness::new(HarnessSdkJsonRpcServerOptions::default());
    let cwd = tempfile::tempdir().unwrap();
    harness.initialize(&cwd.path().to_string_lossy()).await;
    harness.server.prompt(prompt("one", "one")).await.unwrap();
    harness.server.prompt(prompt("two", "two")).await.unwrap();
    harness
        .factory
        .dispose_failures
        .store(2, Ordering::Release);
    let error = harness.server.shutdown().await.unwrap_err();
    let message = error.to_string();
    assert!(message.starts_with("SDK server teardown failed:"), "{message}");
    assert_eq!(message.matches("injected agent disposal failure").count(), 2);
    assert!(harness.agents.list().is_empty());
    assert_eq!(
        harness.server.shutdown().await.unwrap_err().to_string(),
        message
    );
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn rejects_unowned_provider_and_retries_failed_session_creation() {
    let harness = Harness::new(HarnessSdkJsonRpcServerOptions::default());
    assert!(harness.server.has_adapter_for("mock"));
    assert!(!harness.server.has_adapter_for("private"));
    let error = harness
        .server
        .initialize(InitializeParams {
            cwd: ".".to_owned(),
            provider: ProviderId::new("private"),
            model: ModelId::new("model"),
            max_tokens: None,
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "no adapter registered for provider \"private\""
    );
    harness
        .server
        .initialize(InitializeParams {
            cwd: ".".to_owned(),
            provider: ProviderId::new("mock"),
            model: ModelId::new("model"),
            max_tokens: None,
        })
        .await
        .unwrap();
    harness.factory.failures.store(1, Ordering::Release);
    assert!(
        harness
            .server
            .prompt(prompt("retry", "first"))
            .await
            .is_err()
    );
    assert!(
        harness
            .server
            .prompt(prompt("retry", "second"))
            .await
            .is_ok()
    );
    assert_eq!(harness.factory.creates.load(Ordering::Acquire), 2);
    let expected = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        harness
            .factory
            .options
            .lock()
            .last()
            .unwrap()
            .meta
            .cwd
            .as_deref(),
        Some(expected.as_str())
    );
    harness.server.shutdown().await.unwrap();
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn failed_initialize_keeps_the_attempted_route_and_absent_llm_reports_no_adapter() {
    let harness = Harness::new(HarnessSdkJsonRpcServerOptions::default());
    let error = harness
        .server
        .initialize(InitializeParams {
            cwd: "attempted/relative".to_owned(),
            provider: ProviderId::new("private"),
            model: ModelId::new("attempted-model"),
            max_tokens: Some(77),
        })
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "no adapter registered for provider \"private\""
    );
    harness
        .server
        .prompt(prompt("after-failed-initialize", "still configured"))
        .await
        .unwrap();
    let options = harness.factory.options.lock();
    let attempted = options.last().unwrap();
    assert_eq!(
        attempted.agent_options.provider.as_ref().map(ProviderId::as_str),
        Some("private")
    );
    assert_eq!(
        attempted.agent_options.model.as_ref().map(ModelId::as_str),
        Some("attempted-model")
    );
    assert_eq!(attempted.agent_options.max_tokens, Some(77));
    assert!(
        attempted
            .meta
            .cwd
            .as_deref()
            .is_some_and(|cwd| cwd.ends_with("attempted/relative"))
    );
    drop(options);
    harness.server.shutdown().await.unwrap();
    harness.context.fiber().dispose().await.unwrap();

    let harness = Harness::without_llm(HarnessSdkJsonRpcServerOptions::default());
    assert!(!harness.server.has_adapter_for("anything"));
    let error = harness
        .server
        .initialize(InitializeParams {
            cwd: ".".to_owned(),
            provider: ProviderId::new("private"),
            model: ModelId::new("model"),
            max_tokens: None,
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no adapter registered"));
    harness.server.shutdown().await.unwrap();
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn transport_apply_answers_shutdown_before_dispose_and_exits_once() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let factory = Arc::new(TestFactory {
        sessions,
        agents: Arc::clone(&agents),
        creates: AtomicUsize::new(0),
        failures: AtomicUsize::new(0),
        options: Arc::new(Mutex::new(Vec::new())),
        messages: Arc::new(Mutex::new(Vec::new())),
        dispose_failures: Arc::new(AtomicUsize::new(0)),
    });
    agents.set_factory(factory).unwrap();
    let llm = LlmRuntime::install(&context).unwrap();
    llm.register_adapter(&["mock".to_owned()], Arc::new(EmptyAdapter))
        .unwrap();
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);
    let exits = Arc::new(AtomicUsize::new(0));
    let exit_count = Arc::clone(&exits);
    apply_with_runtime(
        &context,
        Config::default(),
        ServerRuntime {
            input: Box::pin(server_read),
            output: Box::pin(server_write),
            exit: Arc::new(move |code| {
                assert_eq!(code, 0);
                exit_count.fetch_add(1, Ordering::AcqRel);
            }),
        },
    )
    .unwrap();
    let client = JsonRpcLineTransport::new(client_read, client_write);
    client.start();
    let (first, second) = tokio::join!(
        client.request("shutdown", Map::new(), None),
        client.request("shutdown", Map::new(), None),
    );
    assert_eq!(first.unwrap(), json!({}));
    assert_eq!(second.unwrap(), json!({}));
    for _ in 0..50 {
        if exits.load(Ordering::Acquire) == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(exits.load(Ordering::Acquire), 1);
    client.close();
}

#[tokio::test]
async fn transport_apply_exits_after_flush_failure_but_bare_dispose_never_exits() {
    let context = runtime_context();
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);
    let flushes = Arc::new(AtomicUsize::new(0));
    let exits = Arc::new(AtomicUsize::new(0));
    let exit_count = Arc::clone(&exits);
    apply_with_runtime(
        &context,
        Config::default(),
        ServerRuntime {
            input: Box::pin(server_read),
            output: Box::pin(FailingFlush {
                inner: server_write,
                flushes: Arc::clone(&flushes),
            }),
            exit: Arc::new(move |code| {
                assert_eq!(code, 0);
                exit_count.fetch_add(1, Ordering::AcqRel);
            }),
        },
    )
    .unwrap();
    let client = JsonRpcLineTransport::new(client_read, client_write);
    client.start();
    assert_eq!(
        client.request("shutdown", Map::new(), None).await.unwrap(),
        json!({})
    );
    for _ in 0..100 {
        if exits.load(Ordering::Acquire) == 1 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(flushes.load(Ordering::Acquire), 1);
    assert_eq!(exits.load(Ordering::Acquire), 1);
    client.close();

    let context = runtime_context();
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let (server_read, server_write) = tokio::io::split(server_io);
    let (client_read, client_write) = tokio::io::split(client_io);
    let exits = Arc::new(AtomicUsize::new(0));
    let exit_count = Arc::clone(&exits);
    apply_with_runtime(
        &context,
        Config::default(),
        ServerRuntime {
            input: Box::pin(server_read),
            output: Box::pin(server_write),
            exit: Arc::new(move |_| {
                exit_count.fetch_add(1, Ordering::AcqRel);
            }),
        },
    )
    .unwrap();
    let client = JsonRpcLineTransport::new(client_read, client_write);
    client.start();
    context.fiber().dispose().await.unwrap();
    assert_eq!(exits.load(Ordering::Acquire), 0);
    client.close();
}
