//! Live agent phase machine, wake latching, cancellation, and maintenance.

use std::{
    panic::AssertUnwindSafe,
    sync::{Arc, OnceLock, Weak},
};

use futures::{FutureExt, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentCancelCause, AgentControlError, AgentController, AgentEvents, AgentOptions,
    AgentStatus, AgentStatusChanged, CancelOptions, Inbox, InboxNotifications, InboxTarget,
    MaintenanceReservation,
};
use seekdeep_cordis::Context;
use seekdeep_core::session::Session;
use seekdeep_llm::{AbortSignal, UserMessage};
use seekdeep_scope::{Scope, ScopeKey, create_scope};
use tokio::sync::Notify;

/// Driver callback invoked for each fresh running reservation.
pub type DriverTask =
    Arc<dyn Fn(Arc<Agent>, Arc<LoopController>) -> BoxFuture<'static, ()> + Send + Sync + 'static>;

/// `agent/inbox/inserted` and `agent/inbox/discarded` payload fields.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentInboxMessage {
    /// Exact inserted or discarded message.
    pub message: UserMessage,
}

/// `agent/inbox/claimed` payload fields.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentInboxClaimed {
    /// Exact claimed message.
    pub message: UserMessage,
    /// Turn that owns the claim.
    pub turn: u64,
}

#[derive(Debug)]
enum Phase {
    Idle {
        last_turn: u64,
    },
    Maintenance {
        signal: AbortSignal,
        last_turn: u64,
        wake_requested: bool,
        activity: u64,
    },
    Running {
        signal: AbortSignal,
        turn: u64,
        step: u64,
        wake_requested: bool,
        activity: u64,
    },
    Disposed {
        last_turn: u64,
    },
}

impl Phase {
    fn status(&self) -> AgentStatus {
        match self {
            Self::Running { .. } => AgentStatus::Running,
            Self::Idle { .. } | Self::Maintenance { .. } | Self::Disposed { .. } => {
                AgentStatus::Idle
            }
        }
    }
}

#[derive(Debug)]
struct ControllerState {
    phase: Phase,
    next_activity: u64,
    completed_activity: u64,
    settling: bool,
}

/// Concrete controller for one public [`Agent`].
pub struct LoopController {
    agent: Weak<Agent>,
    self_weak: Weak<Self>,
    driver: DriverTask,
    state: Mutex<ControllerState>,
    changed: Notify,
}

impl std::fmt::Debug for LoopController {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoopController")
            .field("state", &self.state.lock())
            .finish_non_exhaustive()
    }
}

impl LoopController {
    /// Creates and self-links a controller for an unpublished agent.
    #[must_use]
    pub fn new(agent: Weak<Agent>, last_turn: u64, driver: DriverTask) -> Arc<Self> {
        Arc::new_cyclic(|self_weak| Self {
            agent,
            self_weak: self_weak.clone(),
            driver,
            state: Mutex::new(ControllerState {
                phase: Phase::Idle { last_turn },
                next_activity: 0,
                completed_activity: 0,
                settling: false,
            }),
            changed: Notify::new(),
        })
    }

    /// Current turn/step position while running.
    #[must_use]
    pub fn position(&self) -> Option<(u64, u64)> {
        match &self.state.lock().phase {
            Phase::Running { turn, step, .. } => Some((*turn, *step)),
            Phase::Idle { .. } | Phase::Maintenance { .. } | Phase::Disposed { .. } => None,
        }
    }

    /// Current activity signal while running or in maintenance.
    #[must_use]
    pub fn signal(&self) -> Option<AbortSignal> {
        match &self.state.lock().phase {
            Phase::Running { signal, .. } | Phase::Maintenance { signal, .. } => {
                Some(signal.clone())
            }
            Phase::Idle { .. } | Phase::Disposed { .. } => None,
        }
    }

