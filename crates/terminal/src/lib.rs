//! Provider-neutral persistent PTY registry with exact-owner lifecycle control.

use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{Arc, Weak},
};

use futures::{FutureExt as _, future::Shared};
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_agent::{AGENTS, Agent};
use seekdeep_cordis::{Context, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_llm::AbortSignal;
use serde_json::{Value, json};
use thiserror::Error;

/// Explained-empty package invariant companion.
pub mod invariant;
/// Backend, session, operation, and wire-facing terminal vocabulary.
pub mod types;

pub use types::{
    TerminalAggregateError, TerminalBackend, TerminalBackendCleanupError, TerminalBackendRef,
    TerminalBackendSession, TerminalBackendSessionRef, TerminalBackendSpawnSpec, TerminalFailure,
    TerminalReadRequest, TerminalReadResult, TerminalResult, TerminalSendOperation,
    TerminalSendOperationRef, TerminalSendRead, TerminalSendRequest, TerminalSendResult,
    TerminalSessionId, TerminalSessionSnapshot, TerminalSessionStatus, TerminalSignal,
    TerminalSignalResult, TerminalSpawnRequest, TerminalSpawnResult, TerminalWaitReason,
};

/// Typed Cordis seat corresponding to `ctx.terminals`.
pub const TERMINALS: ServiceKey<TerminalSessionService> = ServiceKey::new("terminals");
/// Loader plugin name.
pub const NAME: &str = "terminal";
/// The registry resolves agent ownership lazily when a session is spawned.
pub const INJECT: &[&str] = &[];

/// Builds the Loader-compatible persistent terminal registry.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, _| {
        Box::pin(async move {
            TerminalSessionService::install(&context)
                .await
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            Ok(())
        })
    })
}

/// Machine-routable PTY service failure codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalErrorCode {
    /// A provider already occupies a backend type.
    DuplicateBackend,
    /// A published or concurrently reserved owner-local name collides.
    DuplicateName,
    /// A session belongs to a different exact agent object.
    ForeignSession,
    /// No provider occupies the requested backend type.
    NoBackend,
    /// The session identity is unknown.
    NoSession,
    /// The exact agent is not live in the active registry.
    OwnerNotLive,
    /// A send already owns this session's exclusive operation slot.
    SendActive,
    /// Service teardown has begun.
    ServiceDisposing,
}

impl TerminalErrorCode {
    /// Stable source-compatible wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DuplicateBackend => "DUPLICATE_BACKEND",
            Self::DuplicateName => "DUPLICATE_NAME",
            Self::ForeignSession => "FOREIGN_SESSION",
            Self::NoBackend => "NO_BACKEND",
            Self::NoSession => "NO_SESSION",
            Self::OwnerNotLive => "OWNER_NOT_LIVE",
            Self::SendActive => "SEND_ACTIVE",
            Self::ServiceDisposing => "SERVICE_DISPOSING",
        }
    }
}

/// Error carrying a stable [`TerminalErrorCode`].
#[derive(Clone, Debug, Error)]
#[error("{message}")]
pub struct TerminalError {
    message: Arc<str>,
    code: TerminalErrorCode,
}

impl TerminalError {
    /// Creates one machine-routable service error.
    #[must_use]
    pub fn new(message: impl Into<Arc<str>>, code: TerminalErrorCode) -> Self {
        Self {
            message: message.into(),
            code,
        }
    }

    /// Stable machine route.
    #[must_use]
    pub const fn code(&self) -> TerminalErrorCode {
        self.code
    }

    fn json(&self) -> Value {
        json!({ "name": "TerminalError", "message": self.message.as_ref(), "code": self.code.as_str() })
    }
}

#[derive(Clone, Debug, Error)]
#[error("terminal operation aborted: {0}")]
struct TerminalAbortReason(Value);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct OwnerKey(usize);

impl OwnerKey {
    fn of(owner: &Arc<Agent>) -> Self {
        Self(Arc::as_ptr(owner).cast::<()>() as usize)
    }
}

