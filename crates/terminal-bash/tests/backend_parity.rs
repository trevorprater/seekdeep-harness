//! Backend startup, confinement, environment, plugin, and rollback parity.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_cordis::{Context, Fiber};
use seekdeep_core::{
    session::{Session, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_llm::AbortSignal;
use seekdeep_sandbox::{
    ConfinedArgv, SandboxEnforcement, SandboxPolicy, SandboxProvider, SandboxService,
    SandboxUnavailableError,
};
use seekdeep_sandbox_policy::{SandboxPolicyConfig, install as install_policy, set_sandbox_mode};
use seekdeep_scope::ScopeKey;
use seekdeep_subprocess::{
    ProcessGroupId, ProcessId, SubprocessOutcome, SubprocessOutput, SubprocessTerminalForeground,
    SubprocessTerminalHandle, SubprocessTerminalHandleRef, SubprocessTerminalSignal,
};
use seekdeep_terminal::{
    TerminalBackend, TerminalBackendCleanupError, TerminalBackendSession, TerminalBackendSpawnSpec,
    TerminalFailure, TerminalReadRequest, TerminalReadResult, TerminalResult,
    TerminalSendOperationRef, TerminalSendRequest, TerminalSessionId, TerminalSessionService,
    TerminalSessionStatus, TerminalSignal, TerminalSignalResult, TerminalSpawnRequest,
};
use seekdeep_terminal_bash::{
    BashPtySession, BashTerminalBackend, ResolvedTerminalBashConfig, TerminalBashConfig,
    child_environment, plugin,
};

#[derive(Debug)]
struct Completion<T> {
    value: Mutex<Option<T>>,
    notify: tokio::sync::Notify,
}

impl<T> Default for Completion<T> {
    fn default() -> Self {
        Self {
            value: Mutex::new(None),
            notify: tokio::sync::Notify::new(),
        }
    }
}

impl<T: Clone> Completion<T> {
    fn resolve(&self, value: T) {
        let mut stored = self.value.lock();
        if stored.is_none() {
            *stored = Some(value);
            self.notify.notify_waiters();
        }
    }

    async fn wait(&self) -> T {
        loop {
            let notified = self.notify.notified();
            if let Some(value) = self.value.lock().clone() {
                return value;
            }
            notified.await;
        }
    }
}

struct DummyTerminal {
    output: SubprocessOutput,
    outcome: Arc<Completion<Result<SubprocessOutcome, String>>>,
}

impl fmt::Debug for DummyTerminal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DummyTerminal")
    }
}

impl DummyTerminal {
    fn new() -> Arc<Self> {
        let (_writer, reader) = tokio::io::duplex(1024);
        Arc::new(Self {
            output: Arc::new(tokio::sync::Mutex::new(Box::pin(reader))),
            outcome: Arc::new(Completion::default()),
        })
    }
}

#[async_trait]
impl SubprocessTerminalHandle for DummyTerminal {
    fn pid(&self) -> ProcessId {
        ProcessId::new(123)
    }

    fn output(&self) -> SubprocessOutput {
        self.output.clone()
    }

    async fn done(&self) -> anyhow::Result<SubprocessOutcome> {
        self.outcome
            .wait()
            .await
            .map_err(|message| anyhow::anyhow!(message))
    }

    async fn write(&self, _data: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn inspect_foreground(&self) -> anyhow::Result<Option<SubprocessTerminalForeground>> {
        Ok(Some(SubprocessTerminalForeground {
            process_group_id: ProcessGroupId::new(123),
            input_waiting: true,
        }))
    }

    async fn signal_foreground(
        &self,
        _signal: SubprocessTerminalSignal,
    ) -> anyhow::Result<ProcessGroupId> {
        Ok(ProcessGroupId::new(123))
    }

    async fn terminate(&self) -> anyhow::Result<()> {
        self.outcome.resolve(Ok(SubprocessOutcome {
            exit_code: None,
            signal: Some(seekdeep_subprocess::ProcessSignal::new("SIGTERM")),
        }));
        Ok(())
    }
}

#[derive(Debug)]
struct StubSession {
    initialize: Arc<Completion<Result<(), TerminalFailure>>>,
    initialize_immediately: Mutex<Option<Result<(), TerminalFailure>>>,
    close_result: Mutex<Result<(), TerminalFailure>>,
    close_calls: AtomicUsize,
}

impl StubSession {
    fn ready() -> Arc<Self> {
        Arc::new(Self {
            initialize: Arc::new(Completion::default()),
            initialize_immediately: Mutex::new(Some(Ok(()))),
            close_result: Mutex::new(Ok(())),
            close_calls: AtomicUsize::new(0),
        })
    }