    /// Updates the running turn/step at their durable commit points.
    ///
    /// # Errors
    ///
    /// Rejects updates outside a running driver.
    pub fn set_position(&self, turn: u64, step: u64) -> Result<(), AgentControlError> {
        let id = self.agent_id();
        let mut state = self.state.lock();
        let Phase::Running {
            turn: current_turn,
            step: current_step,
            ..
        } = &mut state.phase
        else {
            return Err(AgentControlError::ActiveWork(id));
        };
        *current_turn = turn;
        *current_step = step;
        Ok(())
    }

    /// Starts the next turn inside the same live driver with fresh cancellation.
    ///
    /// # Errors
    ///
    /// Rejects calls outside a running phase.
    pub fn advance_turn(&self) -> Result<(), AgentControlError> {
        let id = self.agent_id();
        let mut state = self.state.lock();
        let Phase::Running {
            signal,
            step,
            wake_requested,
            ..
        } = &mut state.phase
        else {
            return Err(AgentControlError::ActiveWork(id));
        };
        *signal = AbortSignal::default();
        *step = 0;
        *wake_requested = false;
        Ok(())
    }

    /// Permanently rejects new work after cancelling and converging the driver.
    pub async fn dispose(&self) {
        let _ = self.cancel(AgentCancelCause::Disposed, CancelOptions::default());
        let _ = self.when_idle().await;
        let last_turn = match &self.state.lock().phase {
            Phase::Idle { last_turn }
            | Phase::Disposed { last_turn }
            | Phase::Maintenance { last_turn, .. } => *last_turn,
            Phase::Running { turn, .. } => *turn,
        };
        self.state.lock().phase = Phase::Disposed { last_turn };
        self.changed.notify_waiters();
    }

    fn agent_id(&self) -> seekdeep_core::session::SessionId {
        self.agent.upgrade().map_or_else(
            || seekdeep_core::session::SessionId::new("disposed"),
            |agent| agent.id().clone(),
        )
    }

    fn publish_status(&self, status: AgentStatus) {
        if let Some(agent) = self.agent.upgrade() {
            agent.set_status(status);
            AgentEvents::new(agent.context().clone(), agent)
                .emit("agent/status", AgentStatusChanged { status });
        }
    }

    fn wake_driver(&self) -> Result<(), AgentControlError> {
        let (activity, status_changed) = {
            let mut state = self.state.lock();
            match &mut state.phase {
                Phase::Maintenance { wake_requested, .. } => {
                    *wake_requested = true;
                    return Ok(());
                }
                Phase::Running {
                    signal,
                    wake_requested,
                    ..
                } => {
                    let disposed = signal
                        .reason()
                        .as_ref()
                        .and_then(|reason| reason.get("kind"))
                        .and_then(serde_json::Value::as_str)
                        == Some("disposed");
                    if !disposed {
                        *wake_requested = true;
                    }
                    return Ok(());
                }
                Phase::Disposed { .. } => {
                    return Err(AgentControlError::Disposed(self.agent_id()));
                }
                Phase::Idle { last_turn } => {
                    let last_turn = *last_turn;
                    state.next_activity += 1;
                    let activity = state.next_activity;
                    state.phase = Phase::Running {
                        signal: AbortSignal::default(),
                        turn: last_turn,
                        step: 0,
                        wake_requested: false,
                        activity,
                    };
                    (activity, true)
                }
            }
        };
        if status_changed {
            self.publish_status(AgentStatus::Running);
        }
        let Some(agent) = self.agent.upgrade() else {
            self.finish_driver(activity);
            return Ok(());
        };
        let Some(controller) = self.self_weak.upgrade() else {
            return Ok(());
        };
        let driver = self.driver.clone();
        spawn_detached(async move {
            let _ = AssertUnwindSafe(driver(agent, controller.clone()))
                .catch_unwind()
                .await;
            controller.finish_driver(activity);
        });
        Ok(())
    }