#[derive(Debug)]
struct PendingSpawn {
    id: u64,
    owner: Arc<Agent>,
    signal: AbortSignal,
    released: Mutex<bool>,
    cleanup_failure: Mutex<Option<TerminalFailure>>,
    settled: tokio::sync::Notify,
}

impl PendingSpawn {
    async fn wait(&self) {
        loop {
            let notified = self.settled.notified();
            if *self.released.lock() {
                return;
            }
            notified.await;
        }
    }

    fn release(&self, cleanup_failure: Option<TerminalFailure>) {
        *self.cleanup_failure.lock() = cleanup_failure;
        *self.released.lock() = true;
        self.settled.notify_waiters();
    }
}

type CloseFuture = Shared<futures::future::BoxFuture<'static, TerminalResult<()>>>;

#[derive(Clone)]
struct CloseFence(CloseFuture);

impl fmt::Debug for CloseFence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CloseFence(..)")
    }
}

impl CloseFence {
    fn new(session: TerminalBackendSessionRef, reason: String) -> Self {
        Self(async move { session.close(&reason).await }.boxed().shared())
    }

    async fn wait(&self) -> TerminalResult<()> {
        self.0.clone().await
    }
}

#[derive(Debug, Default)]
struct SessionLifecycle {
    active_send: Option<u64>,
    closing: Option<Arc<CloseFence>>,
}

#[derive(Debug)]
struct SessionRecord {
    id: TerminalSessionId,
    owner: Arc<Agent>,
    name: Option<String>,
    terminal_type: String,
    session: TerminalBackendSessionRef,
    lifecycle: Mutex<SessionLifecycle>,
}

#[derive(Debug)]
struct ManagedSendOperation {
    inner: TerminalSendOperationRef,
    record: Weak<SessionRecord>,
    send_id: u64,
}

impl TerminalSendOperation for ManagedSendOperation {
    fn done(&self) -> futures::future::BoxFuture<'static, TerminalResult<TerminalSendResult>> {
        let inner = self.inner.clone();
        let record = self.record.clone();
        let send_id = self.send_id;
        async move {
            let result = inner.done().await;
            if let Some(record) = record.upgrade() {
                let mut lifecycle = record.lifecycle.lock();
                if lifecycle.active_send == Some(send_id) {
                    lifecycle.active_send = None;
                }
            }
            result
        }
        .boxed()
    }

    fn read_output(&self) -> TerminalSendRead {
        self.inner.read_output()
    }

    fn cancel(&self) -> bool {
        self.inner.cancel()
    }
}

#[derive(Debug, Default)]
struct RegistryState {
    backends: IndexMap<String, TerminalBackendRef>,
    sessions: IndexMap<TerminalSessionId, Arc<SessionRecord>>,
    reserved_names: HashMap<OwnerKey, HashSet<String>>,
    pending_spawns: IndexMap<u64, Arc<PendingSpawn>>,
    owner_cleanups: HashMap<OwnerKey, EffectHandle>,
    disposed_owners: Vec<Weak<Agent>>,
    next_id: u64,
    next_pending_id: u64,
    next_send_id: u64,
    disposing: bool,
}

/// In-process registry for replaceable PTY backends and exact-agent sessions.
#[derive(Debug)]
pub struct TerminalSessionService {
    context: Context,
    entry_gate: tokio::sync::Mutex<()>,
    state: Mutex<RegistryState>,
}

impl TerminalSessionService {
    /// Installs the service and its awaited teardown effect in one context.
    ///
    /// # Errors
    ///
    /// Returns duplicate-service or inactive-context failures.
    pub async fn install(context: &Context) -> TerminalResult<Arc<Self>> {
        let service = Arc::new(Self {
            context: context.clone(),
            entry_gate: tokio::sync::Mutex::new(()),
            state: Mutex::new(RegistryState::default()),
        });
        let provision = context
            .provide(TERMINALS, service.clone())
            .map_err(|error| TerminalFailure::message(error.to_string()))?;
        let weak = Arc::downgrade(&service);
        let teardown = EffectHandle::new("pty teardown", move || {
            Box::pin(async move {
                if let Some(service) = weak.upgrade() {
                    service
                        .dispose_all()
                        .await
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                }
                Ok(())
            })
        });
        if let Err(error) = context.own(teardown) {
            let _ = provision.dispose().await;
            return Err(TerminalFailure::message(error.to_string()));
        }
        Ok(service)
    }