    fn startup_failure(message: &str, cleanup: Option<&str>) -> Arc<Self> {
        Arc::new(Self {
            initialize: Arc::new(Completion::default()),
            initialize_immediately: Mutex::new(Some(Err(TerminalFailure::message(message)))),
            close_result: Mutex::new(
                cleanup.map_or(Ok(()), |message| Err(TerminalFailure::message(message))),
            ),
            close_calls: AtomicUsize::new(0),
        })
    }

    fn stalled() -> Arc<Self> {
        Arc::new(Self {
            initialize: Arc::new(Completion::default()),
            initialize_immediately: Mutex::new(None),
            close_result: Mutex::new(Ok(())),
            close_calls: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl BashPtySession for StubSession {
    async fn initialize(&self, _signal: Option<AbortSignal>) -> TerminalResult<()> {
        let immediate = self.initialize_immediately.lock().take();
        match immediate {
            Some(result) => result,
            None => self.initialize.wait().await,
        }
    }
}

#[async_trait]
impl TerminalBackendSession for StubSession {
    fn motd(&self) -> String {
        String::new()
    }

    fn pid(&self) -> Option<ProcessId> {
        Some(ProcessId::new(123))
    }

    fn start_send(
        &self,
        _request: TerminalSendRequest,
    ) -> TerminalResult<TerminalSendOperationRef> {
        Err(TerminalFailure::message("unused"))
    }

    fn read(&self, _request: TerminalReadRequest) -> TerminalResult<TerminalReadResult> {
        Err(TerminalFailure::message("unused"))
    }

    async fn signal(&self, _signal: TerminalSignal) -> TerminalResult<TerminalSignalResult> {
        Ok(TerminalSignalResult::delivered(ProcessGroupId::new(123)))
    }

    fn status(&self) -> TerminalSessionStatus {
        TerminalSessionStatus::Running
    }

    async fn close(&self, reason: &str) -> TerminalResult<()> {
        assert!(!reason.is_empty());
        self.close_calls.fetch_add(1, Ordering::AcqRel);
        self.close_result.lock().clone()
    }
}

#[derive(Debug, Default)]
struct RecordingSandbox {
    calls: Mutex<Vec<(Vec<String>, SandboxPolicy)>>,
    empty: bool,
}

impl SandboxProvider for RecordingSandbox {
    fn confine(&self, argv: &[String], policy: &SandboxPolicy) -> anyhow::Result<ConfinedArgv> {
        self.calls.lock().push((argv.to_vec(), policy.clone()));
        Ok(ConfinedArgv {
            argv: if self.empty {
                Vec::new()
            } else {
                [vec!["/sandbox".into(), "--".into()], argv.to_vec()].concat()
            },
            enforcement: SandboxEnforcement::Full,
            denial_signatures: Vec::new(),
            runner_failure_rules: Vec::new(),
        })
    }
}

#[derive(Debug)]
struct UnavailableSandbox;

impl SandboxProvider for UnavailableSandbox {
    fn confine(&self, _argv: &[String], policy: &SandboxPolicy) -> anyhow::Result<ConfinedArgv> {
        Err(anyhow::Error::new(SandboxUnavailableError::new(
            policy.mode,
            None,
        )))
    }
}

fn config() -> ResolvedTerminalBashConfig {
    let config = TerminalBashConfig {
        shell_args: vec!["-i".into()],
        rows: 24.0,
        cols: 80.0,
        scrollback_lines: 10.0,
        scrollback_max_bytes: 100.0,
        max_read_bytes: 50.0,
        poll_interval_ms: 10.0,
        exact_probe_after_ms: 20.0,
        idle_silence_ms: 50.0,
        handoff_grace_ms: 10.0,
        timeout_ms: 100.0,
        dispose_grace_ms: 10.0,
        ..TerminalBashConfig::default()
    };
    config.resolve().unwrap()
}

fn owner(context: &Context, id: &str) -> Arc<Agent> {
    let id = SessionId::new(id);
    let session = Session::create(&id, None, None).unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ))
}

fn spec(owner: Arc<Agent>, signal: Option<AbortSignal>) -> TerminalBackendSpawnSpec {
    TerminalBackendSpawnSpec {
        session_id: TerminalSessionId::new("pty-1"),
        owner,
        terminal_type: "shell".into(),
        name: None,
        cwd: None,
        signal,
    }
}

async fn base_context(
    mode: seekdeep_sandbox::SandboxMode,
) -> (Context, Arc<TerminalSessionService>) {
    let context = Context::new();
    let terminals = TerminalSessionService::install(&context).await.unwrap();
    install_policy(
        &context,
        SandboxPolicyConfig {
            mode,
            workspace_root: Some("/workspace".into()),
        },
    )
    .unwrap();
    (context, terminals)
}

#[test]
fn child_environment_is_deliberate_complete_and_renamed() {
    let context = Context::new();
    let environment = child_environment(&spec(owner(&context, "agent"), None));
    assert_eq!(
        environment,
        BTreeMap::from([
            ("BASH_SILENCE_DEPRECATION_WARNING".into(), "1".into()),
            ("GIT_PAGER".into(), "cat".into()),
            ("PAGER".into(), "cat".into()),
            (
                "PROMPT_COMMAND".into(),
                "printf \"\\033]133;D;%s\\007\" \"$?\"".into()
            ),
            ("PS1".into(), "seekdeep> ".into()),
            ("SEEKDEEP_PTY_SESSION_ID".into(), "pty-1".into()),
            ("SEEKDEEP_SESSION_ID".into(), "agent".into()),
            ("SEEKDEEP_SHELL".into(), "1".into()),
            ("TERM".into(), "dumb".into()),
        ])
    );
    assert!(!environment.contains_key("DSH_SESSION_ID"));
}

#[tokio::test]
async fn rejects_preabort_empty_argv_and_missing_confined_provider_before_allocation() {
    let (context, _) = base_context(seekdeep_sandbox::SandboxMode::ReadOnly).await;
    let empty = Arc::new(RecordingSandbox {
        empty: true,
        ..RecordingSandbox::default()
    });
    SandboxService::new(empty).provide(&context).unwrap();
    let allocations = Arc::new(AtomicUsize::new(0));
    let session = StubSession::ready();
    let backend = BashTerminalBackend::with_factories(
        context.clone(),
        config(),
        {
            let allocations = allocations.clone();
            move |_| {
                allocations.fetch_add(1, Ordering::AcqRel);
                let terminal: SubprocessTerminalHandleRef = DummyTerminal::new();
                async move { Ok(terminal) }
            }
        },
        move |_, _| session.clone(),
    );
    let signal = AbortSignal::default();
    let reason = Arc::new(TerminalFailure::message("spawn aborted"));
    signal.abort_with_typed_reason(
        reason.clone(),
        serde_json::json!({"message": "spawn aborted"}),
    );
    let error = backend
        .spawn(spec(owner(&context, "agent-abort"), Some(signal)))
        .await
        .unwrap_err();
    assert!(error.ptr_eq(&reason));
    assert_eq!(allocations.load(Ordering::Acquire), 0);

    let error = backend
        .spawn(spec(owner(&context, "agent-empty"), None))
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "terminal-bash: sandbox returned empty argv"
    );
    assert_eq!(allocations.load(Ordering::Acquire), 0);

    let (unavailable, _) = base_context(seekdeep_sandbox::SandboxMode::ReadOnly).await;
    SandboxService::new(Arc::new(UnavailableSandbox))
        .provide(&unavailable)
        .unwrap();
    let backend = BashTerminalBackend::with_factories(
        unavailable.clone(),
        config(),
        |_| async { anyhow::bail!("must not allocate") },
        |_, _| StubSession::ready(),
    );
    let error = backend
        .spawn(spec(owner(&unavailable, "agent-unavailable"), None))
        .await
        .unwrap_err();
    let unavailable_error = error.downcast_ref::<SandboxUnavailableError>().unwrap();
    assert_eq!(unavailable_error.name(), "SandboxUnavailableError");
    assert_eq!(unavailable_error.code(), "SANDBOX_UNAVAILABLE");

    let (missing, _) = base_context(seekdeep_sandbox::SandboxMode::WorkspaceWrite).await;
    let backend = BashTerminalBackend::with_factories(
        missing.clone(),
        config(),
        |_| async { anyhow::bail!("must not allocate") },
        |_, _| StubSession::ready(),
    );
    let error = backend
        .spawn(spec(owner(&missing, "agent-missing"), None))
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "terminal-bash: sandbox mode \"workspace-write\" requires a ctx.sandbox provider in the execution world"
    );
}

#[tokio::test]
async fn wraps_exact_argv_policy_spawn_shape_and_returns_initialized_session() {
    let (context, _) = base_context(seekdeep_sandbox::SandboxMode::WorkspaceWrite).await;
    let sandbox = Arc::new(RecordingSandbox::default());
    SandboxService::new(sandbox.clone())
        .provide(&context)
        .unwrap();
    let spawned = Arc::new(Mutex::new(None));
    let returned = StubSession::ready();
    let backend = BashTerminalBackend::with_factories(
        context.clone(),
        config(),
        {
            let spawned = spawned.clone();
            move |spec| {
                *spawned.lock() = Some(spec);
                let terminal: SubprocessTerminalHandleRef = DummyTerminal::new();
                async move { Ok(terminal) }
            }
        },
        {
            let returned = returned.clone();
            move |_, _| returned.clone()
        },
    );
    let mut spawn_spec = spec(owner(&context, "agent-policy"), None);
    spawn_spec.cwd = Some("/work".into());
    let result = backend.spawn(spawn_spec).await.unwrap();
    assert!(Arc::ptr_eq(
        &result,
        &(returned.clone() as Arc<dyn TerminalBackendSession>)
    ));
    let spawned = spawned.lock().clone().unwrap();
    assert_eq!(spawned.argv, vec!["/sandbox", "--", "/bin/bash", "-i"]);
    assert_eq!(spawned.cwd, std::path::PathBuf::from("/work"));
    assert_eq!(spawned.rows, 24);
    assert_eq!(spawned.cols, 80);
    assert!((spawned.grace_ms - 10.0).abs() < f64::EPSILON);
    assert_eq!(
        spawned
            .env
            .as_ref()
            .unwrap()
            .get("SEEKDEEP_SESSION_ID")
            .map(String::as_str),
        Some("agent-policy")
    );
    let calls = sandbox.calls.lock();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, vec!["/bin/bash", "-i"]);
    assert_eq!(
        calls[0].1.mode,
        seekdeep_sandbox::ConfinedSandboxMode::WorkspaceWrite
    );
    assert_eq!(
        calls[0].1.workspace_root,
        std::path::PathBuf::from("/workspace")
    );
    assert_eq!(
        calls[0].1.session_id.as_ref().map(SessionId::as_str),
        Some("agent-policy")
    );
}