    fn finish_driver(&self, activity: u64) {
        let (turn, wake, was_running) = {
            let mut state = self.state.lock();
            let Phase::Running {
                turn,
                wake_requested,
                activity: current,
                ..
            } = &state.phase
            else {
                return;
            };
            if *current != activity {
                return;
            }
            let turn = *turn;
            let wake = *wake_requested;
            let was_running = state.phase.status() == AgentStatus::Running;
            state.settling = true;
            state.phase = Phase::Idle { last_turn: turn };
            (turn, wake, was_running)
        };
        let _ = turn;
        if was_running {
            self.publish_status(AgentStatus::Idle);
        }
        if wake
            && self
                .agent
                .upgrade()
                .is_some_and(|agent| agent.inbox().has_pending())
        {
            let _ = self.wake_driver();
        }
        let mut state = self.state.lock();
        state.completed_activity = state.completed_activity.max(activity);
        state.settling = false;
        drop(state);
        self.changed.notify_waiters();
    }

    fn finish_maintenance(&self, activity: u64) {
        let (last_turn, wake) = {
            let mut state = self.state.lock();
            let Phase::Maintenance {
                last_turn,
                wake_requested,
                activity: current,
                ..
            } = &state.phase
            else {
                return;
            };
            if *current != activity {
                return;
            }
            let values = (*last_turn, *wake_requested);
            state.settling = true;
            state.phase = Phase::Idle {
                last_turn: values.0,
            };
            values
        };
        let _ = last_turn;
        if wake
            && self
                .agent
                .upgrade()
                .is_some_and(|agent| agent.inbox().has_pending())
        {
            let _ = self.wake_driver();
        }
        let mut state = self.state.lock();
        state.completed_activity = state.completed_activity.max(activity);
        state.settling = false;
        drop(state);
        self.changed.notify_waiters();
    }
}

impl AgentController for LoopController {
    fn send(
        &self,
        message: UserMessage,
        target: InboxTarget,
        wakeup: bool,
    ) -> Result<(), AgentControlError> {
        let resolved_target = {
            let state = self.state.lock();
            if matches!(state.phase, Phase::Disposed { .. }) {
                return Err(AgentControlError::Disposed(self.agent_id()));
            }
            let aborted = match &state.phase {
                Phase::Running { signal, .. } | Phase::Maintenance { signal, .. } => {
                    signal.is_aborted()
                }
                Phase::Idle { .. } | Phase::Disposed { .. } => false,
            };
            let wake_after_abort = wakeup && !matches!(state.phase, Phase::Idle { .. }) && aborted;
            if wake_after_abort {
                InboxTarget::NextTurn
            } else {
                target
            }
        };
        let agent = self
            .agent
            .upgrade()
            .ok_or_else(|| AgentControlError::Disposed(self.agent_id()))?;
        agent
            .inbox()
            .splice(resolved_target, f64::INFINITY, 0.0, vec![message])
            .map_err(|error| AgentControlError::Inbox(error.to_string()))?;
        if wakeup {
            self.wake_driver()?;
        }
        Ok(())
    }

    fn cancel(
        &self,
        cause: AgentCancelCause,
        options: CancelOptions,
    ) -> Result<(), AgentControlError> {
        if !options.keep_inbox
            && let Some(agent) = self.agent.upgrade()
        {
            agent
                .inbox()
                .clear()
                .map_err(|error| AgentControlError::Inbox(error.to_string()))?;
        }
        let mut state = self.state.lock();
        match &mut state.phase {
            Phase::Running {
                signal,
                wake_requested,
                ..
            }
            | Phase::Maintenance {
                signal,
                wake_requested,
                ..
            } => {
                if !options.keep_inbox || !signal.is_aborted() {
                    *wake_requested = false;
                }
                signal.abort_with_reason(
                    serde_json::to_value(cause).unwrap_or(serde_json::Value::Null),
                );
            }
            Phase::Idle { .. } => {}
            Phase::Disposed { .. } => {
                return Err(AgentControlError::Disposed(self.agent_id()));
            }
        }
        Ok(())
    }

