//! Public agent types shared by registries, drivers, and integrations.

use std::{
    future::Future,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use futures::future::BoxFuture;
use parking_lot::RwLock;
use seekdeep_cordis::{Context, ServiceKey};
use seekdeep_core::session::{AgentCancelCause, Session, SessionId};
use seekdeep_llm::{AbortSignal, ModelId, ProviderId, UserMessage};
use seekdeep_scope::ScopeKey;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{Inbox, InboxTarget};

/// Agent-scoped service key corresponding to the source context's `ctx.agent`.
pub const AGENT: ServiceKey<Agent> = ServiceKey::new("agent");

/// Cancellation modifiers for [`Agent::cancel`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CancelOptions {
    /// Preserve queued and steering input for later work.
    pub keep_inbox: bool,
}

/// Live-agent controller rejection.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum AgentControlError {
    /// The loop implementation has not attached its controller yet.
    #[error("agent controller is not installed")]
    ControllerMissing,
    /// The exact agent already owns driver or maintenance work.
    #[error("agent \"{0}\" already has active work")]
    ActiveWork(SessionId),
    /// A second controller attempted to claim the agent.
    #[error("agent controller is already installed")]
    ControllerAlreadyInstalled,
    /// The controller rejected an operation after teardown.
    #[error("agent \"{0}\" is disposed")]
    Disposed(SessionId),
    /// Durable inbox mutation failed.
    #[error("{0}")]
    Inbox(String),
}

/// Reservation returned when maintenance atomically claims the idle phase.
pub struct MaintenanceReservation {
    signal: AbortSignal,
    finish: Arc<dyn Fn() + Send + Sync>,
    active: AtomicBool,
}

impl std::fmt::Debug for MaintenanceReservation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MaintenanceReservation")
            .field("signal", &self.signal)
            .field("active", &self.active.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl MaintenanceReservation {
    /// Builds a reservation around a controller-owned completion callback.
    #[must_use]
    pub fn new(signal: AbortSignal, finish: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            signal,
            finish,
            active: AtomicBool::new(true),
        }
    }

    /// Signal cancelled by [`Agent::cancel`] while maintenance owns the phase.
    #[must_use]
    pub fn signal(&self) -> AbortSignal {
        self.signal.clone()
    }

    /// Releases maintenance exactly once.
    pub fn finish(&self) {
        if self.active.swap(false, Ordering::AcqRel) {
            (self.finish)();
        }
    }
}

impl Drop for MaintenanceReservation {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Object-safe loop controller installed into a public [`Agent`].
pub trait AgentController: Send + Sync + 'static {
    /// Routes identified input to the requested boundary.
    ///
    /// # Errors
    ///
    /// Returns after controller disposal or another lifecycle rejection.
    fn send(
        &self,
        message: UserMessage,
        target: InboxTarget,
        wakeup: bool,
    ) -> Result<(), AgentControlError>;
    /// Cancels the current activity, if any.
    ///
    /// # Errors
    ///
    /// Returns durable inbox failures or disposal rejection.
    fn cancel(
        &self,
        cause: AgentCancelCause,
        options: CancelOptions,
    ) -> Result<(), AgentControlError>;
    /// Resolves after the complete current/replacement activity converges idle.
    fn when_idle(&self) -> BoxFuture<'static, ()>;
    /// Atomically reserves the true idle phase for maintenance.
    ///
    /// # Errors
    ///
    /// Rejects when another activity owns the agent or it is disposed.
    fn begin_maintenance(&self) -> Result<MaintenanceReservation, AgentControlError>;
}

/// Merge-extensible per-agent model selection options.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentOptions {
    /// Provider route resolved at request time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ProviderId>,
    /// Provider-specific model identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelId>,
    /// Maximum output tokens per conversation-model request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Delegation depth: zero for a top-level agent and parent depth + 1 for a child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_depth: Option<u64>,
}

/// Observable whole-agent driver state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    /// No driver is active.
    #[default]
    Idle,
    /// A driver owns cancellable work.
    Running,
}

/// Whether and with which messages the loop enters a proposed step.
#[derive(Clone, Debug, PartialEq)]
pub enum PreStepDecision {
    /// Close the turn as blocked without opening the step.
    Reject,
    /// Enter the step with this possibly replaced message batch.
    Enter {
        /// Messages committed at step entry.
        messages: Vec<UserMessage>,
    },
}

/// Action returned by a listener that owns model-request recovery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestErrorAction {
    /// Retry the request inside the same durable step.
    Retry,
    /// Leave the failure terminal.
    Terminal,
}

/// Why an agent session lifecycle began.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStartSource {
    /// Fresh or seeded startup.
    Startup,
    /// Persisted session reconstruction.
    Resume,
    /// Explicit clear/restart.
    Clear,
    /// Compaction-created lifecycle.
    Compact,
}

