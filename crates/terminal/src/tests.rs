use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use futures::{FutureExt as _, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_cordis::{Context, Fiber};
use seekdeep_core::session::{Session, SessionId};
use seekdeep_scope::ScopeKey;
use seekdeep_subprocess::{ProcessGroupId, ProcessId};
use serde_json::json;

use super::*;

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

#[derive(Debug)]
struct StubOperation {
    completion: Arc<Completion<TerminalResult<TerminalSendResult>>>,
}

impl StubOperation {
    fn pending() -> Arc<Self> {
        Arc::new(Self {
            completion: Arc::new(Completion::default()),
        })
    }

    fn failed(message: &str) -> Arc<Self> {
        let operation = Self::pending();
        operation
            .completion
            .resolve(Err(TerminalFailure::message(message)));
        operation
    }
}

impl TerminalSendOperation for StubOperation {
    fn done(&self) -> BoxFuture<'static, TerminalResult<TerminalSendResult>> {
        let completion = self.completion.clone();
        async move { completion.wait().await }.boxed()
    }

    fn read_output(&self) -> TerminalSendRead {
        TerminalSendRead {
            delta: "delta".to_owned(),
            truncated: false,
        }
    }

    fn cancel(&self) -> bool {
        if self.completion.value.lock().is_some() {
            return false;
        }
        self.completion.resolve(Ok(TerminalSendResult {
            viewport: "done".to_owned(),
            wait_reason: TerminalWaitReason::StdinRead,
            session_status: TerminalSessionStatus::Running,
            truncated: false,
        }));
        true
    }
}

#[derive(Debug)]
struct StubSession {
    pid: Mutex<Option<ProcessId>>,
    closed: Mutex<Vec<String>>,
    status: Mutex<TerminalSessionStatus>,
    operation: Mutex<Option<Arc<StubOperation>>>,
    reject_send: AtomicBool,
    reject_close: AtomicBool,
    close_gate: Mutex<Option<Arc<Completion<()>>>>,
}

impl StubSession {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            pid: Mutex::new(Some(ProcessId::new(123))),
            closed: Mutex::new(Vec::new()),
            status: Mutex::new(TerminalSessionStatus::Running),
            operation: Mutex::new(None),
            reject_send: AtomicBool::new(false),
            reject_close: AtomicBool::new(false),
            close_gate: Mutex::new(None),
        })
    }

    fn close_gate(&self) -> Arc<Completion<()>> {
        let gate = Arc::new(Completion::default());
        *self.close_gate.lock() = Some(gate.clone());
        gate
    }
}

#[async_trait]
impl TerminalBackendSession for StubSession {
    fn motd(&self) -> String {
        "stub ready".to_owned()
    }

    fn pid(&self) -> Option<ProcessId> {
        *self.pid.lock()
    }

    fn start_send(
        &self,
        _request: TerminalSendRequest,
    ) -> TerminalResult<TerminalSendOperationRef> {
        let operation = if self.reject_send.load(Ordering::Acquire) {
            StubOperation::failed("send failed")
        } else {
            StubOperation::pending()
        };
        *self.operation.lock() = Some(operation.clone());
        Ok(operation)
    }

    fn read(&self, request: TerminalReadRequest) -> TerminalResult<TerminalReadResult> {
        Ok(TerminalReadResult {
            text: format!(
                "{}:{}",
                request.offset.unwrap_or_default(),
                request.count.unwrap_or_default()
            ),
            total_lines: 1,
            line_begin: 0,
            line_end: 1,
            truncated: false,
        })
    }

    async fn signal(&self, signal: TerminalSignal) -> TerminalResult<TerminalSignalResult> {
        Ok(TerminalSignalResult::delivered(ProcessGroupId::new(
            if signal == TerminalSignal::SIGINT {
                12
            } else {
                13
            },
        )))
    }

    fn status(&self) -> TerminalSessionStatus {
        self.status.lock().clone()
    }

    async fn close(&self, reason: &str) -> TerminalResult<()> {
        self.closed.lock().push(reason.to_owned());
        let gate = self.close_gate.lock().clone();
        if let Some(gate) = gate {
            gate.wait().await;
        }
        if self.reject_close.load(Ordering::Acquire) {
            return Err(TerminalFailure::message("close failed"));
        }
        *self.status.lock() = TerminalSessionStatus::Exited {
            exit_code: Some(0),
            signal: None,
        };
        if let Some(operation) = self.operation.lock().as_ref() {
            operation.cancel();
        }
        Ok(())
    }
}

type SpawnCallback = Arc<
    dyn Fn(
            TerminalBackendSpawnSpec,
        ) -> BoxFuture<'static, TerminalResult<TerminalBackendSessionRef>>
        + Send
        + Sync,
>;

struct CallbackBackend {
    backend_type: String,
    spawn_callback: SpawnCallback,
}

impl fmt::Debug for CallbackBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CallbackBackend")
            .field("backend_type", &self.backend_type)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl TerminalBackend for CallbackBackend {
    fn backend_type(&self) -> &str {
        &self.backend_type
    }

    async fn spawn(
        &self,
        spec: TerminalBackendSpawnSpec,
    ) -> TerminalResult<TerminalBackendSessionRef> {
        (self.spawn_callback)(spec).await
    }
}

fn callback_backend(
    backend_type: &str,
    callback: impl Fn(
        TerminalBackendSpawnSpec,
    ) -> BoxFuture<'static, TerminalResult<TerminalBackendSessionRef>>
    + Send
    + Sync
    + 'static,
) -> TerminalBackendRef {
    Arc::new(CallbackBackend {
        backend_type: backend_type.to_owned(),
        spawn_callback: Arc::new(callback),
    })
}