    fn when_idle(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        let weak = self.self_weak.clone();
        Box::pin(async move {
            loop {
                let Some(controller) = weak.upgrade() else {
                    return Ok(());
                };
                let notified = controller.changed.notified();
                let idle = {
                    let state = controller.state.lock();
                    matches!(state.phase, Phase::Idle { .. } | Phase::Disposed { .. })
                        && !state.settling
                        && state.completed_activity == state.next_activity
                };
                if idle {
                    return Ok(());
                }
                notified.await;
            }
        })
    }

    fn begin_maintenance(&self) -> Result<MaintenanceReservation, AgentControlError> {
        let (signal, activity) = {
            let mut state = self.state.lock();
            let Phase::Idle { last_turn } = state.phase else {
                return Err(match state.phase {
                    Phase::Disposed { .. } => AgentControlError::Disposed(self.agent_id()),
                    Phase::Idle { .. } | Phase::Maintenance { .. } | Phase::Running { .. } => {
                        AgentControlError::ActiveWork(self.agent_id())
                    }
                });
            };
            state.next_activity += 1;
            let activity = state.next_activity;
            let signal = AbortSignal::default();
            state.phase = Phase::Maintenance {
                signal: signal.clone(),
                last_turn,
                wake_requested: false,
                activity,
            };
            (signal, activity)
        };
        let weak = self.self_weak.clone();
        Ok(MaintenanceReservation::new(
            signal,
            Arc::new(move || {
                if let Some(controller) = weak.upgrade() {
                    controller.finish_maintenance(activity);
                }
            }),
        ))
    }
}

struct AgentInboxNotifications {
    events: OnceLock<AgentEvents>,
}

impl AgentInboxNotifications {
    fn new() -> Self {
        Self {
            events: OnceLock::new(),
        }
    }

    fn attach(&self, context: Context, agent: Arc<Agent>) {
        let _ = self.events.set(AgentEvents::new(context, agent));
    }
}

impl InboxNotifications for AgentInboxNotifications {
    fn inserted(&self, message: &UserMessage) {
        if let Some(events) = self.events.get() {
            events.emit(
                "agent/inbox/inserted",
                AgentInboxMessage {
                    message: message.clone(),
                },
            );
        }
    }

    fn discarded(&self, message: &UserMessage) {
        if let Some(events) = self.events.get() {
            events.emit(
                "agent/inbox/discarded",
                AgentInboxMessage {
                    message: message.clone(),
                },
            );
        }
    }

    fn claimed(&self, message: &UserMessage, turn: u64) {
        if let Some(events) = self.events.get() {
            events.emit(
                "agent/inbox/claimed",
                AgentInboxClaimed {
                    message: message.clone(),
                    turn,
                },
            );
        }
    }
}

/// Unpublished concrete agent, its controller, and scoped lifecycle owner.
#[derive(Debug)]
pub struct LoopAgent {
    /// Public agent value registered by agent/session stores.
    pub agent: Arc<Agent>,
    /// Concrete phase controller.
    pub controller: Arc<LoopController>,
    /// Agent-scoped registration boundary.
    pub scope: Scope,
}

impl LoopAgent {
    /// Composes an unpublished loop agent and installs its controller.
    ///
    /// # Errors
    ///
    /// Returns scope, inbox reconstruction, or controller installation errors.
    pub fn new(
        context: &Context,
        session: &Arc<Session>,
        options: AgentOptions,
        parent_scope: Option<ScopeKey>,
        driver: DriverTask,
    ) -> anyhow::Result<Self> {
        let scope_key = ScopeKey::new();
        let mut scope = create_scope(context, scope_key, parent_scope)?;
        scope.context = scope.context.isolate(seekdeep_agent::AGENT);
        let notifications = Arc::new(AgentInboxNotifications::new());
        let inbox = Arc::new(Inbox::new(session.clone(), notifications.clone())?);
        let agent = Arc::new(Agent::new(
            session.id().clone(),
            options,
            session.clone(),
            inbox,
            scope.context.clone(),
            scope_key,
        ));
        scope
            .context
            .provide(seekdeep_agent::AGENT, agent.clone())?;
        notifications.attach(context.clone(), agent.clone());
        let last_turn = session
            .events()
            .into_iter()
            .rev()
            .find(|event| event.event_type == "turn/start")
            .and_then(|event| event.data.get("turn").and_then(serde_json::Value::as_u64))
            .unwrap_or(0);
        let controller = LoopController::new(Arc::downgrade(&agent), last_turn, driver);
        agent.install_controller(controller.clone())?;
        Ok(Self {
            agent,
            controller,
            scope,
        })
    }
}

