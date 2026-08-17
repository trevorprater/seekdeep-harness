//! Two-phase live-agent registry with exactly paired lifecycle notifications.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventArgs, ServiceKey, fiber::EffectHandle};
use seekdeep_core::session::SessionId;
use seekdeep_scope::scope_target;
use thiserror::Error;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::Agent;

/// Typed Cordis slot corresponding to `ctx.agents`.
pub const AGENTS: ServiceKey<AgentRegistry> = ServiceKey::new("agents");

/// Payload for `agent/created` and `agent/disposed`.
#[derive(Clone, Debug)]
pub struct AgentLifecycleEvent {
    /// Exact lifecycle subject.
    pub agent: Arc<Agent>,
}

#[derive(Debug, Default)]
struct EntryState {
    announced: bool,
    announcing: bool,
    detach_requested: bool,
}

#[derive(Debug)]
struct AgentEntry {
    id: SessionId,
    agent: Arc<Agent>,
    owner: Option<Arc<Agent>>,
    state: Mutex<EntryState>,
}

struct RegistryInner {
    id: Uuid,
    context: Context,
    store: Mutex<IndexMap<SessionId, Arc<AgentEntry>>>,
    initiators: Arc<InitiatorTracker>,
}

impl std::fmt::Debug for RegistryInner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegistryInner")
            .field("entries", &self.store.lock().len())
            .finish_non_exhaustive()
    }
}