    /// Registers one non-empty backend type as an exact reversible effect.
    ///
    /// # Errors
    ///
    /// Rejects empty, duplicate, or inactive-scope registration.
    pub fn register_backend(
        self: &Arc<Self>,
        owner_context: &Context,
        backend: &TerminalBackendRef,
    ) -> TerminalResult<EffectHandle> {
        let backend_type = backend.backend_type().to_owned();
        if backend_type.is_empty() {
            return Err(TerminalFailure::message(
                "pty backend type must be non-empty",
            ));
        }
        {
            let mut state = self.state.lock();
            if state.backends.contains_key(&backend_type) {
                return Err(terminal_error(
                    format!("a PTY backend named \"{backend_type}\" is already registered"),
                    TerminalErrorCode::DuplicateBackend,
                ));
            }
            state.backends.insert(backend_type.clone(), backend.clone());
        }
        let weak = Arc::downgrade(self);
        let contribution = backend.clone();
        let contribution_type = backend_type.clone();
        let effect = EffectHandle::synchronous("pty.registerBackend()", move || {
            if let Some(service) = weak.upgrade() {
                let mut state = service.state.lock();
                if state
                    .backends
                    .get(&contribution_type)
                    .is_some_and(|current| Arc::ptr_eq(current, &contribution))
                {
                    state.backends.shift_remove(&contribution_type);
                }
            }
            Ok(())
        });
        if let Err(error) = owner_context.own(effect.clone()) {
            let mut state = self.state.lock();
            if state
                .backends
                .get(&backend_type)
                .is_some_and(|current| Arc::ptr_eq(current, backend))
            {
                state.backends.shift_remove(&backend_type);
            }
            return Err(TerminalFailure::message(error.to_string()));
        }
        Ok(effect)
    }

    /// Lists backend types in registration order.
    #[must_use]
    pub fn list_backends(&self) -> Vec<String> {
        self.state.lock().backends.keys().cloned().collect()
    }