#[tokio::test]
async fn startup_failure_closes_and_cleanup_failure_retains_both_identities() {
    let (context, _) = base_context(seekdeep_sandbox::SandboxMode::DangerFullAccess).await;
    let terminal = || {
        let terminal: SubprocessTerminalHandleRef = DummyTerminal::new();
        async move { Ok(terminal) }
    };
    let failed = StubSession::startup_failure("startup failed", None);
    let backend =
        BashTerminalBackend::with_factories(context.clone(), config(), move |_| terminal(), {
            let failed = failed.clone();
            move |_, _| failed.clone()
        });
    let error = backend
        .spawn(spec(owner(&context, "agent-failed"), None))
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "startup failed");
    assert_eq!(failed.close_calls.load(Ordering::Acquire), 1);

    let double = StubSession::startup_failure("startup failed", Some("cleanup failed"));
    let backend =
        BashTerminalBackend::with_factories(context.clone(), config(), move |_| terminal(), {
            let double = double.clone();
            move |_, _| double.clone()
        });
    let error = backend
        .spawn(spec(owner(&context, "agent-double"), None))
        .await
        .unwrap_err();
    let aggregate = error.downcast_ref::<TerminalBackendCleanupError>().unwrap();
    assert_eq!(aggregate.spawn_error.to_string(), "startup failed");
    assert_eq!(aggregate.cleanup_error.to_string(), "cleanup failed");
}