/// Public live-agent handle.
///
/// Driving operations are installed by the agent-loop layer; this core value
/// owns the stable identity, durable session, inbox, scoped context, options,
/// and status observed by every other package.
pub struct Agent {
    id: SessionId,
    options: AgentOptions,
    session: Arc<Session>,
    inbox: Arc<Inbox>,
    status: RwLock<AgentStatus>,
    context: Context,
    scope_key: ScopeKey,
    controller: OnceLock<Arc<dyn AgentController>>,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Agent")
            .field("id", &self.id)
            .field("options", &self.options)
            .field("status", &self.status())
            .field("scope_key", &self.scope_key)
            .finish_non_exhaustive()
    }
}

impl Agent {
    /// Constructs an already-composed live agent value.
    ///
    /// Identity equality with the session is deliberately checked at registry
    /// entry, the authoritative publication collision boundary.
    #[must_use]
    pub fn new(
        id: SessionId,
        options: AgentOptions,
        session: Arc<Session>,
        inbox: Arc<Inbox>,
        context: Context,
        scope_key: ScopeKey,
    ) -> Self {
        Self {
            id,
            options,
            session,
            inbox,
            status: RwLock::new(AgentStatus::Idle),
            context,
            scope_key,
            controller: OnceLock::new(),
        }
    }

    /// Single identity shared with the durable session.
    #[must_use]
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// Provider and model configuration.
    #[must_use]
    pub fn options(&self) -> &AgentOptions {
        &self.options
    }

    /// Durable source-of-truth session.
    #[must_use]
    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    /// Durable pending-input projection.
    #[must_use]
    pub fn inbox(&self) -> &Arc<Inbox> {
        &self.inbox
    }

    /// Current observable lifecycle state.
    #[must_use]
    pub fn status(&self) -> AgentStatus {
        *self.status.read()
    }

    /// Agent-owned scoped context.
    #[must_use]
    pub fn context(&self) -> &Context {
        &self.context
    }

    /// Opaque key coupling the agent subject to scoped routing.
    #[must_use]
    pub fn scope_key(&self) -> ScopeKey {
        self.scope_key
    }

    /// Updates the status at the loop's transition commit point.
    pub fn set_status(&self, status: AgentStatus) {
        *self.status.write() = status;
    }

    /// Installs the single loop controller before publication.
    ///
    /// # Errors
    ///
    /// Rejects a second controller.
    pub fn install_controller(
        &self,
        controller: Arc<dyn AgentController>,
    ) -> Result<(), AgentControlError> {
        self.controller
            .set(controller)
            .map_err(|_| AgentControlError::ControllerAlreadyInstalled)
    }

    fn controller(&self) -> Result<&Arc<dyn AgentController>, AgentControlError> {
        self.controller
            .get()
            .ok_or(AgentControlError::ControllerMissing)
    }

    /// Routes identified input and optionally wakes the driver.
    ///
    /// # Errors
    ///
    /// Returns controller lifecycle failures.
    pub fn send(
        &self,
        message: UserMessage,
        target: InboxTarget,
        wakeup: bool,
    ) -> Result<(), AgentControlError> {
        self.controller()?.send(message, target, wakeup)
    }

    /// Queues one ordinary follow-up turn and wakes the driver.
    ///
    /// # Errors
    ///
    /// Returns controller lifecycle failures.
    pub fn followup(&self, message: UserMessage) -> Result<(), AgentControlError> {
        self.send(message, InboxTarget::NextTurn, true)
    }

    /// Queues steering at the nearest step boundary and wakes the driver.
    ///
    /// # Errors
    ///
    /// Returns controller lifecycle failures.
    pub fn steer(&self, message: UserMessage) -> Result<(), AgentControlError> {
        self.send(message, InboxTarget::NextStep, true)
    }

    /// Queues model-facing context without waking an idle driver.
    ///
    /// # Errors
    ///
    /// Returns controller lifecycle failures.
    pub fn inject(&self, message: UserMessage) -> Result<(), AgentControlError> {
        self.send(message, InboxTarget::NextStep, false)
    }

    /// Clears pending work unless preserved and aborts the current activity.
    ///
    /// # Errors
    ///
    /// Returns when no controller is installed.
    pub fn cancel(
        &self,
        cause: AgentCancelCause,
        options: CancelOptions,
    ) -> Result<(), AgentControlError> {
        self.controller()?.cancel(cause, options)
    }

    /// Waits until no driver or maintenance activity remains.
    ///
    /// # Errors
    ///
    /// Returns when no controller is installed.
    pub fn when_idle(&self) -> Result<BoxFuture<'static, ()>, AgentControlError> {
        Ok(self.controller()?.when_idle())
    }

    /// Runs one task from the true idle phase.
    ///
    /// The reservation is released even when the returned future is dropped.
    ///
    /// # Errors
    ///
    /// Rejects active work or a missing/disposed controller.
    pub fn run_maintenance<T, Fut>(
        &self,
        job: impl FnOnce(AbortSignal) -> Fut,
    ) -> Result<BoxFuture<'static, T>, AgentControlError>
    where
        T: Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
    {
        let reservation = self.controller()?.begin_maintenance()?;
        let future = job(reservation.signal());
        Ok(Box::pin(async move {
            let result = future.await;
            reservation.finish();
            result
        }))
    }
}