/// Agent registry publication failure.
#[derive(Debug, Error)]
pub enum AgentRegistryError {
    /// Agent and session identities diverged before publication.
    #[error("agent id \"{agent_id}\" does not match session id \"{session_id}\"")]
    IdentityMismatch {
        /// Agent-facing identity.
        agent_id: SessionId,
        /// Durable session identity.
        session_id: SessionId,
    },
    /// Another exact lifecycle currently owns the identity.
    #[error("agent \"{0}\" is already registered")]
    Duplicate(SessionId),
    /// The subject is absent or a different same-ID instance is live.
    #[error("agent \"{0}\" is not live in this registry")]
    NotLive(SessionId),
    /// Creation announcement already began.
    #[error("agent \"{0}\" was already announced")]
    AlreadyAnnounced(SessionId),
    /// Cordis rejected lifecycle effect ownership.
    #[error(transparent)]
    Cordis(#[from] seekdeep_cordis::CordisError),
    /// A synchronous creation observer vetoed publication.
    #[error(transparent)]
    Announcement(#[from] anyhow::Error),
    /// No process-local initiating agent is active.
    #[error("no initiating agent is active")]
    NoInitiator,
    /// The registry no longer accepts or exposes initiator scopes.
    #[error("agent initiator scope is disposed")]
    InitiatorDisposed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InitiatorLifecycle {
    Active,
    Closing,
    Disposed,
}

#[derive(Debug)]
struct InitiatorState {
    lifecycle: InitiatorLifecycle,
    active_runs: usize,
}

#[derive(Debug)]
struct InitiatorTracker {
    state: Mutex<InitiatorState>,
    changed: Notify,
}

#[derive(Debug)]
struct InitiatorRun {
    active: AtomicBool,
    tracker: Arc<InitiatorTracker>,
    parent: Option<Arc<Self>>,
}

impl InitiatorRun {
    fn release(&self) {
        if !self.active.swap(false, Ordering::AcqRel) {
            return;
        }
        let notify = {
            let mut state = self.tracker.state.lock();
            state.active_runs = state.active_runs.saturating_sub(1);
            state.active_runs == 0
        };
        if notify {
            self.tracker.changed.notify_waiters();
        }
    }
}

impl Drop for InitiatorRun {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Clone)]
struct InitiatorBinding {
    registry: Uuid,
    agent: Option<Arc<Agent>>,
    run: Arc<InitiatorRun>,
}

tokio::task_local! {
    static INITIATOR_BINDING: InitiatorBinding;
}

/// Identity-bound, single-shot capability for removing one exact entry.
#[derive(Clone, Debug)]
pub struct AgentDetach {
    registry: Arc<RegistryInner>,
    entry: Arc<AgentEntry>,
    entered: Arc<AtomicBool>,
}

impl AgentDetach {
    /// Removes this exact lifecycle at most once.
    ///
    /// A request made from a synchronous creation listener is deferred until
    /// the complete captured creation dispatch unwinds.
    pub fn detach(&self) {
        if self.entered.swap(false, Ordering::AcqRel) {
            self.registry.request_detach(&self.entry);
        }
    }
}

impl RegistryInner {
    fn request_detach(self: &Arc<Self>, entry: &Arc<AgentEntry>) {
        {
            let mut state = entry.state.lock();
            if state.announcing {
                state.detach_requested = true;
                return;
            }
        }
        self.detach_entered(entry);
    }

    fn detach_entered(self: &Arc<Self>, entry: &Arc<AgentEntry>) {
        let announced = {
            let mut store = self.store.lock();
            let Some(current) = store.get(&entry.id) else {
                return;
            };
            if !Arc::ptr_eq(current, entry) {
                return;
            }
            store.shift_remove(&entry.id);
            let mut state = entry.state.lock();
            state.detach_requested = false;
            state.announced
        };
        if announced {
            self.emit_disposed(entry);
        }
    }

    fn emit_disposed(&self, entry: &AgentEntry) {
        let dispatch = scope_target(&self.context, Some(entry.agent.scope_key()));
        let args = EventArgs::one(AgentLifecycleEvent {
            agent: entry.agent.clone(),
        });
        match self
            .context
            .events()
            .prepare_emit(&dispatch, "agent/disposed", &args)
        {
            Ok(emission) => emission.emit_contained(|error| {
                tracing::warn!(agent = %entry.id, %error, "agent/disposed listener failed");
            }),
            Err(error) => {
                tracing::warn!(agent = %entry.id, %error, "agent/disposed dispatch failed");
            }
        }
    }
}

/// Insertion-ordered registry of exact live agent instances.
#[derive(Clone, Debug)]
pub struct AgentRegistry {
    inner: Arc<RegistryInner>,
}

impl AgentRegistry {
    /// Creates a registry whose lifecycle events dispatch through `context`.
    #[must_use]
    pub fn new(context: Context) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                id: Uuid::now_v7(),
                context,
                store: Mutex::new(IndexMap::new()),
                initiators: Arc::new(InitiatorTracker {
                    state: Mutex::new(InitiatorState {
                        lifecycle: InitiatorLifecycle::Active,
                        active_runs: 0,
                    }),
                    changed: Notify::new(),
                }),
            }),
        }
    }

    /// Publishes this registry on the exact `agents` service slot.
    ///
    /// # Errors
    ///
    /// Returns ordinary duplicate-service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(AGENTS, self.clone())
    }

    /// Returns the initiating agent inherited by the current asynchronous
    /// foreground chain, or `None` outside (or inside a clearing boundary).
    ///
    /// # Errors
    ///
    /// Returns after initiator lifecycle disposal invalidates retained registry
    /// references.
    pub fn current_initiator(&self) -> Result<Option<Arc<Agent>>, AgentRegistryError> {
        if self.inner.initiators.state.lock().lifecycle == InitiatorLifecycle::Disposed {
            return Err(AgentRegistryError::InitiatorDisposed);
        }
        Ok(INITIATOR_BINDING
            .try_with(|binding| {
                (binding.registry == self.inner.id)
                    .then(|| binding.agent.clone())
                    .flatten()
            })
            .ok()
            .flatten())
    }

    /// Returns the initiating agent or rejects agentless execution.
    ///
    /// # Errors
    ///
    /// Returns when no initiator is active or the lifecycle was disposed.
    pub fn require_initiator(&self) -> Result<Arc<Agent>, AgentRegistryError> {
        self.current_initiator()?
            .ok_or(AgentRegistryError::NoInitiator)
    }

    /// Runs one returned asynchronous foreground lifetime with exact initiating
    /// agent attribution. Nested scopes restore their parent on completion.
    ///
    /// # Errors
    ///
    /// Rejects new boundaries once initiator teardown starts.
    pub async fn scope_initiator<F, T>(
        &self,
        agent: Arc<Agent>,
        operation: F,
    ) -> Result<T, AgentRegistryError>
    where
        F: std::future::Future<Output = T>,
    {
        let binding = self.begin_initiator(Some(agent))?;
        Ok(INITIATOR_BINDING.scope(binding, operation).await)
    }

    /// Runs one asynchronous foreground lifetime while deliberately clearing
    /// inherited initiator attribution.
    ///
    /// # Errors
    ///
    /// Rejects new boundaries once initiator teardown starts.
    pub async fn scope_without_initiator<F, T>(&self, operation: F) -> Result<T, AgentRegistryError>
    where
        F: std::future::Future<Output = T>,
    {
        let binding = self.begin_initiator(None)?;
        Ok(INITIATOR_BINDING.scope(binding, operation).await)
    }

    /// Runs a synchronous operation with exact initiating-agent attribution.
    ///
    /// # Errors
    ///
    /// Rejects new boundaries once initiator teardown starts.
    pub fn with_initiator<T>(
        &self,
        agent: Arc<Agent>,
        operation: impl FnOnce() -> T,
    ) -> Result<T, AgentRegistryError> {
        let binding = self.begin_initiator(Some(agent))?;
        Ok(INITIATOR_BINDING.sync_scope(binding, operation))
    }

    /// Runs a synchronous operation with inherited attribution cleared.
    ///
    /// # Errors
    ///
    /// Rejects new boundaries once initiator teardown starts.
    pub fn without_initiator<T>(
        &self,
        operation: impl FnOnce() -> T,
    ) -> Result<T, AgentRegistryError> {
        let binding = self.begin_initiator(None)?;
        Ok(INITIATOR_BINDING.sync_scope(binding, operation))
    }

    fn begin_initiator(
        &self,
        agent: Option<Arc<Agent>>,
    ) -> Result<InitiatorBinding, AgentRegistryError> {
        {
            let mut state = self.inner.initiators.state.lock();
            if state.lifecycle != InitiatorLifecycle::Active {
                return Err(AgentRegistryError::InitiatorDisposed);
            }
            state.active_runs += 1;
        }
        let parent = INITIATOR_BINDING
            .try_with(|binding| (binding.registry == self.inner.id).then(|| binding.run.clone()))
            .ok()
            .flatten();
        Ok(InitiatorBinding {
            registry: self.inner.id,
            agent,
            run: Arc::new(InitiatorRun {
                active: AtomicBool::new(true),
                tracker: self.inner.initiators.clone(),
                parent,
            }),
        })
    }

    /// Rejects new initiator boundaries while existing foreground lifetimes
    /// remain readable and drain.
    pub fn close_initiators(&self) {
        let mut state = self.inner.initiators.state.lock();
        if state.lifecycle == InitiatorLifecycle::Active {
            state.lifecycle = InitiatorLifecycle::Closing;
        }
    }

    /// Closes initiator creation, drains every non-reentrant returned lifetime,
    /// and invalidates retained registry references.
    pub async fn dispose_initiators(&self) {
        self.close_initiators();
        self.release_reentrant_initiators();
        loop {
            let notified = self.inner.initiators.changed.notified();
            if self.inner.initiators.state.lock().active_runs == 0 {
                break;
            }
            notified.await;
        }
        self.inner.initiators.state.lock().lifecycle = InitiatorLifecycle::Disposed;
        self.inner.initiators.changed.notify_waiters();
    }

    fn release_reentrant_initiators(&self) {
        let mut run = INITIATOR_BINDING
            .try_with(|binding| (binding.registry == self.inner.id).then(|| binding.run.clone()))
            .ok()
            .flatten();
        while let Some(current) = run.take() {
            current.release();
            run.clone_from(&current.parent);
        }
    }

    /// Inserts a prepared agent without announcing it.
    ///
    /// # Errors
    ///
    /// Rejects identity mismatch or a live same-ID entry.
    pub fn enter(
        &self,
        agent: Arc<Agent>,
        owner: Option<Arc<Agent>>,
    ) -> Result<AgentDetach, AgentRegistryError> {
        if agent.id() != agent.session().id() {
            return Err(AgentRegistryError::IdentityMismatch {
                agent_id: agent.id().clone(),
                session_id: agent.session().id().clone(),
            });
        }
        let entry = Arc::new(AgentEntry {
            id: agent.id().clone(),
            agent,
            owner,
            state: Mutex::new(EntryState::default()),
        });
        let mut store = self.inner.store.lock();
        if store.contains_key(&entry.id) {
            return Err(AgentRegistryError::Duplicate(entry.id.clone()));
        }
        store.insert(entry.id.clone(), entry.clone());
        drop(store);
        Ok(AgentDetach {
            registry: self.inner.clone(),
            entry,
            entered: Arc::new(AtomicBool::new(true)),
        })
    }

    /// Announces one exact entered agent.
    ///
    /// # Errors
    ///
    /// Rejects absent/replaced subjects, duplicate/reentrant announcements,
    /// and synchronous creation-listener vetoes.
    pub fn announce(&self, agent: &Arc<Agent>) -> Result<(), AgentRegistryError> {
        let entry = {
            let store = self.inner.store.lock();
            let Some(entry) = store.get(agent.id()).cloned() else {
                return Err(AgentRegistryError::NotLive(agent.id().clone()));
            };
            if !Arc::ptr_eq(&entry.agent, agent) {
                return Err(AgentRegistryError::NotLive(agent.id().clone()));
            }
            entry
        };
        {
            let mut state = entry.state.lock();
            if state.announced || state.announcing {
                return Err(AgentRegistryError::AlreadyAnnounced(entry.id.clone()));
            }
            state.announcing = true;
            state.announced = true;
        }

        let dispatch = scope_target(&self.inner.context, Some(entry.agent.scope_key()));
        let args = EventArgs::one(AgentLifecycleEvent {
            agent: entry.agent.clone(),
        });
        let result = self
            .inner
            .context
            .events()
            .prepare_emit(&dispatch, "agent/created", &args)
            .and_then(seekdeep_cordis::PreparedEmission::emit);

        let detach_requested = {
            let mut state = entry.state.lock();
            state.announcing = false;
            state.detach_requested
        };
        if detach_requested {
            self.inner.detach_entered(&entry);
        }
        result.map_err(AgentRegistryError::Announcement)
    }

    /// Registers, announces, and lifecycle-owns a prepared agent.
    ///
    /// # Errors
    ///
    /// Rolls the visible entry back on ownership or announcement failure.
    pub fn register(
        &self,
        owner_context: &Context,
        agent: &Arc<Agent>,
        owner: Option<Arc<Agent>>,
    ) -> Result<EffectHandle, AgentRegistryError> {
        let detach = self.enter(agent.clone(), owner)?;
        let disposer = detach.clone();
        let effect = EffectHandle::synchronous("agents.register()", move || {
            disposer.detach();
            Ok(())
        });
        if let Err(error) = owner_context.own(effect.clone()) {
            detach.detach();
            return Err(error.into());
        }
        if let Err(error) = self.announce(agent) {
            detach.detach();
            return Err(error);
        }
        Ok(effect)
    }

    /// Resolves a live agent by shared session identity.
    #[must_use]
    pub fn get(&self, id: &SessionId) -> Option<Arc<Agent>> {
        self.inner
            .store
            .lock()
            .get(id)
            .map(|entry| entry.agent.clone())
    }

    /// Tests exact runtime creator ownership.
    #[must_use]
    pub fn is_owned_by(&self, id: &SessionId, owner: &Arc<Agent>) -> bool {
        self.inner
            .store
            .lock()
            .get(id)
            .and_then(|entry| entry.owner.as_ref())
            .is_some_and(|actual| Arc::ptr_eq(actual, owner))
    }

    /// Returns all live agents in registration order.
    #[must_use]
    pub fn list(&self) -> Vec<Arc<Agent>> {
        self.inner
            .store
            .lock()
            .values()
            .map(|entry| entry.agent.clone())
            .collect()
    }

    /// Returns all live agents without a runtime creator, in registration order.
    #[must_use]
    pub fn roots(&self) -> Vec<Arc<Agent>> {
        self.inner
            .store
            .lock()
            .values()
            .filter(|entry| entry.owner.is_none())
            .map(|entry| entry.agent.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use parking_lot::Mutex;
    use seekdeep_cordis::{EventOptions, EventReply};
    use seekdeep_core::session::{Session, SessionId};
    use seekdeep_scope::ScopeKey;

    use crate::{InboxNotifications, inbox::NoopInboxNotifications};

    use super::*;

    fn agent(context: &Context, id: &str) -> Arc<Agent> {
        let id = SessionId::new(id);
        let session = Session::create(&id, None, None).expect("session");
        let notifications: Arc<dyn InboxNotifications> = Arc::new(NoopInboxNotifications);
        let inbox = Arc::new(crate::Inbox::new(session.clone(), notifications).expect("inbox"));
        Arc::new(Agent::new(
            id,
            crate::AgentOptions::default(),
            session,
            inbox,
            context.clone(),
            ScopeKey::new(),
        ))
    }

    #[tokio::test]
    async fn registers_exact_entries_and_pairs_lifecycle() {
        let context = Context::new();
        let registry = AgentRegistry::new(context.clone());
        let lifecycle = Arc::new(Mutex::new(Vec::new()));
        for (name, label) in [("agent/created", "created"), ("agent/disposed", "disposed")] {
            let lifecycle = lifecycle.clone();
            context
                .events()
                .on_sync(
                    &context,
                    name,
                    move |_, args| {
                        let payload = args.get::<AgentLifecycleEvent>(0).expect("payload");
                        lifecycle
                            .lock()
                            .push(format!("{label}:{}", payload.agent.id()));
                        Ok(EventReply::Undefined)
                    },
                    EventOptions::default(),
                )
                .expect("listener");
        }
        let first = agent(&context, "a1");
        let effect = registry.register(&context, &first, None).expect("register");
        assert!(Arc::ptr_eq(
            &registry.get(first.id()).expect("live"),
            &first
        ));
        assert!(matches!(
            registry.enter(agent(&context, "a1"), None),
            Err(AgentRegistryError::Duplicate(_))
        ));
        effect.dispose().await.expect("dispose");
        assert!(registry.get(first.id()).is_none());
        assert_eq!(*lifecycle.lock(), ["created:a1", "disposed:a1"]);
    }

    #[test]
    fn rejects_identity_mismatch() {
        let context = Context::new();
        let registry = AgentRegistry::new(context.clone());
        let original = agent(&context, "agent-id");
        let session_id = SessionId::new("session-id");
        let session = Session::create(&session_id, None, None).expect("session");
        let notifications: Arc<dyn InboxNotifications> = Arc::new(NoopInboxNotifications);
        let inbox = Arc::new(crate::Inbox::new(session.clone(), notifications).expect("inbox"));
        let mismatched = Arc::new(Agent::new(
            original.id().clone(),
            crate::AgentOptions::default(),
            session,
            inbox,
            context,
            ScopeKey::new(),
        ));
        assert!(matches!(
            registry.enter(mismatched, None),
            Err(AgentRegistryError::IdentityMismatch { .. })
        ));
    }

    #[test]
    fn tracks_runtime_ownership_and_order() {
        let context = Context::new();
        let registry = AgentRegistry::new(context.clone());
        let root = agent(&context, "root");
        let child = agent(&context, "child");
        let root_detach = registry.enter(root.clone(), None).expect("root");
        registry.announce(&root).expect("announce root");
        let child_detach = registry
            .enter(child.clone(), Some(root.clone()))
            .expect("child");
        registry.announce(&child).expect("announce child");
        assert!(Arc::ptr_eq(&registry.list()[0], &root));
        assert!(Arc::ptr_eq(&registry.list()[1], &child));
        assert_eq!(registry.roots().len(), 1);
        assert!(registry.is_owned_by(child.id(), &root));
        child_detach.detach();
        root_detach.detach();
    }

    #[test]
    fn veto_rolls_register_back_and_pairs_partial_creation() {
        let context = Context::new();
        let registry = AgentRegistry::new(context.clone());
        let lifecycle = Arc::new(Mutex::new(Vec::new()));
        let first_seen = lifecycle.clone();
        context
            .events()
            .on_sync(
                &context,
                "agent/created",
                move |_, _| {
                    first_seen.lock().push("created");
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .expect("first");
        context
            .events()
            .on_sync(
                &context,
                "agent/created",
                |_, _| anyhow::bail!("creation veto"),
                EventOptions::default(),
            )
            .expect("veto");
        let disposed = lifecycle.clone();
        context
            .events()
            .on_sync(
                &context,
                "agent/disposed",
                move |_, _| {
                    disposed.lock().push("disposed");
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .expect("disposed");
        let subject = agent(&context, "vetoed");
        let error = registry
            .register(&context, &subject, None)
            .expect_err("veto");
        assert!(error.to_string().contains("creation veto"));
        assert!(registry.get(subject.id()).is_none());
        assert_eq!(*lifecycle.lock(), ["created", "disposed"]);
    }

    #[test]
    fn stale_detach_cannot_remove_replacement() {
        let context = Context::new();
        let registry = AgentRegistry::new(context.clone());
        let first = agent(&context, "split");
        let stale = registry.enter(first.clone(), None).expect("first");
        registry.announce(&first).expect("announce");
        stale.detach();
        let replacement = agent(&context, "split");
        let current = registry
            .enter(replacement.clone(), None)
            .expect("replacement");
        stale.detach();
        assert!(Arc::ptr_eq(
            &registry.get(replacement.id()).expect("live"),
            &replacement
        ));
        current.detach();
    }

    #[test]
    fn defers_detach_until_creation_dispatch_unwinds() {
        let context = Context::new();
        let registry = AgentRegistry::new(context.clone());
        let subject = agent(&context, "reentrant");
        let detach = registry.enter(subject.clone(), None).expect("enter");
        let order = Arc::new(Mutex::new(Vec::new()));
        let first_registry = registry.clone();
        let first_subject = subject.clone();
        let first_detach = detach.clone();
        let first_order = order.clone();
        context
            .events()
            .on_sync(
                &context,
                "agent/created",
                move |_, _| {
                    first_order
                        .lock()
                        .push(first_registry.get(first_subject.id()).is_some());
                    first_detach.detach();
                    first_order
                        .lock()
                        .push(first_registry.get(first_subject.id()).is_some());
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .expect("first");
        let second_registry = registry.clone();
        let second_subject = subject.clone();
        let second_order = order.clone();
        context
            .events()
            .on_sync(
                &context,
                "agent/created",
                move |_, _| {
                    second_order
                        .lock()
                        .push(second_registry.get(second_subject.id()).is_some());
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .expect("second");
        registry.announce(&subject).expect("announce");
        assert_eq!(*order.lock(), [true, true, true]);
        assert!(registry.get(subject.id()).is_none());
    }

    #[tokio::test]
    async fn overlapping_initiator_scopes_keep_exact_identity_and_restore_nesting() {
        let context = Context::new();
        let registry = AgentRegistry::new(context.clone());
        let first = agent(&context, "first");
        let second = agent(&context, "second");
        assert!(registry.current_initiator().expect("readable").is_none());
        assert!(matches!(
            registry.require_initiator(),
            Err(AgentRegistryError::NoInitiator)
        ));

        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let first_task = {
            let registry = registry.clone();
            let first = first.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                let expected = first.clone();
                registry
                    .scope_initiator(first, async {
                        let before = registry.require_initiator().expect("first before");
                        barrier.wait().await;
                        tokio::task::yield_now().await;
                        let after = registry.require_initiator().expect("first after");
                        (before, after, expected)
                    })
                    .await
                    .expect("first scope")
            })
        };
        let second_task = {
            let registry = registry.clone();
            let second = second.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                let expected = second.clone();
                registry
                    .scope_initiator(second, async {
                        let before = registry.require_initiator().expect("second before");
                        barrier.wait().await;
                        tokio::task::yield_now().await;
                        let after = registry.require_initiator().expect("second after");
                        (before, after, expected)
                    })
                    .await
                    .expect("second scope")
            })
        };
        for (before, after, expected) in [
            first_task.await.expect("first join"),
            second_task.await.expect("second join"),
        ] {
            assert!(Arc::ptr_eq(&before, &expected));
            assert!(Arc::ptr_eq(&after, &expected));
        }

        registry
            .scope_initiator(first.clone(), async {
                assert!(Arc::ptr_eq(
                    &registry.require_initiator().expect("outer"),
                    &first
                ));
                registry
                    .scope_without_initiator(async {
                        assert!(registry.current_initiator().expect("cleared").is_none());
                    })
                    .await
                    .expect("clear");
                assert!(Arc::ptr_eq(
                    &registry.require_initiator().expect("restored"),
                    &first
                ));
            })
            .await
            .expect("outer scope");
        assert!(registry.current_initiator().expect("outside").is_none());
    }

    #[tokio::test]
    async fn initiator_disposal_drains_foreground_and_invalidates_retained_registry() {
        let context = Context::new();
        let registry = AgentRegistry::new(context.clone());
        let subject = agent(&context, "draining");
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let scoped_registry = registry.clone();
        let scope = tokio::spawn(async move {
            scoped_registry
                .scope_initiator(subject, async {
                    entered_tx.send(()).ok();
                    let _ = release_rx.await;
                    scoped_registry
                        .require_initiator()
                        .expect("readable while closing")
                })
                .await
        });
        entered_rx.await.expect("entered");
        let disposing_registry = registry.clone();
        let disposal = tokio::spawn(async move {
            disposing_registry.dispose_initiators().await;
        });
        tokio::task::yield_now().await;
        assert!(!disposal.is_finished());
        assert!(matches!(
            registry
                .scope_without_initiator(async {})
                .await
                .expect_err("closing rejects"),
            AgentRegistryError::InitiatorDisposed
        ));
        release_tx.send(()).expect("release");
        let observed = scope.await.expect("scope join").expect("scope result");
        assert_eq!(observed.id().as_str(), "draining");
        tokio::time::timeout(Duration::from_secs(1), disposal)
            .await
            .expect("disposal timeout")
            .expect("disposal join");
        assert!(matches!(
            registry.current_initiator(),
            Err(AgentRegistryError::InitiatorDisposed)
        ));
    }

    #[tokio::test]
    async fn reentrant_initiator_disposal_excludes_its_own_parent_chain() {
        let context = Context::new();
        let registry = AgentRegistry::new(context.clone());
        let subject = agent(&context, "reentrant-dispose");
        let nested_registry = registry.clone();
        tokio::time::timeout(
            Duration::from_secs(1),
            registry.scope_initiator(subject, async move {
                nested_registry
                    .scope_without_initiator(async {
                        nested_registry.dispose_initiators().await;
                    })
                    .await
                    .expect("nested clear");
            }),
        )
        .await
        .expect("must not self-deadlock")
        .expect("outer scope");
        assert!(matches!(
            registry.current_initiator(),
            Err(AgentRegistryError::InitiatorDisposed)
        ));
    }
}
