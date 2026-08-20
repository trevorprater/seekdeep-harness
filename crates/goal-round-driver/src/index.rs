//! Same-session goal-round driver over public agent, session, and goal services.

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_agent::{
    AGENTS, Agent, AgentEvent, AgentLifecycleEvent, AgentStatus, PreStepDecision,
};
use seekdeep_agent_loop::{
    AgentErrorEvent, AgentInboxClaimed, AgentInboxMessage, AgentPreStepEvent, AgentStatusChanged,
    SessionStartEvent,
};
use seekdeep_cordis::{Context, EventOptions, EventReply, FiberState, Plugin};
use seekdeep_core::session::{Session, SessionEvent};
use seekdeep_core::session_store::SESSIONS;
use seekdeep_goal::{
    GOAL, GoalActivation, GoalChangedEvent, GoalId, GoalMessageSource, GoalPhase, GoalRef,
    GoalService, GoalView,
};
use seekdeep_llm::{ContentBlock, MessageId, MessageSource, UserMessage};
use serde_json::Value;

use crate::prompt::render_goal_round_prompt;

/// Cordis plugin name.
pub const NAME: &str = "goal-round-driver";

/// Services required by the driver.
pub const INJECT: &[&str] = &["agents", "goals", "sessions"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttemptPhase {
    Queued,
    Claimed,
    Admitted,
}

#[derive(Clone, Debug)]
struct RoundAttempt {
    goal_id: GoalId,
    revision: u64,
    round: u64,
    message_id: MessageId,
    content: Vec<ContentBlock>,
    phase: AttemptPhase,
    cancelled: bool,
    stale: bool,
}

#[allow(clippy::struct_excessive_bools)]
struct DriverState {
    agent: Arc<Agent>,
    attempt: Option<RoundAttempt>,
    competing_queued: bool,
    needs_checkpoint: bool,
    requested: bool,
    run: Option<tokio::task::JoinHandle<()>>,
    stopping: bool,
}

fn is_goal_round_source(source: &MessageSource) -> bool {
    source.kind == "goal"
        && source
            .fields
            .get("round")
            .and_then(Value::as_u64)
            .is_some_and(|round| round > 0)
}

fn goal_source(source: &MessageSource) -> Option<GoalMessageSource> {
    if source.kind != "goal" {
        return None;
    }
    let goal_id = source.fields.get("goalId").and_then(Value::as_str)?;
    let revision = source.fields.get("revision").and_then(Value::as_u64)?;
    let round = source.fields.get("round").and_then(Value::as_u64)?;
    if goal_id.is_empty() || revision < 1 || round < 1 {
        return None;
    }
    Some(GoalMessageSource {
        kind: seekdeep_goal::GoalSourceKind::Goal,
        goal_id: GoalId::new(goal_id),
        revision,
        round,
    })
}

fn same_round(source: &GoalMessageSource, round: &RoundAttempt) -> bool {
    source.goal_id == round.goal_id
        && source.revision == round.revision
        && source.round == round.round
}

fn same_queued(content: &[ContentBlock], source: &MessageSource, attempt: &RoundAttempt) -> bool {
    goal_source(source).is_some_and(|source| same_round(&source, attempt))
        && content == attempt.content
}

fn goal_ref(goal: &GoalView) -> GoalRef {
    GoalRef {
        id: goal.id.clone(),
        revision: goal.revision,
    }
}

struct Driver {
    context: Context,
    goals: Arc<GoalService>,
    agents: Arc<seekdeep_agent::AgentRegistry>,
    states: Mutex<HashMap<usize, Arc<Mutex<DriverState>>>>,
}

fn agent_key(agent: &Arc<Agent>) -> usize {
    Arc::as_ptr(agent) as usize
}

impl Driver {
    fn state_for(self: &Arc<Self>, agent: &Arc<Agent>) -> Arc<Mutex<DriverState>> {
        let key = agent_key(agent);
        if let Some(state) = self.states.lock().get(&key).cloned() {
            return state;
        }
        let state = Arc::new(Mutex::new(DriverState {
            agent: agent.clone(),
            attempt: None,
            competing_queued: false,
            needs_checkpoint: false,
            requested: false,
            run: None,
            stopping: false,
        }));
        self.states.lock().insert(key, state.clone());
        state
    }

    fn current_goal(&self, state: &DriverState) -> Option<GoalView> {
        if self.agents.get(state.agent.id())?.id() != state.agent.id() {
            return None;
        }
        self.goals.get(&state.agent).ok().flatten()
    }

    fn ready_to_drive(&self, state: &DriverState) -> bool {
        self.context.fiber().state() == FiberState::Active
            && !state.stopping
            && self
                .agents
                .get(state.agent.id())
                .is_some_and(|a| Arc::ptr_eq(&a, &state.agent))
            && state.agent.status() == AgentStatus::Idle
            && !state.competing_queued
    }

    fn ready_after_checkpoint(&self, state: &DriverState) -> bool {
        self.ready_to_drive(state) && !state.needs_checkpoint
    }

    fn disarm(&self, state: &DriverState) {
        if let Err(error) = (|| -> anyhow::Result<()> {
            let goal = self.current_goal(state);
            if goal.is_some_and(|goal| goal.activation == GoalActivation::Armed) {
                self.goals.disarm(&state.agent)?;
            }
            Ok(())
        })() {
            tracing::warn!(
                "goal-round-driver: could not disarm agent {:?}: {error}",
                state.agent.id()
            );
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn drive(self: &Arc<Self>, state: &Arc<Mutex<DriverState>>) -> anyhow::Result<()> {
        let (agent, ready) = {
            let guard = state.lock();
            (guard.agent.clone(), self.ready_to_drive(&guard))
        };
        if !ready {
            return Ok(());
        }
        {
            let needs_checkpoint = {
                let mut guard = state.lock();
                let needs = guard.needs_checkpoint;
                if needs {
                    guard.needs_checkpoint = false;
                }
                needs
            };
            if needs_checkpoint {
                let sessions = self
                    .context
                    .get(SESSIONS)
                    .ok_or_else(|| anyhow::anyhow!("goal-round-driver requires sessions"))?;
                if let Err(error) = sessions.flush(agent.session()).await {
                    tracing::warn!(
                        "goal-round-driver: durability checkpoint failed for agent {:?}: {error}",
                        agent.id()
                    );
                    self.disarm(&state.lock());
                    return Ok(());
                }
                let guard = state.lock();
                if !self.ready_after_checkpoint(&guard) {
                    return Ok(());
                }
            }
        }
        let attempt = state.lock().attempt.clone();
        if attempt.is_some() {
            let mut guard = state.lock();
            guard.attempt = None;
            guard.needs_checkpoint = true;
            guard.requested = true;
            return Ok(());
        }
        let mut guard = state.lock();
        let Some(goal) = self.current_goal(&guard) else {
            return Ok(());
        };
        if goal.phase != GoalPhase::Active || goal.activation != GoalActivation::Armed {
            return Ok(());
        }
        if goal.rounds_started >= goal.max_goal_rounds {
            drop(guard);
            let _ = self.goals.block(
                &agent,
                &goal_ref(&goal),
                &serde_json::json!({
                    "code": "round-limit",
                    "message": format!("Goal reached its configured limit of {} rounds.", goal.max_goal_rounds),
                }),
            );
            return Ok(());
        }
        let round = goal.rounds_started + 1;
        let content = render_goal_round_prompt(&goal, round);
        let message = UserMessage::new(
            content.clone(),
            MessageSource {
                kind: "goal".to_owned(),
                fields: {
                    let mut fields = serde_json::Map::new();
                    fields.insert("goalId".to_owned(), serde_json::json!(goal.id.as_str()));
                    fields.insert("revision".to_owned(), serde_json::json!(goal.revision));
                    fields.insert("round".to_owned(), serde_json::json!(round));
                    fields
                },
            },
        );
        let message_id = message.id().clone();
        let reservation = RoundAttempt {
            goal_id: goal.id.clone(),
            revision: goal.revision,
            round,
            message_id: message_id.clone(),
            content: content.clone(),
            phase: AttemptPhase::Queued,
            cancelled: false,
            stale: false,
        };
        guard.attempt = Some(reservation);
        drop(guard);
        if let Err(error) = agent.followup(message) {
            state.lock().attempt = None;
            tracing::warn!(
                "goal-round-driver: could not queue round {round} for agent {:?}: {error}",
                agent.id()
            );
            let latest = self.current_goal(&state.lock());
            if latest.as_ref().is_some_and(|latest| {
                latest.id == goal.id
                    && latest.revision == goal.revision
                    && latest.phase == GoalPhase::Active
                    && latest.activation == GoalActivation::Armed
            }) {
                let _ = self.goals.block(
                    &agent,
                    &goal_ref(latest.as_ref().expect("checked")),
                    &serde_json::json!({
                        "code": "queue-failed",
                        "message": format!("Could not queue goal round {round}: {error}"),
                    }),
                );
            }
        }
        Ok(())
    }

    fn request_drive(self: &Arc<Self>, state: &Arc<Mutex<DriverState>>) {
        {
            let mut guard = state.lock();
            if guard.stopping {
                return;
            }
            guard.requested = true;
            if guard.run.is_some() {
                return;
            }
        }
        let driver = self.clone();
        let state = state.clone();
        let spawn_state = state.clone();
        let run = match self.agents.without_initiator(|| ()) {
            Ok(()) => tokio::spawn(async move {
                loop {
                    let should_run = {
                        let mut guard = spawn_state.lock();
                        if !guard.requested || guard.stopping {
                            break;
                        }
                        guard.requested = false;
                        true
                    };
                    if !should_run {
                        break;
                    }
                    if let Err(error) = driver.drive(&spawn_state).await {
                        tracing::warn!(
                            "goal-round-driver: driver failed for agent {:?}: {error}",
                            spawn_state.lock().agent.id()
                        );
                        driver.disarm(&spawn_state.lock());
                    }
                }
            }),
            Err(error) => {
                tracing::warn!(
                    "goal-round-driver: could not start driver for agent {:?}: {error}",
                    state.lock().agent.id()
                );
                self.disarm(&state.lock());
                return;
            }
        };
        state.lock().run = Some(run);
    }

    #[allow(clippy::too_many_lines)]
    fn register(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let error_driver = self.clone();
        context.events().on_sync(
            context,
            "agent/error",
            move |_, args| {
                let Some(event) = args.get::<AgentEvent<AgentErrorEvent>>(0) else {
                    return Ok(EventReply::Undefined);
                };
                let state = error_driver.state_for(&event.agent);
                error_driver.disarm(&state.lock());
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;

        let created_driver = self.clone();
        context.events().on_sync(
            context,
            "agent/created",
            move |_, args| {
                let Some(event) = args.get::<AgentLifecycleEvent>(0) else {
                    return Ok(EventReply::Undefined);
                };
                created_driver.state_for(&event.agent);
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;

        let disposed_driver = self.clone();
        context.events().on_sync(
            context,
            "agent/disposed",
            move |_, args| {
                let Some(event) = args.get::<AgentLifecycleEvent>(0) else {
                    return Ok(EventReply::Undefined);
                };
                disposed_driver
                    .states
                    .lock()
                    .remove(&agent_key(&event.agent));
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;

        let session_start_driver = self.clone();
        context.events().on_sync(
            context,
            "agent/session-start",
            move |_, args| {
                let Some(event) = args.get::<AgentEvent<SessionStartEvent>>(0) else {
                    return Ok(EventReply::Undefined);
                };
                let state = session_start_driver.state_for(&event.agent);
                let mut guard = state.lock();
                guard.attempt = None;
                guard.competing_queued = false;
                guard.needs_checkpoint = false;
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;

        let status_driver = self.clone();
        context.events().on_sync(
            context,
            "agent/status",
            move |_, args| {
                let Some(event) = args.get::<AgentEvent<AgentStatusChanged>>(0) else {
                    return Ok(EventReply::Undefined);
                };
                if event.payload.status != AgentStatus::Idle {
                    return Ok(EventReply::Undefined);
                }
                let state = status_driver.state_for(&event.agent);
                {
                    let mut guard = state.lock();
                    guard.competing_queued = false;
                    let attempt = guard.attempt.clone();
                    let goal = status_driver.current_goal(&guard);
                    if let Some(goal) = goal
                        && (attempt.as_ref().is_some_and(|a| {
                            matches!(a.phase, AttemptPhase::Queued | AttemptPhase::Claimed)
                                || a.cancelled
                        }))
                        && goal.phase == GoalPhase::Active
                        && goal.activation == GoalActivation::Armed
                    {
                        guard.attempt = None;
                        drop(guard);
                        if let Err(error) = status_driver.goals.pause(&event.agent, &goal_ref(&goal)) {
                            tracing::warn!("goal-round-driver: could not pause cancelled goal for agent {:?}: {error}", event.agent.id());
                            status_driver.disarm(&state.lock());
                        }
                    }
                }
                status_driver.request_drive(&state);
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;

        let changed_driver = self.clone();
        context.events().on_sync(
            context,
            "goal/changed",
            move |_, args| {
                let Some(event) = args.get::<AgentEvent<GoalChangedEvent>>(0) else {
                    return Ok(EventReply::Undefined);
                };
                let state = changed_driver.state_for(&event.agent);
                state.lock().needs_checkpoint = true;
                changed_driver.request_drive(&state);
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;

        let inserted_driver = self.clone();
        context.events().on_sync(
            context,
            "agent/inbox/inserted",
            move |_, args| {
                let Some(event) = args.get::<AgentEvent<AgentInboxMessage>>(0) else {
                    return Ok(EventReply::Undefined);
                };
                if !event
                    .agent
                    .inbox()
                    .next_turn()
                    .iter()
                    .any(|candidate| candidate.id() == event.payload.message.id())
                {
                    return Ok(EventReply::Undefined);
                }
                let state = inserted_driver.state_for(&event.agent);
                let mut guard = state.lock();
                let attempt = guard.attempt.clone();
                if attempt.as_ref().is_some_and(|attempt| {
                    same_queued(
                        event.payload.message.content(),
                        event.payload.message.source(),
                        attempt,
                    )
                }) {
                    return Ok(EventReply::Undefined);
                }
                guard.competing_queued = true;
                if let Some(attempt) = &mut guard.attempt
                    && attempt.phase == AttemptPhase::Queued
                {
                    attempt.stale = true;
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;

        let claimed_driver = self.clone();
        context.events().on_sync(
            context,
            "agent/inbox/claimed",
            move |_, args| {
                let Some(event) = args.get::<AgentEvent<AgentInboxClaimed>>(0) else {
                    return Ok(EventReply::Undefined);
                };
                let state = claimed_driver.state_for(&event.agent);
                let mut guard = state.lock();
                if let Some(attempt) = &mut guard.attempt
                    && same_queued(
                        event.payload.message.content(),
                        event.payload.message.source(),
                        attempt,
                    )
                {
                    attempt.phase = AttemptPhase::Claimed;
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;

        let discarded_driver = self.clone();
        context.events().on_sync(
            context,
            "agent/inbox/discarded",
            move |_, args| {
                let Some(event) = args.get::<AgentEvent<AgentInboxMessage>>(0) else {
                    return Ok(EventReply::Undefined);
                };
                let state = discarded_driver.state_for(&event.agent);
                let mut guard = state.lock();
                if let Some(attempt) = &mut guard.attempt
                    && same_queued(
                        event.payload.message.content(),
                        event.payload.message.source(),
                        attempt,
                    )
                {
                    attempt.cancelled = true;
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;

        let session_driver = self.clone();
        context.events().on_sync(
            context,
            "session/event",
            move |_, args| {
                let Some(session) = args.get::<Session>(0) else {
                    return Ok(EventReply::Undefined);
                };
                let Some(event) = args.get::<SessionEvent>(1) else {
                    return Ok(EventReply::Undefined);
                };
                let Some(agent) = session_driver.agents.get(session.id()) else {
                    return Ok(EventReply::Undefined);
                };
                if !Arc::ptr_eq(agent.session(), &session) {
                    return Ok(EventReply::Undefined);
                }
                let state = session_driver.state_for(&agent);
                match event.event_type.as_str() {
                    "user/message" => {
                        let mut guard = state.lock();
                        if let Some(attempt) = &mut guard.attempt
                            && event.data.get("id").and_then(Value::as_str)
                                == Some(attempt.message_id.as_str())
                        {
                            attempt.phase = AttemptPhase::Admitted;
                        }
                    }
                    "turn/end" => {
                        let reason_kind = event
                            .data
                            .get("reason")
                            .and_then(|reason| reason.get("kind"))
                            .and_then(Value::as_str);
                        if reason_kind == Some("max-tokens") {
                            session_driver.disarm(&state.lock());
                            return Ok(EventReply::Undefined);
                        }
                        if reason_kind != Some("aborted") {
                            return Ok(EventReply::Undefined);
                        }
                        let mut guard = state.lock();
                        if matches!(
                            guard.attempt.as_ref().map(|a| a.phase),
                            Some(AttemptPhase::Claimed | AttemptPhase::Admitted)
                        ) {
                            if let Some(attempt) = &mut guard.attempt {
                                attempt.cancelled = true;
                            }
                        } else {
                            drop(guard);
                            session_driver.disarm(&state.lock());
                        }
                    }
                    _ => {}
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;

        let pre_step_driver = self.clone();
        context.events().on_waterfall(
            context,
            "agent/pre-step",
            move |_, args, next| {
                let Some(event) = args.get::<AgentEvent<AgentPreStepEvent>>(0) else {
                    return Box::pin(async move {
                        Err(anyhow::anyhow!("agent/pre-step lacks its payload"))
                    });
                };
                let agent = event.agent.clone();
                let messages = event.payload.messages.clone();
                let signal = event.payload.signal.clone();
                let driver = pre_step_driver.clone();
                Box::pin(async move {
                    let submitted = messages
                        .iter()
                        .find(|message| is_goal_round_source(message.source()))
                        .cloned();
                    let Some(submitted) = submitted else {
                        return next.run().await;
                    };
                    let Some(source) = goal_source(submitted.source()) else {
                        return next.run().await;
                    };
                    let state = driver.state_for(&agent);
                    let mut valid =
                        driver.valid_reservation(&state.lock(), submitted.content(), &source);
                    if let Err(error) = valid {
                        tracing::warn!(
                            "goal-round-driver: pre-step check failed for agent {:?}: {error}",
                            agent.id()
                        );
                        driver.disarm(&state.lock());
                        valid = Ok(false);
                    }
                    let valid = valid.unwrap_or(false);
                    if !valid {
                        let mut guard = state.lock();
                        if let Some(attempt) = &guard.attempt
                            && same_round(&source, attempt)
                        {
                            let mut attempt = guard.attempt.take().expect("checked");
                            attempt.stale = true;
                            drop(guard);
                            driver.request_drive(&state);
                        }
                        Driver::restore_other_claimed(&agent, &messages, submitted.id());
                        return Ok(EventReply::Value(Arc::new(PreStepDecision::Reject)));
                    }
                    let decision = match next.run().await {
                        Ok(reply) => reply
                            .downcast::<PreStepDecision>()
                            .map(|decision| (*decision).clone())
                            .ok_or_else(|| {
                                anyhow::anyhow!("agent/pre-step returned an invalid decision")
                            })?,
                        Err(error) => {
                            if signal.is_aborted() {
                                return Err(error);
                            }
                            state.lock().attempt = None;
                            driver.request_drive(&state);
                            return Err(error);
                        }
                    };
                    if signal.is_aborted() {
                        if let PreStepDecision::Enter { messages: entered } = &decision {
                            Driver::restore_other_claimed(&agent, entered, submitted.id());
                        }
                        return Ok(EventReply::Value(Arc::new(decision)));
                    }
                    if matches!(decision, PreStepDecision::Reject) {
                        state.lock().attempt = None;
                        let goal = driver.current_goal(&state.lock());
                        if goal.as_ref().is_some_and(|goal| {
                            goal.id == source.goal_id
                                && goal.revision == source.revision
                                && goal.phase == GoalPhase::Active
                                && goal.activation == GoalActivation::Armed
                        }) {
                            let _ = driver.goals.block(
                                &agent,
                                &goal_ref(goal.as_ref().expect("checked")),
                                &serde_json::json!({
                                    "code": "prompt-rejected",
                                    "message": "Goal round was rejected before entering its step.",
                                }),
                            );
                        }
                        return Ok(EventReply::Value(Arc::new(decision)));
                    }
                    let valid =
                        driver.valid_reservation(&state.lock(), submitted.content(), &source);
                    if !valid.unwrap_or(false) {
                        state.lock().attempt = None;
                        let entered = match &decision {
                            PreStepDecision::Enter { messages } => messages.clone(),
                            PreStepDecision::Reject => Vec::new(),
                        };
                        Driver::restore_other_claimed(&agent, &entered, submitted.id());
                        driver.request_drive(&state);
                        return Ok(EventReply::Value(Arc::new(PreStepDecision::Reject)));
                    }
                    Ok(EventReply::Value(Arc::new(decision)))
                })
            },
            EventOptions::default(),
        )?;
        Ok(())
    }

    #[allow(clippy::unnecessary_wraps)]
    fn valid_reservation(
        &self,
        state: &DriverState,
        content: &[ContentBlock],
        source: &GoalMessageSource,
    ) -> anyhow::Result<bool> {
        let attempt = state.attempt.as_ref();
        let goal = self.current_goal(state);
        Ok(self.context.fiber().state() == FiberState::Active
            && !state.stopping
            && attempt
                .is_some_and(|attempt| attempt.phase == AttemptPhase::Claimed && !attempt.stale)
            && attempt.is_some_and(|attempt| same_round(source, attempt))
            && attempt.is_some_and(|attempt| content == attempt.content)
            && goal.as_ref().is_some_and(|goal| {
                goal.id == source.goal_id
                    && goal.revision == source.revision
                    && goal.phase == GoalPhase::Active
                    && goal.activation == GoalActivation::Armed
                    && source.round == goal.rounds_started + 1
            }))
    }

    fn restore_other_claimed(agent: &Arc<Agent>, messages: &[UserMessage], message_id: &MessageId) {
        let retained = messages
            .iter()
            .filter(|message| {
                message.id() != message_id
                    && !(message.source().kind == "goal"
                        && message.source().fields.get("round").and_then(Value::as_u64) == Some(0))
            })
            .cloned()
            .collect::<Vec<_>>();
        for message in retained.iter().rev() {
            let inbox = agent.inbox();
            if inbox
                .next_step()
                .iter()
                .any(|candidate| candidate.id() == message.id())
                || inbox
                    .next_turn()
                    .iter()
                    .any(|candidate| candidate.id() == message.id())
            {
                continue;
            }
            let _ = inbox.prepend(seekdeep_agent::InboxTarget::NextStep, message.clone());
        }
    }
}

/// Installs automatic same-session continuation and its race fences.
///
/// # Errors
///
/// Returns missing-service or listener registration failures.
pub fn apply(context: &Context) -> anyhow::Result<()> {
    let goals = context
        .get(GOAL)
        .ok_or_else(|| anyhow::anyhow!("goal-round-driver requires goals"))?;
    let agents = context
        .get(AGENTS)
        .ok_or_else(|| anyhow::anyhow!("goal-round-driver requires agents"))?;
    let driver = Arc::new(Driver {
        context: context.clone(),
        goals,
        agents,
        states: Mutex::new(HashMap::new()),
    });
    driver.register(context)?;

    for agent in driver.agents.list() {
        let state = driver.state_for(&agent);
        driver.disarm(&state.lock());
    }

    let cleanup_driver = driver.clone();
    context.own(seekdeep_cordis::fiber::EffectHandle::new(
        "goal-round-driver lifecycle",
        move || {
            Box::pin(async move {
                let mut waits = Vec::new();
                {
                    let states: Vec<_> = cleanup_driver.states.lock().values().cloned().collect();
                    for state in states {
                        let mut guard = state.lock();
                        guard.stopping = true;
                        cleanup_driver.disarm(&guard);
                        if let Some(run) = guard.run.take() {
                            waits.push(run);
                        }
                    }
                    cleanup_driver.states.lock().clear();
                }
                for wait in waits {
                    let _ = wait.await;
                }
                Ok(())
            })
        },
    ))?;
    Ok(())
}

/// Builds the loader-compatible goal-round-driver plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, _config| {
        Box::pin(async move {
            apply(&context)?;
            Ok(())
        })
    })
}