#[tokio::test]
async fn cancellation_wins_stalled_initialization_and_starts_rollback_without_waiting_for_it() {
    let (context, _) = base_context(seekdeep_sandbox::SandboxMode::DangerFullAccess).await;
    let stalled = StubSession::stalled();
    let backend = BashTerminalBackend::with_factories(
        context.clone(),
        config(),
        |_| {
            let terminal: SubprocessTerminalHandleRef = DummyTerminal::new();
            async move { Ok(terminal) }
        },
        {
            let stalled = stalled.clone();
            move |_, _| stalled.clone()
        },
    );
    let signal = AbortSignal::default();
    let spawning = tokio::spawn({
        let backend = backend.clone();
        let owner = owner(&context, "agent-stalled");
        let signal = signal.clone();
        async move { backend.spawn(spec(owner, Some(signal))).await }
    });
    tokio::task::yield_now().await;
    let reason = Arc::new(TerminalFailure::message("cancel stalled startup"));
    signal.abort_with_typed_reason(reason.clone(), serde_json::json!("cancel stalled startup"));
    let error = spawning.await.unwrap().unwrap_err();
    assert!(error.ptr_eq(&reason));
    assert_eq!(stalled.close_calls.load(Ordering::Acquire), 1);
    stalled.initialize.resolve(Ok(()));
}