#[derive(Debug)]
struct StubBackend {
    backend_type: String,
    sessions: Mutex<Vec<Arc<StubSession>>>,
}

impl StubBackend {
    fn new(backend_type: &str) -> Arc<Self> {
        Arc::new(Self {
            backend_type: backend_type.to_owned(),
            sessions: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl TerminalBackend for StubBackend {
    fn backend_type(&self) -> &str {
        &self.backend_type
    }

    async fn spawn(
        &self,
        _spec: TerminalBackendSpawnSpec,
    ) -> TerminalResult<TerminalBackendSessionRef> {
        let session = StubSession::new();
        self.sessions.lock().push(session.clone());
        Ok(session)
    }
}

struct StubOwner {
    agent: Arc<Agent>,
    fiber: Arc<Fiber>,
}

struct Harness {
    context: Context,
    service_context: Context,
    service_fiber: Arc<Fiber>,
    registry: Arc<AgentRegistry>,
    service: Arc<TerminalSessionService>,
}

impl Harness {
    async fn new() -> Self {
        let context = Context::new();
        let registry = Arc::new(AgentRegistry::new(context.clone()));
        registry.provide(&context).expect("agent registry");
        let service_fiber = Fiber::active_child("terminal-service");
        let service_context = context.with_fiber(service_fiber.clone());
        let service = TerminalSessionService::install(&service_context)
            .await
            .expect("terminal service");
        Self {
            context,
            service_context,
            service_fiber,
            registry,
            service,
        }
    }

    fn owner(&self, id: &str) -> StubOwner {
        let fiber = Fiber::active_child(format!("agent-{id}"));
        let owner_context = self.context.with_fiber(fiber.clone());
        let id = SessionId::new(id);
        let session = Session::create(&id, None, None).expect("session");
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
        let agent = Arc::new(Agent::new(
            id,
            AgentOptions::default(),
            session,
            inbox,
            owner_context,
            ScopeKey::new(),
        ));
        StubOwner { agent, fiber }
    }

    fn register(&self, owner: &StubOwner) {
        self.registry
            .register(&self.context, &owner.agent, None)
            .expect("register agent");
    }

    fn register_backend<T>(&self, backend: Arc<T>) -> EffectHandle
    where
        T: TerminalBackend + 'static,
    {
        let backend: TerminalBackendRef = backend;
        self.service
            .register_backend(&self.service_context, &backend)
            .expect("register backend")
    }

    fn register_backend_ref(&self, backend: &TerminalBackendRef) -> EffectHandle {
        self.service
            .register_backend(&self.service_context, backend)
            .expect("register backend")
    }
}

fn request(backend_type: &str, name: Option<&str>) -> TerminalSpawnRequest {
    TerminalSpawnRequest {
        terminal_type: backend_type.to_owned(),
        name: name.map(str::to_owned),
        cwd: None,
    }
}

fn send_request() -> TerminalSendRequest {
    TerminalSendRequest {
        text: "echo hi".to_owned(),
        submit: true,
        signal: None,
    }
}

fn code(error: &TerminalFailure) -> Option<TerminalErrorCode> {
    error
        .downcast_ref::<TerminalError>()
        .map(TerminalError::code)
}

fn caller_abort(reason: &TerminalFailure) -> AbortSignal {
    let signal = AbortSignal::default();
    signal.abort_with_typed_reason(Arc::new(reason.clone()), json!(reason.to_string()));
    signal
}

fn spawn_task(
    service: Arc<TerminalSessionService>,
    owner: Arc<Agent>,
    spawn_request: TerminalSpawnRequest,
    signal: Option<AbortSignal>,
) -> tokio::task::JoinHandle<TerminalResult<TerminalSpawnResult>> {
    tokio::spawn(async move { service.spawn(owner, spawn_request, signal).await })
}

async fn yield_until(mut condition: impl FnMut() -> bool) {
    for _ in 0..100 {
        if condition() {
            return;
        }
        tokio::task::yield_now().await;
    }
    assert!(condition(), "condition did not become true");
}

#[tokio::test]
async fn backend_registry_preserves_brand_order_and_exact_contributions() {
    let harness = Harness::new().await;
    assert_eq!(TerminalSessionId::new("pty-1").as_str(), "pty-1");
    let first = StubBackend::new("stub");
    let effect = harness.register_backend(first.clone());
    assert_eq!(harness.service.list_backends(), ["stub"]);
    let duplicate: TerminalBackendRef = StubBackend::new("stub");
    let error = harness
        .service
        .register_backend(&harness.service_context, &duplicate)
        .expect_err("duplicate");
    assert_eq!(code(&error), Some(TerminalErrorCode::DuplicateBackend));

    let replacement: TerminalBackendRef = StubBackend::new("replacement");
    harness
        .service
        .state
        .lock()
        .backends
        .insert("stub".to_owned(), replacement);
    effect.dispose().await.expect("dispose old contribution");
    assert_eq!(harness.service.list_backends(), ["stub"]);
}

#[tokio::test]
async fn backend_registry_rejects_empty_types() {
    let harness = Harness::new().await;
    let empty: TerminalBackendRef = StubBackend::new("");
    let error = harness
        .service
        .register_backend(&harness.service_context, &empty)
        .expect_err("empty type");
    assert_eq!(error.to_string(), "pty backend type must be non-empty");
}

#[tokio::test]
async fn publication_and_every_operation_are_fenced_to_the_exact_owner() {
    let harness = Harness::new().await;
    let backend = StubBackend::new("stub");
    harness.register_backend(backend);
    let owner = harness.owner("owner");
    let foreign = harness.owner("foreign");
    harness.register(&owner);
    harness.register(&foreign);

    let created = harness
        .service
        .spawn(
            owner.agent.clone(),
            TerminalSpawnRequest {
                terminal_type: "stub".to_owned(),
                name: Some("main".to_owned()),
                cwd: Some("/tmp".to_owned()),
            },
            None,
        )
        .await
        .expect("spawn");
    assert_eq!(created.session_id, TerminalSessionId::new("pty-1"));
    assert_eq!(created.name.as_deref(), Some("main"));
    assert_eq!(created.pid, Some(ProcessId::new(123)));
    assert_eq!(created.motd, "stub ready");
    assert!(harness.service.has_owner_activity(&owner.agent));
    assert_eq!(harness.service.list(&owner.agent).len(), 1);
    assert!(harness.service.list(&foreign.agent).is_empty());
    let read_error = harness
        .service
        .read(
            &foreign.agent,
            &created.session_id,
            TerminalReadRequest::default(),
        )
        .expect_err("foreign read");
    assert_eq!(code(&read_error), Some(TerminalErrorCode::ForeignSession));
    assert_eq!(
        code(
            &harness
                .service
                .signal(&foreign.agent, &created.session_id, TerminalSignal::SIGINT)
                .await
                .expect_err("foreign signal")
        ),
        Some(TerminalErrorCode::ForeignSession)
    );
    assert_eq!(
        code(
            &harness
                .service
                .kill(&foreign.agent, &created.session_id, None)
                .await
                .expect_err("foreign kill")
        ),
        Some(TerminalErrorCode::ForeignSession)
    );
}

#[tokio::test]
async fn validation_duplicate_names_and_exclusive_sends_match_source() {
    let harness = Harness::new().await;
    let owner = harness.owner("owner");
    let error = harness
        .service
        .spawn(owner.agent.clone(), request("missing", None), None)
        .await
        .expect_err("non-live owner");
    assert_eq!(code(&error), Some(TerminalErrorCode::OwnerNotLive));
    harness.register(&owner);
    let error = harness
        .service
        .spawn(owner.agent.clone(), request("missing", None), None)
        .await
        .expect_err("missing backend");
    assert_eq!(code(&error), Some(TerminalErrorCode::NoBackend));
    let backend = StubBackend::new("stub");
    harness.register_backend(backend.clone());
    let created = harness
        .service
        .spawn(owner.agent.clone(), request("stub", Some("main")), None)
        .await
        .expect("spawn");
    assert_eq!(
        harness
            .service
            .spawn(owner.agent.clone(), request("stub", Some("")), None)
            .await
            .expect_err("empty name")
            .to_string(),
        "PTY session name must be non-empty"
    );
    let caller_reason = TerminalFailure::message("spawn aborted");
    let aborted = caller_abort(&caller_reason);
    let error = harness
        .service
        .spawn(owner.agent.clone(), request("stub", None), Some(aborted))
        .await
        .expect_err("pre-aborted");
    assert!(error.ptr_eq(&caller_reason));
    let error = harness
        .service
        .spawn(owner.agent.clone(), request("stub", Some("main")), None)
        .await
        .expect_err("duplicate name");
    assert_eq!(code(&error), Some(TerminalErrorCode::DuplicateName));

    let operation = harness
        .service
        .start_send(&owner.agent, &created.session_id, send_request())
        .expect("first send");
    let error = harness
        .service
        .start_send(&owner.agent, &created.session_id, send_request())
        .expect_err("exclusive send");
    assert_eq!(code(&error), Some(TerminalErrorCode::SendActive));
    assert_eq!(operation.read_output().delta, "delta");
    assert!(operation.cancel());
    operation.done().await.expect("settled");
    let next = harness
        .service
        .start_send(&owner.agent, &created.session_id, send_request())
        .expect("next send");
    assert!(next.cancel());
    next.done().await.expect("next settled");
    tokio::task::yield_now().await;

    backend.sessions.lock()[0]
        .reject_send
        .store(true, Ordering::Release);
    let failed = harness
        .service
        .start_send(&owner.agent, &created.session_id, send_request())
        .expect("failed operation handle");
    assert_eq!(
        failed.done().await.expect_err("send failure").to_string(),
        "send failed"
    );
}

#[tokio::test]
async fn concurrent_name_is_reserved_and_disappearing_owner_rolls_back() {
    let harness = Harness::new().await;
    let gate = Arc::new(Completion::default());
    let backend_gate = gate.clone();
    let backend = callback_backend("slow", move |_| {
        let gate = backend_gate.clone();
        async move { gate.wait().await }.boxed()
    });
    harness.register_backend_ref(&backend);
    let owner = harness.owner("owner");
    harness.register(&owner);
    let pending = {
        let service = harness.service.clone();
        let pending_owner = owner.agent.clone();
        tokio::spawn(async move {
            service
                .spawn(pending_owner, request("slow", Some("main")), None)
                .await
        })
    };
    tokio::task::yield_now().await;
    let error = harness
        .service
        .spawn(owner.agent.clone(), request("slow", Some("main")), None)
        .await
        .expect_err("reserved name");
    assert_eq!(code(&error), Some(TerminalErrorCode::DuplicateName));
    let disposal = tokio::spawn({
        let fiber = owner.fiber.clone();
        async move { fiber.dispose().await }
    });
    let session = StubSession::new();
    gate.resolve(Ok(session.clone()));
    let error = pending
        .await
        .expect("spawn task")
        .expect_err("owner vanished");
    assert_eq!(code(&error), Some(TerminalErrorCode::OwnerNotLive));
    disposal
        .await
        .expect("dispose task")
        .expect("owner disposal");
    assert_eq!(session.closed.lock().as_slice(), ["PTY spawn rolled back"]);
}

#[tokio::test]
async fn caller_cancellation_after_backend_completion_wins_and_rolls_back() {
    let harness = Harness::new().await;
    let gate = Arc::new(Completion::default());
    let backend_gate = gate.clone();
    let backend = callback_backend("slow", move |_| {
        let gate = backend_gate.clone();
        async move { gate.wait().await }.boxed()
    });
    harness.register_backend_ref(&backend);
    let owner = harness.owner("owner");
    harness.register(&owner);
    let signal = AbortSignal::default();
    let pending = {
        let service = harness.service.clone();
        let pending_owner = owner.agent.clone();
        let pending_signal = signal.clone();
        tokio::spawn(async move {
            service
                .spawn(pending_owner, request("slow", None), Some(pending_signal))
                .await
        })
    };
    tokio::task::yield_now().await;
    let reason = TerminalFailure::message("cancelled by caller");
    signal.abort_with_typed_reason(Arc::new(reason.clone()), json!(reason.to_string()));
    let session = StubSession::new();
    gate.resolve(Ok(session.clone()));
    let error = pending
        .await
        .expect("spawn task")
        .expect_err("caller cancellation");
    assert!(error.ptr_eq(&reason));
    assert_eq!(session.closed.lock().as_slice(), ["PTY spawn rolled back"]);
    assert!(Arc::ptr_eq(
        &harness.registry.get(owner.agent.id()).expect("live owner"),
        &owner.agent
    ));
}

#[tokio::test]
async fn caller_cancellation_wins_even_when_unpublished_rollback_fails() {
    let harness = Harness::new().await;
    let gate = Arc::new(Completion::default());
    let backend_gate = gate.clone();
    let backend = callback_backend("slow", move |_| {
        let gate = backend_gate.clone();
        async move { gate.wait().await }.boxed()
    });
    harness.register_backend_ref(&backend);
    let owner = harness.owner("owner");
    harness.register(&owner);
    let signal = AbortSignal::default();
    let pending = {
        let service = harness.service.clone();
        let pending_owner = owner.agent.clone();
        let pending_signal = signal.clone();
        tokio::spawn(async move {
            service
                .spawn(pending_owner, request("slow", None), Some(pending_signal))
                .await
        })
    };
    tokio::task::yield_now().await;
    let reason = TerminalFailure::message("cancelled by caller");
    signal.abort_with_typed_reason(Arc::new(reason.clone()), json!(reason.to_string()));
    let session = StubSession::new();
    session.reject_close.store(true, Ordering::Release);
    gate.resolve(Ok(session.clone()));
    let error = pending
        .await
        .expect("spawn task")
        .expect_err("caller cancellation");
    assert!(error.ptr_eq(&reason));
    assert!(harness.service.has_owner_activity(&owner.agent));
    let disposal = harness
        .service
        .dispose_all()
        .await
        .expect_err("retained cleanup failure");
    assert_eq!(disposal.to_string(), "failed to clean up PTY lifecycle");
    assert!(!harness.service.has_owner_activity(&owner.agent));
    assert_eq!(session.closed.lock().as_slice(), ["PTY spawn rolled back"]);
}

#[tokio::test]
async fn caller_reason_replaces_backend_rejection_in_response_to_abort() {
    let harness = Harness::new().await;
    let started = Arc::new(Completion::default());
    let backend_started = started.clone();
    let backend = callback_backend("abortable", move |spec| {
        let started = backend_started.clone();
        async move {
            let signal = spec.signal.expect("spawn signal");
            started.resolve(());
            signal.cancelled().await;
            Err(TerminalFailure::message("backend observed cancellation"))
        }
        .boxed()
    });
    harness.register_backend_ref(&backend);
    let owner = harness.owner("owner");
    harness.register(&owner);
    let signal = AbortSignal::default();
    let pending = {
        let service = harness.service.clone();
        let pending_owner = owner.agent.clone();
        let pending_signal = signal.clone();
        tokio::spawn(async move {
            service
                .spawn(
                    pending_owner,
                    request("abortable", None),
                    Some(pending_signal),
                )
                .await
        })
    };
    started.wait().await;
    let reason = TerminalFailure::message("cancelled by caller");
    signal.abort_with_typed_reason(Arc::new(reason.clone()), json!(reason.to_string()));
    let error = pending
        .await
        .expect("spawn task")
        .expect_err("caller cancellation");
    assert!(error.ptr_eq(&reason));
}

async fn retained_caller_cleanup_failure(scope: &str) {
    let harness = Harness::new().await;
    let started = Arc::new(Completion::default());
    let backend_started = started.clone();
    let cleanup_failure = TerminalFailure::message("backend cleanup failed");
    let backend_cleanup_failure = cleanup_failure.clone();
    let backend = callback_backend("cleanup-failing", move |spec| {
        let started = backend_started.clone();
        let cleanup = backend_cleanup_failure.clone();
        async move {
            let signal = spec.signal.expect("spawn signal");
            started.resolve(());
            signal.cancelled().await;
            Err(TerminalFailure::new(TerminalBackendCleanupError::new(
                abort_failure(&signal),
                cleanup,
            )))
        }
        .boxed()
    });
    harness.register_backend_ref(&backend);
    let owner = harness.owner("owner");
    harness.register(&owner);
    let signal = AbortSignal::default();
    let pending = spawn_task(
        harness.service.clone(),
        owner.agent.clone(),
        request("cleanup-failing", None),
        Some(signal.clone()),
    );
    started.wait().await;
    let reason = TerminalFailure::message("cancelled by caller");
    signal.abort_with_typed_reason(Arc::new(reason.clone()), json!(reason.to_string()));
    let error = pending
        .await
        .expect("spawn task")
        .expect_err("caller cancellation");
    assert!(error.ptr_eq(&reason));
    assert!(harness.service.has_owner_activity(&owner.agent));
    let disposal = if scope == "owner" {
        harness.service.owner_disposed(owner.agent.clone()).await
    } else {
        harness.service.dispose_all().await
    }
    .expect_err("retained cleanup failure");
    assert_eq!(disposal.to_string(), "failed to clean up PTY lifecycle");
    assert!(!harness.service.has_owner_activity(&owner.agent));
    let lifecycle = disposal
        .downcast_ref::<TerminalAggregateError>()
        .expect("lifecycle aggregate");
    let rollback = lifecycle.errors()[0]
        .downcast_ref::<TerminalAggregateError>()
        .expect("rollback aggregate");
    assert!(rollback.errors()[0].ptr_eq(&cleanup_failure));
}

#[tokio::test]
async fn caller_cleanup_failure_is_retained_until_owner_disposal() {
    retained_caller_cleanup_failure("owner").await;
}

#[tokio::test]
async fn caller_cleanup_failure_is_retained_until_service_disposal() {
    retained_caller_cleanup_failure("service").await;
}

async fn disposal_aborts_and_awaits_unpublished_setup(scope: &str) {
    let harness = Harness::new().await;
    let gate = Arc::new(Completion::default());
    let started = Arc::new(Completion::default());
    let observed_signal = Arc::new(Mutex::new(None));
    let backend_gate = gate.clone();
    let backend_started = started.clone();
    let backend_signal = observed_signal.clone();
    let backend = callback_backend("slow", move |spec| {
        let gate = backend_gate.clone();
        backend_started.resolve(());
        *backend_signal.lock() = spec.signal;
        async move { gate.wait().await }.boxed()
    });
    harness.register_backend_ref(&backend);
    let owner = harness.owner("owner");
    harness.register(&owner);
    let pending = spawn_task(
        harness.service.clone(),
        owner.agent.clone(),
        request("slow", None),
        None,
    );
    started.wait().await;
    let expected = if scope == "owner" {
        TerminalErrorCode::OwnerNotLive
    } else {
        TerminalErrorCode::ServiceDisposing
    };
    let disposal = if scope == "owner" {
        let fiber = owner.fiber.clone();
        tokio::spawn(async move {
            fiber
                .dispose()
                .await
                .map_err(|error| TerminalFailure::message(error.to_string()))
        })
    } else {
        let service = harness.service.clone();
        tokio::spawn(async move { service.dispose_all().await })
    };
    tokio::task::yield_now().await;
    let signal = observed_signal.lock().clone().expect("backend signal");
    assert!(signal.is_aborted());
    assert_eq!(
        signal
            .typed_reason::<TerminalError>()
            .expect("typed disposal reason")
            .code(),
        expected
    );
    assert!(!disposal.is_finished());
    let session = StubSession::new();
    gate.resolve(Ok(session.clone()));
    let pending_error = pending
        .await
        .expect("spawn task")
        .expect_err("disposal cancellation");
    assert_eq!(code(&pending_error), Some(expected));
    disposal
        .await
        .expect("disposal task")
        .expect("disposal succeeds");
    assert_eq!(session.closed.lock().as_slice(), ["PTY spawn rolled back"]);
}

#[tokio::test]
async fn owner_disposal_aborts_and_awaits_unpublished_setup() {
    disposal_aborts_and_awaits_unpublished_setup("owner").await;
}

#[tokio::test]
async fn service_disposal_aborts_and_awaits_unpublished_setup() {
    disposal_aborts_and_awaits_unpublished_setup("service").await;
}

#[tokio::test]
async fn unpublished_rollback_failure_is_reported_through_service_disposal() {
    let harness = Harness::new().await;
    let gate = Arc::new(Completion::default());
    let started = Arc::new(Completion::default());
    let backend_gate = gate.clone();
    let backend_started = started.clone();
    let backend = callback_backend("slow", move |_| {
        let gate = backend_gate.clone();
        backend_started.resolve(());
        async move { gate.wait().await }.boxed()
    });
    harness.register_backend_ref(&backend);
    let owner = harness.owner("owner");
    harness.register(&owner);
    let pending = spawn_task(
        harness.service.clone(),
        owner.agent.clone(),
        request("slow", None),
        None,
    );
    started.wait().await;
    let disposal = {
        let service = harness.service.clone();
        tokio::spawn(async move { service.dispose_all().await })
    };
    let session = StubSession::new();
    session.reject_close.store(true, Ordering::Release);
    gate.resolve(Ok(session.clone()));
    let pending_error = pending
        .await
        .expect("spawn task")
        .expect_err("rollback failure");
    assert_eq!(
        pending_error.to_string(),
        "PTY spawn and rollback both failed"
    );
    let disposal_error = disposal
        .await
        .expect("disposal task")
        .expect_err("cleanup failure");
    assert_eq!(
        disposal_error.to_string(),
        "failed to clean up PTY lifecycle"
    );
    assert_eq!(session.closed.lock().as_slice(), ["PTY spawn rolled back"]);
}

async fn disposal_retains_backend_startup_cleanup_failure(scope: &str) {
    let harness = Harness::new().await;
    let started = Arc::new(Completion::default());
    let backend_started = started.clone();
    let cleanup_failure = TerminalFailure::message("backend cleanup failed");
    let backend_cleanup = cleanup_failure.clone();
    let backend_reason = Arc::new(Mutex::new(None));
    let observed_reason = backend_reason.clone();
    let backend = callback_backend("cleanup-failing", move |spec| {
        let started = backend_started.clone();
        let cleanup = backend_cleanup.clone();
        let observed = observed_reason.clone();
        async move {
            let signal = spec.signal.expect("spawn signal");
            started.resolve(());
            signal.cancelled().await;
            let reason = abort_failure(&signal);
            *observed.lock() = Some(reason.clone());
            Err(TerminalFailure::new(TerminalBackendCleanupError::new(
                reason, cleanup,
            )))
        }
        .boxed()
    });
    harness.register_backend_ref(&backend);
    let owner = harness.owner("owner");
    harness.register(&owner);
    let pending = spawn_task(
        harness.service.clone(),
        owner.agent.clone(),
        request("cleanup-failing", None),
        None,
    );
    started.wait().await;
    let disposal = if scope == "owner" {
        let service = harness.service.clone();
        let disposal_owner = owner.agent.clone();
        tokio::spawn(async move { service.owner_disposed(disposal_owner).await })
    } else {
        let service = harness.service.clone();
        tokio::spawn(async move { service.dispose_all().await })
    };
    let pending_error = pending
        .await
        .expect("spawn task")
        .expect_err("service cancellation");
    let observed = backend_reason.lock().clone().expect("backend reason");
    assert!(pending_error.ptr_eq(&observed));
    assert_eq!(
        code(&pending_error),
        Some(if scope == "owner" {
            TerminalErrorCode::OwnerNotLive
        } else {
            TerminalErrorCode::ServiceDisposing
        })
    );
    let disposal_error = disposal
        .await
        .expect("disposal task")
        .expect_err("cleanup failure");
    let lifecycle = disposal_error
        .downcast_ref::<TerminalAggregateError>()
        .expect("lifecycle aggregate");
    let rollback = lifecycle.errors()[0]
        .downcast_ref::<TerminalAggregateError>()
        .expect("rollback aggregate");
    assert!(rollback.errors()[0].ptr_eq(&cleanup_failure));
}

#[tokio::test]
async fn owner_disposal_retains_backend_startup_cleanup_failure() {
    disposal_retains_backend_startup_cleanup_failure("owner").await;
}

#[tokio::test]
async fn service_disposal_retains_backend_startup_cleanup_failure() {
    disposal_retains_backend_startup_cleanup_failure("service").await;
}

#[tokio::test]
async fn independent_reservations_provider_failure_and_fused_signal_are_preserved() {
    let harness = Harness::new().await;
    let first_gate = Arc::new(Completion::default());
    let second_gate = Arc::new(Completion::default());
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let backend_first = first_gate.clone();
    let backend_second = second_gate.clone();
    let backend_count = count.clone();
    let backend = callback_backend("slow", move |_| {
        let gate = if backend_count.fetch_add(1, Ordering::AcqRel) == 0 {
            backend_first.clone()
        } else {
            backend_second.clone()
        };
        async move { gate.wait().await }.boxed()
    });
    harness.register_backend_ref(&backend);
    let owner = harness.owner("owner");
    harness.register(&owner);
    let first = spawn_task(
        harness.service.clone(),
        owner.agent.clone(),
        request("slow", Some("one")),
        None,
    );
    let second = spawn_task(
        harness.service.clone(),
        owner.agent.clone(),
        request("slow", Some("two")),
        None,
    );
    yield_until(|| count.load(Ordering::Acquire) == 2).await;
    first_gate.resolve(Ok(StubSession::new()));
    first.await.expect("first task").expect("first spawn");
    second_gate.resolve(Ok(StubSession::new()));
    second.await.expect("second task").expect("second spawn");

    let throwing = callback_backend("throwing", |_| {
        async { Err(TerminalFailure::message("provider failed")) }.boxed()
    });
    harness.register_backend_ref(&throwing);
    assert_eq!(
        harness
            .service
            .spawn(owner.agent.clone(), request("throwing", None), None)
            .await
            .expect_err("provider failure")
            .to_string(),
        "provider failed"
    );

    let saw_signal = Arc::new(AtomicBool::new(false));
    let backend_saw_signal = saw_signal.clone();
    let signaled = callback_backend("signaled", move |spec| {
        backend_saw_signal.store(spec.signal.is_some(), Ordering::Release);
        async { Ok(StubSession::new() as TerminalBackendSessionRef) }.boxed()
    });
    harness.register_backend_ref(&signaled);
    harness
        .service
        .spawn(
            owner.agent.clone(),
            request("signaled", None),
            Some(AbortSignal::default()),
        )
        .await
        .expect("signaled spawn");
    assert!(saw_signal.load(Ordering::Acquire));
}

#[tokio::test]
async fn optional_pid_is_omitted() {
    let harness = Harness::new().await;
    let owner = harness.owner("owner");
    harness.register(&owner);
    let session = StubSession::new();
    *session.pid.lock() = None;
    let backend_session = session.clone();
    let backend = callback_backend("virtual", move |_| {
        let session = backend_session.clone();
        async move { Ok(session as TerminalBackendSessionRef) }.boxed()
    });
    harness.register_backend_ref(&backend);
    let created = harness
        .service
        .spawn(owner.agent.clone(), request("virtual", None), None)
        .await
        .expect("virtual spawn");
    assert_eq!(created.pid, None);
    let json = serde_json::to_value(created).expect("serialize");
    assert!(json.get("pid").is_none());
}

#[tokio::test]
async fn owner_loss_and_failed_rollback_never_publish_false_success() {
    let harness = Harness::new().await;
    let owner = harness.owner("owner");
    harness.register(&owner);
    let failed_session = StubSession::new();
    failed_session.reject_close.store(true, Ordering::Release);
    let disposal_result = Arc::new(Completion::default());
    let backend_result = disposal_result.clone();
    let service = harness.service.clone();
    let disposal_owner = owner.agent.clone();
    let backend_session = failed_session.clone();
    let backend = callback_backend("bad-spawn", move |spec| {
        let service = service.clone();
        let owner = disposal_owner.clone();
        let result = backend_result.clone();
        let session = backend_session.clone();
        async move {
            let signal = spec.signal.expect("spawn signal");
            tokio::spawn(async move {
                result.resolve(service.owner_disposed(owner).await);
            });
            signal.cancelled().await;
            Ok(session as TerminalBackendSessionRef)
        }
        .boxed()
    });
    harness.register_backend_ref(&backend);
    let spawn_error = harness
        .service
        .spawn(owner.agent.clone(), request("bad-spawn", None), None)
        .await
        .expect_err("owner loss plus failed rollback");
    assert_eq!(
        spawn_error.to_string(),
        "PTY spawn and rollback both failed"
    );
    let owner_error = disposal_result
        .wait()
        .await
        .expect_err("retained rollback failure");
    assert_eq!(owner_error.to_string(), "failed to clean up PTY lifecycle");
    assert!(harness.service.list(&owner.agent).is_empty());
    assert_eq!(
        failed_session.closed.lock().as_slice(),
        ["PTY spawn rolled back"]
    );
}

#[tokio::test]
async fn kill_close_failure_preserves_the_published_session_for_retry() {
    let harness = Harness::new().await;
    let owner = harness.owner("owner");
    harness.register(&owner);
    let backend = StubBackend::new("bad-close");
    harness.register_backend(backend.clone());
    let created = harness
        .service
        .spawn(owner.agent.clone(), request("bad-close", None), None)
        .await
        .expect("spawn");
    backend.sessions.lock()[0]
        .reject_close
        .store(true, Ordering::Release);
    assert_eq!(
        harness
            .service
            .kill(&owner.agent, &created.session_id, None)
            .await
            .expect_err("close failure")
            .to_string(),
        "close failed"
    );
    assert_eq!(harness.service.list(&owner.agent).len(), 1);
}

#[tokio::test]
async fn joined_close_refuses_sends_and_removes_only_after_quiescence() {
    let harness = Harness::new().await;
    let owner = harness.owner("owner");
    harness.register(&owner);
    let backend = StubBackend::new("stub");
    harness.register_backend(backend.clone());
    let created = harness
        .service
        .spawn(owner.agent.clone(), request("stub", None), None)
        .await
        .expect("spawn");
    let session = backend.sessions.lock()[0].clone();
    let gate = session.close_gate();
    let first = {
        let service = harness.service.clone();
        let kill_owner = owner.agent.clone();
        let id = created.session_id.clone();
        tokio::spawn(async move { service.kill(&kill_owner, &id, None).await })
    };
    yield_until(|| !session.closed.lock().is_empty()).await;
    assert_eq!(
        harness
            .service
            .start_send(&owner.agent, &created.session_id, send_request())
            .expect_err("closing send")
            .to_string(),
        format!("PTY session {} is closing", created.session_id)
    );
    let second = {
        let service = harness.service.clone();
        let kill_owner = owner.agent.clone();
        let id = created.session_id.clone();
        tokio::spawn(async move { service.kill(&kill_owner, &id, None).await })
    };
    gate.resolve(());
    assert!(first.await.expect("first task").expect("first close"));
    assert!(!second.await.expect("second task").expect("joined close"));
    assert_eq!(
        code(
            &harness
                .service
                .read(
                    &owner.agent,
                    &created.session_id,
                    TerminalReadRequest::default(),
                )
                .expect_err("removed session")
        ),
        Some(TerminalErrorCode::NoSession)
    );
}

#[tokio::test]
async fn owner_cleanup_outlives_backend_registration_and_is_awaited() {
    let harness = Harness::new().await;
    let backend = StubBackend::new("stub");
    let backend_effect = harness.register_backend(backend.clone());
    let owner = harness.owner("owner");
    harness.register(&owner);
    let created = harness
        .service
        .spawn(owner.agent.clone(), request("stub", None), None)
        .await
        .expect("spawn");
    backend_effect.dispose().await.expect("unregister backend");
    assert!(harness.service.list_backends().is_empty());
    assert_eq!(
        harness
            .service
            .read(
                &owner.agent,
                &created.session_id,
                TerminalReadRequest::default(),
            )
            .expect("read existing")
            .text,
        "0:0"
    );
    owner.fiber.dispose().await.expect("owner cleanup");
    assert_eq!(
        backend.sessions.lock()[0].closed.lock().as_slice(),
        ["PTY owner disposed"]
    );
    assert!(harness.service.list(&owner.agent).is_empty());
}

#[tokio::test]
async fn kill_is_idempotent_and_service_disposal_closes_every_owner() {
    let harness = Harness::new().await;
    let backend = StubBackend::new("stub");
    harness.register_backend(backend.clone());
    let first_owner = harness.owner("first");
    let second_owner = harness.owner("second");
    harness.register(&first_owner);
    harness.register(&second_owner);
    let first = harness
        .service
        .spawn(first_owner.agent.clone(), request("stub", None), None)
        .await
        .expect("first spawn");
    harness
        .service
        .spawn(second_owner.agent.clone(), request("stub", None), None)
        .await
        .expect("second spawn");
    assert!(
        harness
            .service
            .kill(&first_owner.agent, &first.session_id, None)
            .await
            .expect("kill")
    );
    assert_eq!(
        backend.sessions.lock()[0].closed.lock().as_slice(),
        ["model request"]
    );
    harness
        .service
        .dispose_all()
        .await
        .expect("service disposal");
    assert_eq!(
        backend.sessions.lock()[1].closed.lock().as_slice(),
        ["PTY service disposed"]
    );
    assert_eq!(
        code(
            &harness
                .service
                .spawn(first_owner.agent.clone(), request("stub", None), None)
                .await
                .expect_err("disposing")
        ),
        Some(TerminalErrorCode::ServiceDisposing)
    );
}

#[tokio::test]
async fn close_records_joins_failures_then_allows_retry_without_clobbering_fences() {
    let harness = Harness::new().await;
    let backend = StubBackend::new("stub");
    harness.register_backend(backend.clone());
    let owner = harness.owner("owner");
    harness.register(&owner);
    harness
        .service
        .spawn(owner.agent.clone(), request("stub", None), None)
        .await
        .expect("spawn");
    let session = backend.sessions.lock()[0].clone();
    session.reject_close.store(true, Ordering::Release);
    let gate = session.close_gate();
    let records = harness
        .service
        .state
        .lock()
        .sessions
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let first = {
        let service = harness.service.clone();
        let records = records.clone();
        tokio::spawn(async move { service.close_records(records, "test failure").await })
    };
    yield_until(|| !session.closed.lock().is_empty()).await;
    let joined = {
        let service = harness.service.clone();
        let records = records.clone();
        tokio::spawn(async move { service.close_records(records, "joined failure").await })
    };
    gate.resolve(());
    assert_eq!(
        first
            .await
            .expect("first task")
            .expect_err("first failure")
            .to_string(),
        "failed to close 1 PTY session(s)"
    );
    assert_eq!(
        joined
            .await
            .expect("joined task")
            .expect_err("joined failure")
            .to_string(),
        "failed to close 1 PTY session(s)"
    );
    session.reject_close.store(false, Ordering::Release);
    let retry_records = harness
        .service
        .state
        .lock()
        .sessions
        .values()
        .cloned()
        .collect();
    harness
        .service
        .close_records(retry_records, "retry")
        .await
        .expect("retry");
    assert_eq!(session.closed.lock().as_slice(), ["test failure", "retry"]);
    assert!(harness.service.state.lock().sessions.is_empty());
}

#[tokio::test]
async fn service_disposal_clears_registries_and_owner_cleanups_after_close_failure() {
    let harness = Harness::new().await;
    let backend = StubBackend::new("stub");
    harness.register_backend(backend.clone());
    let owner = harness.owner("owner");
    harness.register(&owner);
    harness
        .service
        .spawn(owner.agent.clone(), request("stub", None), None)
        .await
        .expect("spawn");
    backend.sessions.lock()[0]
        .reject_close
        .store(true, Ordering::Release);
    let error = harness
        .service
        .dispose_all()
        .await
        .expect_err("close failure");
    assert!(
        error
            .to_string()
            .contains("failed to clean up PTY lifecycle")
    );
    let state = harness.service.state.lock();
    assert!(state.backends.is_empty());
    assert!(state.owner_cleanups.is_empty());
}

#[tokio::test]
async fn installed_service_fiber_runs_awaited_teardown() {
    let harness = Harness::new().await;
    let backend = StubBackend::new("stub");
    harness.register_backend(backend.clone());
    let owner = harness.owner("owner");
    harness.register(&owner);
    harness
        .service
        .spawn(owner.agent.clone(), request("stub", None), None)
        .await
        .expect("spawn");
    harness
        .service_fiber
        .dispose()
        .await
        .expect("service fiber disposal");
    assert_eq!(
        backend.sessions.lock()[0].closed.lock().as_slice(),
        ["PTY service disposed"]
    );
}

#[test]
fn wire_types_preserve_literal_delivery_and_unknown_exit_signals() {
    let delivered = TerminalSignalResult::delivered(ProcessGroupId::new(77));
    assert!(delivered.is_delivered());
    assert_eq!(
        serde_json::to_value(&delivered).expect("signal result"),
        json!({"delivered": true, "targetPgid": 77})
    );
    assert!(
        serde_json::from_value::<TerminalSignalResult>(
            json!({"delivered": false, "targetPgid": 77})
        )
        .is_err()
    );

    let status = TerminalSessionStatus::Exited {
        exit_code: None,
        signal: Some(seekdeep_subprocess::ProcessSignal::new("SIGFUTURE")),
    };
    let encoded = serde_json::to_value(&status).expect("status");
    assert_eq!(
        serde_json::from_value::<TerminalSessionStatus>(encoded).expect("round trip"),
        status
    );
}