    /// Creates and publishes one owner-scoped session after setup succeeds.
    ///
    /// # Errors
    ///
    /// Preserves source validation, cancellation, rollback, and publication failures.
    #[allow(clippy::too_many_lines)] // Preserve the source's single ordered publication transaction.
    pub async fn spawn(
        self: &Arc<Self>,
        owner: Arc<Agent>,
        request: TerminalSpawnRequest,
        caller_signal: Option<AbortSignal>,
    ) -> TerminalResult<TerminalSpawnResult> {
        let entry = self.entry_gate.lock().await;
        self.assert_active()?;
        if let Some(signal) = &caller_signal {
            throw_if_aborted(signal)?;
        }
        self.ensure_owner_cleanup(&owner)?;
        let backend = self
            .state
            .lock()
            .backends
            .get(&request.terminal_type)
            .cloned()
            .ok_or_else(|| {
                terminal_error(
                    format!(
                        "no PTY backend registered for \"{}\"",
                        request.terminal_type
                    ),
                    TerminalErrorCode::NoBackend,
                )
            })?;
        if request.name.as_deref() == Some("") {
            return Err(TerminalFailure::message(
                "PTY session name must be non-empty",
            ));
        }
        self.reserve_name(&owner, request.name.as_deref())?;
        let pending = self.reserve_spawn(owner.clone());
        let backend_signal = caller_signal.as_ref().map_or_else(
            || pending.signal.clone(),
            |caller| AbortSignal::fuse(caller, &pending.signal),
        );
        let session_id = {
            let mut state = self.state.lock();
            state.next_id += 1;
            TerminalSessionId::new(format!("pty-{}", state.next_id))
        };
        drop(entry);

        let mut cleanup_failure = None;
        let mut published = false;
        let mut session = None;
        let result = match backend
            .spawn(TerminalBackendSpawnSpec {
                session_id: session_id.clone(),
                owner: owner.clone(),
                terminal_type: request.terminal_type.clone(),
                name: request.name.clone(),
                cwd: request.cwd.clone(),
                signal: Some(backend_signal),
            })
            .await
        {
            Ok(created) => {
                session = Some(created.clone());
                let publication = caller_signal
                    .as_ref()
                    .map_or(Ok(()), throw_if_aborted)
                    .and_then(|()| {
                        let record = Arc::new(SessionRecord {
                            id: session_id.clone(),
                            owner: owner.clone(),
                            name: request.name.clone(),
                            terminal_type: request.terminal_type.clone(),
                            session: created.clone(),
                            lifecycle: Mutex::new(SessionLifecycle::default()),
                        });
                        self.publish_record(&owner, record.clone())?;
                        Ok(record)
                    });
                match publication {
                    Ok(record) => {
                        let snapshot = Self::spawn_snapshot(&record);
                        published = true;
                        Ok(snapshot)
                    }
                    Err(error) => Err(error),
                }
            }
            Err(error) => {
                if let Some(backend_cleanup) = error.downcast_ref::<TerminalBackendCleanupError>() {
                    cleanup_failure = Some(backend_cleanup.cleanup_error.clone());
                }
                Err(error)
            }
        };

        let final_result = match result {
            Ok(value) => Ok(value),
            Err(mut failure) => {
                let mut rollback_failure = None;
                if !published
                    && let Some(unpublished) = session
                    && let Err(error) = unpublished.close("PTY spawn rolled back").await
                {
                    cleanup_failure = Some(error.clone());
                    rollback_failure = Some(error);
                }
                if let Some(signal) = &caller_signal
                    && signal.is_aborted()
                {
                    failure = abort_failure(signal);
                } else if pending.signal.is_aborted() {
                    failure = abort_failure(&pending.signal);
                }
                if let Some(rollback) = rollback_failure
                    && caller_signal
                        .as_ref()
                        .is_none_or(|signal| !signal.is_aborted())
                {
                    Err(TerminalFailure::new(TerminalAggregateError::new(
                        "PTY spawn and rollback both failed",
                        vec![failure, rollback],
                    )))
                } else {
                    Err(failure)
                }
            }
        };
        self.release_spawn(&pending, cleanup_failure);
        self.release_name(&owner, request.name.as_deref());
        final_result
    }

    /// Reports activity across unpublished setup and published session lifetime.
    #[must_use]
    pub fn has_owner_activity(&self, owner: &Arc<Agent>) -> bool {
        let key = OwnerKey::of(owner);
        let state = self.state.lock();
        state
            .pending_spawns
            .values()
            .any(|pending| OwnerKey::of(&pending.owner) == key)
            || state
                .sessions
                .values()
                .any(|record| Arc::ptr_eq(&record.owner, owner))
    }

    /// Starts one exclusive interactive send.
    ///
    /// # Errors
    ///
    /// Rejects unknown, foreign, closing, or already-active sessions.
    pub fn start_send(
        &self,
        owner: &Arc<Agent>,
        id: &TerminalSessionId,
        request: TerminalSendRequest,
    ) -> TerminalResult<TerminalSendOperationRef> {
        let record = self.expect_owned(owner, id)?;
        let (operation, send_id) = {
            let mut lifecycle = record.lifecycle.lock();
            if lifecycle.closing.is_some() {
                return Err(TerminalFailure::message(format!(
                    "PTY session {id} is closing"
                )));
            }
            if lifecycle.active_send.is_some() {
                return Err(terminal_error(
                    format!("PTY session {id} already has an active send"),
                    TerminalErrorCode::SendActive,
                ));
            }
            let send_id = {
                let mut state = self.state.lock();
                state.next_send_id += 1;
                state.next_send_id
            };
            lifecycle.active_send = Some(send_id);
            match record.session.start_send(request) {
                Ok(operation) => (operation, send_id),
                Err(error) => {
                    lifecycle.active_send = None;
                    return Err(error);
                }
            }
        };
        let operation: TerminalSendOperationRef = Arc::new(ManagedSendOperation {
            inner: operation,
            record: Arc::downgrade(&record),
            send_id,
        });
        let watched = operation.clone();
        tokio::spawn(async move {
            let _ = watched.done().await;
        });
        Ok(operation)
    }