#[tokio::test]
async fn plugin_shape_validation_and_backend_registration_are_reversible() {
    let plugin = plugin();
    assert_eq!(plugin.name(), "terminal-bash");
    assert_eq!(
        plugin.inject(),
        ["terminals", "sandboxPolicy", "subprocess"]
    );

    let context = Context::new();
    let terminals = TerminalSessionService::install(&context).await.unwrap();
    install_policy(
        &context,
        SandboxPolicyConfig {
            mode: seekdeep_sandbox::SandboxMode::DangerFullAccess,
            workspace_root: Some("/tmp".into()),
        },
    )
    .unwrap();
    // Dependency management is tested in Cordis; direct apply pins this
    // package's exact reversible registry contribution.
    let effect = seekdeep_terminal_bash::apply(&context, &TerminalBashConfig::default()).unwrap();
    assert_eq!(terminals.list_backends(), vec!["shell"]);
    effect.dispose().await.unwrap();
    assert!(terminals.list_backends().is_empty());
}

fn fenced_owner(
    context: &Context,
    sessions: &Arc<SessionStore>,
    registry: &Arc<AgentRegistry>,
    raw_id: &str,
) -> (Arc<Agent>, Arc<Fiber>) {
    let fiber = Fiber::active_child(format!("agent-{raw_id}"));
    let owner_context = context.with_fiber(fiber.clone());
    let id = SessionId::new(raw_id);
    let session = sessions
        .create(
            &owner_context,
            Some(id.clone()),
            CreateSessionOptions::default(),
        )
        .unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    let owner = Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session,
        inbox,
        owner_context.clone(),
        ScopeKey::new(),
    ));
    registry.register(&owner_context, &owner, None).unwrap();
    (owner, fiber)
}