fn spawn_detached(future: impl std::future::Future<Output = ()> + Send + 'static) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(future);
    } else {
        std::thread::spawn(move || futures::executor::block_on(future));
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use seekdeep_agent::{AgentEvent, InboxTarget};
    use seekdeep_cordis::{EventOptions, EventReply};
    use seekdeep_core::session::SessionId;
    use seekdeep_llm::{ContentBlock, MessageSource};
    use tokio::sync::{Semaphore, mpsc, oneshot};

    use super::*;

    fn message(text: &str) -> UserMessage {
        UserMessage::new(
            vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            MessageSource::user(),
        )
    }

    fn controlled_driver(
        started: mpsc::UnboundedSender<usize>,
        releases: Arc<Semaphore>,
    ) -> DriverTask {
        let count = Arc::new(AtomicUsize::new(0));
        Arc::new(move |agent, _controller| {
            let index = count.fetch_add(1, Ordering::AcqRel) + 1;
            let started = started.clone();
            let releases = releases.clone();
            Box::pin(async move {
                let _ = agent.inbox().claim(InboxTarget::NextTurn, index as u64);
                let _ = started.send(index);
                if let Ok(permit) = releases.acquire().await {
                    permit.forget();
                }
            })
        })
    }

    #[tokio::test]
    async fn latches_wake_after_abort_and_when_idle_follows_replacement() {
        let context = Context::new();
        let session = Session::create(&SessionId::new("wake-latch"), None, None).expect("session");
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let releases = Arc::new(Semaphore::new(0));
        let loop_agent = LoopAgent::new(
            &context,
            &session,
            AgentOptions::default(),
            None,
            controlled_driver(started_tx, releases.clone()),
        )
        .expect("loop agent");
        let statuses = Arc::new(Mutex::new(Vec::new()));
        let observed = statuses.clone();
        context
            .events()
            .on_sync(
                &context,
                "agent/status",
                move |_, args| {
                    let event = args
                        .get::<AgentEvent<AgentStatusChanged>>(0)
                        .expect("status event");
                    observed.lock().push(event.payload.status);
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )
            .expect("listen");

        loop_agent.agent.followup(message("first")).expect("first");
        assert_eq!(started_rx.recv().await, Some(1));
        loop_agent
            .agent
            .cancel(AgentCancelCause::User, CancelOptions { keep_inbox: true })
            .expect("cancel");
        loop_agent
            .agent
            .steer(message("after abort"))
            .expect("steer");
        let idle = loop_agent.agent.when_idle().expect("idle");
        tokio::pin!(idle);
        releases.add_permits(1);
        assert_eq!(started_rx.recv().await, Some(2));
        assert!(idle.as_mut().now_or_never().is_none());
        releases.add_permits(1);
        loop_agent.agent.when_idle().expect("idle 2").await.unwrap();
        assert_eq!(
            *statuses.lock(),
            [
                AgentStatus::Running,
                AgentStatus::Idle,
                AgentStatus::Running,
                AgentStatus::Idle
            ]
        );
        assert!(!loop_agent.agent.inbox().has_pending());
    }

    #[tokio::test]
    async fn latches_live_wake_that_arrives_after_the_driver_claims() {
        let context = Context::new();
        let session =
            Session::create(&SessionId::new("live-wake-latch"), None, None).expect("session");
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let releases = Arc::new(Semaphore::new(0));
        let loop_agent = LoopAgent::new(
            &context,
            &session,
            AgentOptions::default(),
            None,
            controlled_driver(started_tx, releases.clone()),
        )
        .expect("loop agent");

        loop_agent.agent.followup(message("first")).expect("first");
        assert_eq!(started_rx.recv().await, Some(1));
        loop_agent
            .agent
            .steer(message("after final claim"))
            .expect("late steer");
        releases.add_permits(1);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), started_rx.recv())
                .await
                .expect("latched wake must start a replacement driver"),
            Some(2)
        );
        releases.add_permits(1);
        loop_agent.agent.when_idle().expect("idle").await.unwrap();
        assert!(!loop_agent.agent.inbox().has_pending());
    }

    #[tokio::test]
    async fn maintenance_stays_publicly_idle_and_releases_waking_work() {
        let context = Context::new();
        let session = Session::create(&SessionId::new("maintenance"), None, None).expect("session");
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let releases = Arc::new(Semaphore::new(0));
        let loop_agent = LoopAgent::new(
            &context,
            &session,
            AgentOptions::default(),
            None,
            controlled_driver(started_tx, releases.clone()),
        )
        .expect("loop agent");
        let (finish_tx, finish_rx) = oneshot::channel();
        let maintenance = loop_agent
            .agent
            .run_maintenance(move |signal| async move {
                assert!(!signal.is_aborted());
                let _ = finish_rx.await;
                7_u8
            })
            .expect("maintenance");
        assert_eq!(loop_agent.agent.status(), AgentStatus::Idle);
        assert!(matches!(
            loop_agent.agent.run_maintenance(|_| async {}),
            Err(AgentControlError::ActiveWork(_))
        ));
        loop_agent.agent.followup(message("queued")).expect("queue");
        assert!(started_rx.try_recv().is_err());
        finish_tx.send(()).expect("finish maintenance");
        assert_eq!(maintenance.await.unwrap(), 7);
        assert_eq!(started_rx.recv().await, Some(1));
        releases.add_permits(1);
        loop_agent.agent.when_idle().expect("idle").await.unwrap();

        let dropped = loop_agent
            .agent
            .run_maintenance(|_| async { futures::future::pending::<()>().await })
            .expect("second maintenance");
        drop(dropped);
        loop_agent
            .agent
            .when_idle()
            .expect("drop released")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn cancellation_keeps_first_reason_and_idle_cancel_does_not_arm_future_work() {
        let context = Context::new();
        let session = Session::create(&SessionId::new("cancel"), None, None).expect("session");
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let releases = Arc::new(Semaphore::new(0));
        let loop_agent = LoopAgent::new(
            &context,
            &session,
            AgentOptions::default(),
            None,
            controlled_driver(started_tx, releases.clone()),
        )
        .expect("loop agent");
        loop_agent
            .agent
            .cancel(AgentCancelCause::Parent, CancelOptions::default())
            .expect("idle cancel");
        loop_agent.agent.followup(message("run")).expect("run");
        assert_eq!(started_rx.recv().await, Some(1));
        let signal = loop_agent.controller.signal().expect("signal");
        assert!(!signal.is_aborted());
        loop_agent
            .agent
            .cancel(
                AgentCancelCause::Hook {
                    reason: "first".to_owned(),
                },
                CancelOptions { keep_inbox: true },
            )
            .expect("first cancel");
        loop_agent
            .agent
            .cancel(
                AgentCancelCause::Hook {
                    reason: "second".to_owned(),
                },
                CancelOptions { keep_inbox: true },
            )
            .expect("second cancel");
        assert_eq!(signal.reason().expect("reason")["reason"], "first");
        releases.add_permits(1);
        loop_agent.agent.when_idle().expect("idle").await.unwrap();
    }
}