    /// Reads one bounded scrollback page from an owned session.
    ///
    /// # Errors
    ///
    /// Rejects unknown or foreign sessions and forwards backend read failures.
    pub fn read(
        &self,
        owner: &Arc<Agent>,
        id: &TerminalSessionId,
        request: TerminalReadRequest,
    ) -> TerminalResult<TerminalReadResult> {
        self.expect_owned(owner, id)?.session.read(request)
    }

    /// Signals an owned session's verified foreground process group.
    ///
    /// # Errors
    ///
    /// Rejects unknown or foreign sessions and forwards backend signal failures.
    pub async fn signal(
        &self,
        owner: &Arc<Agent>,
        id: &TerminalSessionId,
        signal: TerminalSignal,
    ) -> TerminalResult<TerminalSignalResult> {
        self.expect_owned(owner, id)?.session.signal(signal).await
    }

    /// Closes one session and removes it only after quiescent cleanup.
    ///
    /// Returns `false` when it joined the same already-running close.
    ///
    /// # Errors
    ///
    /// Rejects unknown or foreign sessions and preserves backend close failures.
    pub async fn kill(
        &self,
        owner: &Arc<Agent>,
        id: &TerminalSessionId,
        reason: Option<&str>,
    ) -> TerminalResult<bool> {
        let record = self.expect_owned(owner, id)?;
        let (fence, joined) = {
            let mut lifecycle = record.lifecycle.lock();
            if let Some(closing) = &lifecycle.closing {
                (closing.clone(), true)
            } else {
                let closing = Arc::new(CloseFence::new(
                    record.session.clone(),
                    reason.unwrap_or("model request").to_owned(),
                ));
                lifecycle.closing = Some(closing.clone());
                (closing, false)
            }
        };
        match fence.wait().await {
            Ok(()) => {
                if !joined {
                    self.remove_exact_session(id, &record);
                }
                Ok(!joined)
            }
            Err(error) => {
                let mut lifecycle = record.lifecycle.lock();
                if lifecycle
                    .closing
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &fence))
                {
                    lifecycle.closing = None;
                }
                Err(error)
            }
        }
    }

    /// Lists fresh snapshots for exactly one owner in publication order.
    #[must_use]
    pub fn list(&self, owner: &Arc<Agent>) -> Vec<TerminalSessionSnapshot> {
        let records = self
            .state
            .lock()
            .sessions
            .values()
            .filter(|record| Arc::ptr_eq(&record.owner, owner))
            .cloned()
            .collect::<Vec<_>>();
        records
            .iter()
            .map(|record| Self::snapshot(record))
            .collect()
    }

    fn assert_active(&self) -> TerminalResult<()> {
        if self.state.lock().disposing {
            Err(terminal_error(
                "PTY service is disposing",
                TerminalErrorCode::ServiceDisposing,
            ))
        } else {
            Ok(())
        }
    }

    fn publish_record(&self, owner: &Arc<Agent>, record: Arc<SessionRecord>) -> TerminalResult<()> {
        let mut state = self.state.lock();
        if state.disposing {
            return Err(terminal_error(
                "PTY service is disposing",
                TerminalErrorCode::ServiceDisposing,
            ));
        }
        if !self.is_live_owner_locked(&mut state, owner) {
            return Err(terminal_error(
                "PTY owner is no longer live",
                TerminalErrorCode::OwnerNotLive,
            ));
        }
        state.sessions.insert(record.id.clone(), record);
        Ok(())
    }

    fn is_live_owner_locked(&self, state: &mut RegistryState, owner: &Arc<Agent>) -> bool {
        state
            .disposed_owners
            .retain(|disposed| disposed.strong_count() > 0);
        if state.disposed_owners.iter().any(|disposed| {
            disposed
                .upgrade()
                .is_some_and(|candidate| Arc::ptr_eq(&candidate, owner))
        }) {
            return false;
        }
        self.context
            .get(AGENTS)
            .and_then(|agents| agents.get(owner.id()))
            .is_some_and(|registered| Arc::ptr_eq(&registered, owner))
    }

    fn ensure_owner_cleanup(self: &Arc<Self>, owner: &Arc<Agent>) -> TerminalResult<()> {
        let key = OwnerKey::of(owner);
        {
            let mut state = self.state.lock();
            if !self.is_live_owner_locked(&mut state, owner) {
                return Err(terminal_error(
                    format!("agent \"{}\" is not the registered PTY owner", owner.id()),
                    TerminalErrorCode::OwnerNotLive,
                ));
            }
            if state.owner_cleanups.contains_key(&key) {
                return Ok(());
            }
        }
        let weak_service = Arc::downgrade(self);
        let cleanup_owner = owner.clone();
        let effect = EffectHandle::new("pty.ownerCleanup()", move || {
            Box::pin(async move {
                if let Some(service) = weak_service.upgrade() {
                    service
                        .owner_disposed(cleanup_owner)
                        .await
                        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                }
                Ok(())
            })
        });
        {
            let mut state = self.state.lock();
            if !self.is_live_owner_locked(&mut state, owner) {
                return Err(terminal_error(
                    format!("agent \"{}\" is not the registered PTY owner", owner.id()),
                    TerminalErrorCode::OwnerNotLive,
                ));
            }
            if state.owner_cleanups.contains_key(&key) {
                return Ok(());
            }
            state.owner_cleanups.insert(key, effect.clone());
        }
        if owner.context().own(effect).is_err() {
            let mut state = self.state.lock();
            state.owner_cleanups.remove(&key);
            state.disposed_owners.push(Arc::downgrade(owner));
            return Err(terminal_error(
                format!("agent \"{}\" is not the registered PTY owner", owner.id()),
                TerminalErrorCode::OwnerNotLive,
            ));
        }
        Ok(())
    }

    fn reserve_name(&self, owner: &Arc<Agent>, name: Option<&str>) -> TerminalResult<()> {
        let Some(name) = name else {
            return Ok(());
        };
        let key = OwnerKey::of(owner);
        let mut state = self.state.lock();
        if state
            .sessions
            .values()
            .any(|record| Arc::ptr_eq(&record.owner, owner) && record.name.as_deref() == Some(name))
        {
            return Err(terminal_error(
                format!("PTY session name \"{name}\" already exists for this owner"),
                TerminalErrorCode::DuplicateName,
            ));
        }
        let reserved = state.reserved_names.entry(key).or_default();
        if !reserved.insert(name.to_owned()) {
            return Err(terminal_error(
                format!("PTY session name \"{name}\" is already being created"),
                TerminalErrorCode::DuplicateName,
            ));
        }
        Ok(())
    }

    fn release_name(&self, owner: &Arc<Agent>, name: Option<&str>) {
        let Some(name) = name else {
            return;
        };
        let key = OwnerKey::of(owner);
        let mut state = self.state.lock();
        if let Some(reserved) = state.reserved_names.get_mut(&key) {
            reserved.remove(name);
            if reserved.is_empty() {
                state.reserved_names.remove(&key);
            }
        }
    }

    fn reserve_spawn(&self, owner: Arc<Agent>) -> Arc<PendingSpawn> {
        let mut state = self.state.lock();
        state.next_pending_id += 1;
        let pending = Arc::new(PendingSpawn {
            id: state.next_pending_id,
            owner,
            signal: AbortSignal::default(),
            released: Mutex::new(false),
            cleanup_failure: Mutex::new(None),
            settled: tokio::sync::Notify::new(),
        });
        state.pending_spawns.insert(pending.id, pending.clone());
        pending
    }

    fn release_spawn(&self, pending: &Arc<PendingSpawn>, cleanup_failure: Option<TerminalFailure>) {
        let removable = cleanup_failure.is_none();
        pending.release(cleanup_failure);
        if removable {
            self.state.lock().pending_spawns.shift_remove(&pending.id);
        }
    }

    async fn abort_pending_spawns(
        &self,
        owner: Option<&Arc<Agent>>,
        reason: Arc<TerminalError>,
    ) -> TerminalResult<()> {
        let pending = self
            .state
            .lock()
            .pending_spawns
            .values()
            .filter(|pending| owner.is_none_or(|owner| Arc::ptr_eq(&pending.owner, owner)))
            .cloned()
            .collect::<Vec<_>>();
        for spawn in &pending {
            spawn
                .signal
                .abort_with_typed_reason(reason.clone(), reason.json());
        }
        futures::future::join_all(pending.iter().map(|spawn| spawn.wait())).await;
        let failures = pending
            .iter()
            .filter_map(|spawn| spawn.cleanup_failure.lock().clone())
            .collect::<Vec<_>>();
        {
            let mut state = self.state.lock();
            for spawn in &pending {
                state.pending_spawns.shift_remove(&spawn.id);
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(TerminalFailure::new(TerminalAggregateError::new(
                "failed to roll back unpublished PTY setup",
                failures,
            )))
        }
    }

    fn expect_owned(
        &self,
        owner: &Arc<Agent>,
        id: &TerminalSessionId,
    ) -> TerminalResult<Arc<SessionRecord>> {
        let record = self.state.lock().sessions.get(id).cloned().ok_or_else(|| {
            terminal_error(
                format!("unknown PTY session {id}"),
                TerminalErrorCode::NoSession,
            )
        })?;
        if !Arc::ptr_eq(&record.owner, owner) {
            return Err(terminal_error(
                format!("PTY session {id} belongs to another agent"),
                TerminalErrorCode::ForeignSession,
            ));
        }
        Ok(record)
    }

    fn snapshot(record: &SessionRecord) -> TerminalSessionSnapshot {
        TerminalSessionSnapshot {
            session_id: record.id.clone(),
            name: record.name.clone(),
            terminal_type: record.terminal_type.clone(),
            pid: record.session.pid(),
            status: record.session.status(),
        }
    }

    fn spawn_snapshot(record: &SessionRecord) -> TerminalSpawnResult {
        TerminalSpawnResult {
            session_id: record.id.clone(),
            name: record.name.clone(),
            terminal_type: record.terminal_type.clone(),
            pid: record.session.pid(),
            status: record.session.status(),
            motd: record.session.motd(),
        }
    }

    async fn owner_disposed(self: &Arc<Self>, owner: Arc<Agent>) -> TerminalResult<()> {
        let key = OwnerKey::of(&owner);
        {
            let _entry = self.entry_gate.lock().await;
            let mut state = self.state.lock();
            state.disposed_owners.push(Arc::downgrade(&owner));
            state.owner_cleanups.remove(&key);
        }
        let result = self
            .abort_and_close(
                Some(&owner),
                Arc::new(TerminalError::new(
                    "PTY owner is no longer live",
                    TerminalErrorCode::OwnerNotLive,
                )),
                "PTY owner disposed",
            )
            .await;
        self.state.lock().reserved_names.remove(&key);
        result
    }

    async fn abort_and_close(
        &self,
        owner: Option<&Arc<Agent>>,
        abort_reason: Arc<TerminalError>,
        close_reason: &str,
    ) -> TerminalResult<()> {
        let mut failures = Vec::new();
        if let Err(error) = self.abort_pending_spawns(owner, abort_reason).await {
            failures.push(error);
        }
        let records = self
            .state
            .lock()
            .sessions
            .values()
            .filter(|record| owner.is_none_or(|owner| Arc::ptr_eq(&record.owner, owner)))
            .cloned()
            .collect::<Vec<_>>();
        if let Err(error) = self.close_records(records, close_reason).await {
            failures.push(error);
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(TerminalFailure::new(TerminalAggregateError::new(
                "failed to clean up PTY lifecycle",
                failures,
            )))
        }
    }

    async fn dispose_all(self: &Arc<Self>) -> TerminalResult<()> {
        {
            let _entry = self.entry_gate.lock().await;
            self.state.lock().disposing = true;
        }
        let lifecycle_result = self
            .abort_and_close(
                None,
                Arc::new(TerminalError::new(
                    "PTY service is disposing",
                    TerminalErrorCode::ServiceDisposing,
                )),
                "PTY service disposed",
            )
            .await;
        let cleanups = {
            let mut state = self.state.lock();
            state.backends.clear();
            state.reserved_names.clear();
            state.pending_spawns.clear();
            state
                .owner_cleanups
                .drain()
                .map(|(_, cleanup)| cleanup)
                .collect::<Vec<_>>()
        };
        let cleanup_results = futures::future::join_all(
            cleanups
                .iter()
                .map(seekdeep_cordis::fiber::EffectHandle::dispose),
        )
        .await;
        if let Some(error) = cleanup_results.into_iter().find_map(Result::err) {
            return Err(TerminalFailure::message(error.to_string()));
        }
        lifecycle_result
    }

    async fn close_records(
        &self,
        records: Vec<Arc<SessionRecord>>,
        reason: &str,
    ) -> TerminalResult<()> {
        let operations = records
            .iter()
            .map(|record| {
                let fence = {
                    let mut lifecycle = record.lifecycle.lock();
                    lifecycle.closing.clone().unwrap_or_else(|| {
                        let fence =
                            Arc::new(CloseFence::new(record.session.clone(), reason.to_owned()));
                        lifecycle.closing = Some(fence.clone());
                        fence
                    })
                };
                (record.clone(), fence)
            })
            .collect::<Vec<_>>();
        let results = futures::future::join_all(
            operations
                .iter()
                .map(|(_, fence)| async move { fence.wait().await }),
        )
        .await;
        let mut failures = Vec::new();
        for ((record, fence), result) in operations.into_iter().zip(results) {
            match result {
                Ok(()) => self.remove_exact_session(&record.id, &record),
                Err(error) => {
                    let mut lifecycle = record.lifecycle.lock();
                    if lifecycle
                        .closing
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &fence))
                    {
                        lifecycle.closing = None;
                    }
                    failures.push(error);
                }
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(TerminalFailure::new(TerminalAggregateError::new(
                format!("failed to close {} PTY session(s)", failures.len()),
                failures,
            )))
        }
    }

    fn remove_exact_session(&self, id: &TerminalSessionId, record: &Arc<SessionRecord>) {
        let mut state = self.state.lock();
        if state
            .sessions
            .get(id)
            .is_some_and(|current| Arc::ptr_eq(current, record))
        {
            state.sessions.shift_remove(id);
        }
    }
}

fn terminal_error(message: impl Into<Arc<str>>, code: TerminalErrorCode) -> TerminalFailure {
    TerminalFailure::new(TerminalError::new(message, code))
}

fn throw_if_aborted(signal: &AbortSignal) -> TerminalResult<()> {
    if signal.is_aborted() {
        Err(abort_failure(signal))
    } else {
        Ok(())
    }
}

/// Recovers the exact typed cancellation reason, or wraps its JSON representation.
#[must_use]
pub fn abort_failure(signal: &AbortSignal) -> TerminalFailure {
    if let Some(failure) = signal.typed_reason::<TerminalFailure>() {
        return (*failure).clone();
    }
    if let Some(error) = signal.typed_reason::<TerminalError>() {
        return TerminalFailure::from_arc(error);
    }
    TerminalFailure::new(TerminalAbortReason(signal.reason().unwrap_or(Value::Null)))
}

#[cfg(test)]
mod tests;