fn register_stub_backend(
    context: &Context,
    terminals: &Arc<TerminalSessionService>,
    session: Arc<StubSession>,
    backend_type: &str,
) -> Arc<Fiber> {
    let fiber = Fiber::active_child(format!("provider-{backend_type}"));
    let provider_context = context.with_fiber(fiber.clone());
    let mut config = config();
    backend_type.clone_into(&mut config.backend_type);
    let backend = BashTerminalBackend::with_factories(
        context.clone(),
        config,
        |_| {
            let terminal: SubprocessTerminalHandleRef = DummyTerminal::new();
            async move { Ok(terminal) }
        },
        move |_, _| session.clone(),
    );
    let backend: Arc<dyn TerminalBackend> = backend;
    terminals
        .register_backend(&provider_context, &backend)
        .unwrap();
    fiber
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // One test preserves the source's cross-reload fence chronology.
async fn owner_lifetime_fence_survives_provider_reload_and_covers_unpublished_creation() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let registry = Arc::new(AgentRegistry::new(context.clone()));
    registry.provide(&context).unwrap();
    let terminals = TerminalSessionService::install(&context).await.unwrap();
    install_policy(
        &context,
        SandboxPolicyConfig {
            mode: seekdeep_sandbox::SandboxMode::DangerFullAccess,
            workspace_root: Some("/tmp".into()),
        },
    )
    .unwrap();

    let (owner, owner_fiber) = fenced_owner(&context, &sessions, &registry, "mode-owner");
    let provider = register_stub_backend(&context, &terminals, StubSession::ready(), "stub");
    let created = terminals
        .spawn(
            owner.clone(),
            TerminalSpawnRequest {
                terminal_type: "stub".into(),
                name: None,
                cwd: None,
            },
            None,
        )
        .await
        .unwrap();
    set_sandbox_mode(
        owner.session(),
        seekdeep_sandbox::SandboxMode::DangerFullAccess,
    )
    .unwrap();
    provider.dispose().await.unwrap();
    assert!(terminals.list_backends().is_empty());
    let error =
        set_sandbox_mode(owner.session(), seekdeep_sandbox::SandboxMode::ReadOnly).unwrap_err();
    assert!(error.to_string().contains(
        "cannot change sandbox mode from \"danger-full-access\" to \"read-only\" while persistent terminal sessions are open or being created"
    ));
    assert_eq!(
        owner
            .session()
            .events()
            .iter()
            .filter(|event| event.event_type == "sandbox/mode")
            .count(),
        1
    );

    let replacement = register_stub_backend(&context, &terminals, StubSession::ready(), "stub");
    replacement.dispose().await.unwrap();
    terminals
        .kill(&owner, &created.session_id, Some("test complete"))
        .await
        .unwrap();
    set_sandbox_mode(owner.session(), seekdeep_sandbox::SandboxMode::ReadOnly).unwrap();

    let (pending_owner, pending_fiber) =
        fenced_owner(&context, &sessions, &registry, "pending-mode-owner");
    let stalled = StubSession::stalled();
    let pending_provider =
        register_stub_backend(&context, &terminals, stalled.clone(), "pending-stub");
    let spawning = tokio::spawn({
        let terminals = terminals.clone();
        let pending_owner = pending_owner.clone();
        async move {
            terminals
                .spawn(
                    pending_owner,
                    TerminalSpawnRequest {
                        terminal_type: "pending-stub".into(),
                        name: None,
                        cwd: None,
                    },
                    None,
                )
                .await
        }
    });
    for _ in 0..20 {
        if terminals.has_owner_activity(&pending_owner) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(terminals.has_owner_activity(&pending_owner));
    assert!(
        set_sandbox_mode(
            pending_owner.session(),
            seekdeep_sandbox::SandboxMode::ReadOnly
        )
        .unwrap_err()
        .to_string()
        .contains("open or being created")
    );
    stalled.initialize.resolve(Ok(()));
    let pending = spawning.await.unwrap().unwrap();
    terminals
        .kill(&pending_owner, &pending.session_id, Some("test complete"))
        .await
        .unwrap();
    assert!(!terminals.has_owner_activity(&pending_owner));

    pending_provider.dispose().await.unwrap();
    pending_fiber.dispose().await.unwrap();
    owner_fiber.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}
