//! Same-session goal domain: event-sourced state, projection fold, and service.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;
use seekdeep_agent::{AGENTS, Agent, AgentEvent, AgentEvents};
use seekdeep_agent_loop::SessionStartEvent;
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_core::session::{AppendOptions, Session, SessionEvent};
use seekdeep_session_projection::{
    ProjectionDefinition, ProjectionTransition, SESSION_PROJECTIONS,
};
use seekdeep_typert_protocol::{
    RemoteMethodMarker, TypertBoundaryValue, TypertHostArgument, TypertInvocableService,
    TypertInvocationFuture, TypertRemoteService, typert_remote_method,
};
use serde_json::Value;
use uuid::Uuid;

use crate::domain::{
    GoalChangeKind, GoalChangeMeta, GoalChanged, GoalChangedEvent, GoalClearChangeMeta,
    GoalClearOperation, GoalErrorCode, GoalOperation, GoalSnapshotChangeMeta,
};
use crate::fold::{
    GoalFoldState, apply_goal_event, decode_goal_change, empty_goal_fold_state, goal_change_ref,
};
use crate::runtime::{GOAL_CHANGE_VERSION, GoalError};
use crate::types::{
    CreateGoalRequest, CreateGoalResult, EditGoalRequest, GoalActivation, GoalBlockReason, GoalId,
    GoalPhase, GoalProjection, GoalRef, GoalSnapshot, GoalView,
};

/// Typed Cordis slot for the goal service.
pub const GOAL: ServiceKey<GoalService> = ServiceKey::new("goals");

/// Deployment default for goal creation.
pub const DEFAULT_MAX_GOAL_ROUNDS: u64 = 256;

/// Injectable decision-time and goal-identity environment.
pub trait GoalEnvironment: std::fmt::Debug + Send + Sync {
    /// Current decision time in Unix milliseconds.
    fn now_millis(&self) -> u64;
    /// Opaque goal identity for a create event at the supplied session position.
    fn goal_id(&self, session: &Session, now: u64) -> GoalId;
}

#[derive(Debug)]
struct SystemGoalEnvironment;

impl GoalEnvironment for SystemGoalEnvironment {
    fn now_millis(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| {
                u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
            })
    }

    fn goal_id(&self, session: &Session, now: u64) -> GoalId {
        let seed = format!("{}:{}:{now}", session.id(), session.seq());
        GoalId::new(format!(
            "goal-{}",
            Uuid::new_v5(&Uuid::NAMESPACE_OID, seed.as_bytes())
        ))
    }
}

/// Deployment defaults for goal creation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// Total rounds used when a create request omits its own cap.
    pub default_max_goal_rounds: Option<u64>,
}

/// Resolved defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResolvedConfig {
    /// Validated positive default round cap.
    pub default_max_goal_rounds: u64,
}

/// Validated create input with every deployment default materialized.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedCreateGoal {
    /// Normalized objective.
    pub objective: String,
    /// Materialized round cap.
    pub max_goal_rounds: u64,
}

fn is_lower_kebab(value: &str) -> bool {
    !value.is_empty()
        && value.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && value.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// Validates a caller-visible positive round cap.
///
/// # Errors
///
/// Returns an invalid-round-cap failure.
pub fn resolve_max_goal_rounds(value: u64) -> Result<u64, GoalError> {
    if value < 1 {
        return Err(GoalError::new(
            "maxGoalRounds must be a positive safe integer",
            GoalErrorCode::GoalInvalidMaxRounds,
        ));
    }
    Ok(value)
}

/// Validates and normalizes an objective at the domain boundary.
///
/// # Errors
///
/// Returns an invalid-objective failure.
pub fn resolve_objective(value: &str) -> Result<String, GoalError> {
    if value.trim().is_empty() {
        return Err(GoalError::new(
            "goal objective must be a non-empty string",
            GoalErrorCode::GoalInvalidObjective,
        ));
    }
    Ok(value.trim().to_owned())
}

/// Materializes deployment defaults and validates one create request.
///
/// # Errors
///
/// Returns an invalid-objective or invalid-round-cap failure.
pub fn resolve_create_goal(
    request: &CreateGoalRequest,
    default_max_goal_rounds: u64,
) -> Result<ResolvedCreateGoal, GoalError> {
    Ok(ResolvedCreateGoal {
        objective: resolve_objective(&request.objective)?,
        max_goal_rounds: resolve_max_goal_rounds(
            request.max_goal_rounds.unwrap_or(default_max_goal_rounds),
        )?,
    })
}

/// Validates and detaches one policy-owned blocker explanation.
///
/// # Errors
///
/// Returns an invalid-block-reason failure.
pub fn resolve_block_reason(reason: &Value) -> Result<GoalBlockReason, GoalError> {
    let object = reason.as_object();
    let code = object
        .and_then(|object| object.get("code"))
        .and_then(Value::as_str);
    let message = object
        .and_then(|object| object.get("message"))
        .and_then(Value::as_str);
    let Some(code) = code.filter(|code| is_lower_kebab(code)) else {
        return Err(GoalError::new(
            "goal block reason requires a lower-kebab-case code and a non-empty message",
            GoalErrorCode::GoalInvalidBlockReason,
        ));
    };
    let Some(message) = message.filter(|message| !message.trim().is_empty()) else {
        return Err(GoalError::new(
            "goal block reason requires a lower-kebab-case code and a non-empty message",
            GoalErrorCode::GoalInvalidBlockReason,
        ));
    };
    Ok(GoalBlockReason {
        code: code.to_owned(),
        message: message.trim().to_owned(),
    })
}

/// Light last-wins fold of the goal projection unit.
#[must_use]
pub fn apply_goal_projection(
    state: Option<GoalProjection>,
    event: &SessionEvent,
) -> Option<GoalProjection> {
    if event.event_type != "goal/change" {
        return state;
    }
    let Some(change) = decode_goal_change(&event.data).ok().flatten() else {
        return state;
    };
    match change {
        GoalChangeMeta::Clear(_) => None,
        GoalChangeMeta::Snapshot(snapshot) => Some(GoalProjection {
            goal: snapshot.goal,
            rounds_started: snapshot.rounds_started,
            created_at: snapshot.created_at,
            updated_at: snapshot.updated_at,
        }),
    }
}

/// Activation intent carried across the synchronous append boundary.
#[derive(Clone, Copy, Debug)]
struct PendingActivation {
    /// Session sequence the pending activation applies to.
    seq: u64,
    /// Intended post-commit activation.
    activation: GoalActivation,
}

/// Process-local cache plus activation intent crossing the append boundary.
#[derive(Debug)]
struct GoalCache {
    /// Strict replay accumulator.
    state: GoalFoldState,
    /// Current process-local continuation eligibility.
    activation: GoalActivation,
    /// Next unobserved session event sequence.
    observed_seq: u64,
    /// Activation staged for the event currently being appended.
    pending_activation: Option<PendingActivation>,
}

/// Goal service backed exclusively by the owning session log.
pub struct GoalService {
    context: Context,
    resolved: ResolvedConfig,
    caches: Mutex<HashMap<usize, Arc<Mutex<GoalCache>>>>,
    environment: Arc<dyn GoalEnvironment>,
}

impl GoalService {
    /// Constructs an unprovided service and registers its lifecycle listeners.
    ///
    /// # Errors
    ///
    /// Returns an invalid default round cap, a listener registration failure,
    /// or a projection registration failure.
    pub fn new(context: &Context, config: Config) -> anyhow::Result<Arc<Self>> {
        Self::new_with_environment(context, config, Arc::new(SystemGoalEnvironment))
    }

    /// Constructs the service with an injected deterministic environment.
    ///
    /// # Errors
    ///
    /// Returns the same configuration, listener, and projection failures as [`Self::new`].
    pub fn new_with_environment(
        context: &Context,
        config: Config,
        environment: Arc<dyn GoalEnvironment>,
    ) -> anyhow::Result<Arc<Self>> {
        let service = Arc::new(Self {
            context: context.clone(),
            resolved: ResolvedConfig {
                default_max_goal_rounds: resolve_max_goal_rounds(
                    config
                        .default_max_goal_rounds
                        .unwrap_or(DEFAULT_MAX_GOAL_ROUNDS),
                )?,
            },
            caches: Mutex::new(HashMap::new()),
            environment,
        });

        let weak = Arc::downgrade(&service);
        context.events().on_sync(
            context,
            "agent/session-start",
            move |_, args| {
                let event = args
                    .get::<AgentEvent<SessionStartEvent>>(0)
                    .ok_or_else(|| anyhow::anyhow!("agent/session-start lacks its event"))?;
                if let Some(service) = weak.upgrade() {
                    let cache = service.cache(event.agent.session())?;
                    cache.lock().activation = GoalActivation::Disarmed;
                }
                Ok(EventReply::Undefined)
            },
            global_events(),
        )?;

        if let Some(registry) = context.get(SESSION_PROJECTIONS) {
            registry.register(context, goal_projection_definition())?;
        }
        Ok(service)
    }

    /// Publishes this exact service on the goal service slot.
    ///
    /// # Errors
    ///
    /// Returns ordinary duplicate-service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(GOAL, self.clone())
    }

    /// Installs and publishes the goal service.
    ///
    /// # Errors
    ///
    /// Returns construction, listener, projection, or service failures.
    pub fn install(context: &Context, config: Config) -> anyhow::Result<Arc<Self>> {
        let service = Self::new(context, config)?;
        service.provide(context)?;
        Ok(service)
    }

    /// Reads the current goal for one exact live agent.
    ///
    /// # Errors
    ///
    /// Returns a rejection when the agent is not the registry's live instance
    /// or when the durable stream fails to fold.
    pub fn get(&self, agent: &Arc<Agent>) -> anyhow::Result<Option<GoalView>> {
        self.assert_live(agent)?;
        let cache = self.cache(agent.session())?;
        Self::sync(agent.session(), &cache)?;
        Self::view(&cache)
    }

    /// Removes process-local continuation authority without changing durable
    /// phase or revision.
    ///
    /// # Errors
    ///
    /// Returns a rejection when the agent is not the registry's live instance
    /// or when the durable stream fails to fold.
    pub fn disarm(&self, agent: &Arc<Agent>) -> anyhow::Result<Option<GoalView>> {
        self.assert_live(agent)?;
        let cache = self.cache(agent.session())?;
        Self::sync(agent.session(), &cache)?;
        cache.lock().activation = GoalActivation::Disarmed;
        Self::view(&cache)
    }

    /// Creates and arms a goal. A completed goal may be replaced; every other
    /// current phase must be cleared or resumed instead.
    ///
    /// # Errors
    ///
    /// Returns an existing-goal, live-agent, fold, or commit failure.
    pub fn create(
        &self,
        agent: &Arc<Agent>,
        request: &CreateGoalRequest,
    ) -> anyhow::Result<GoalView> {
        let spec = resolve_create_goal(request, self.resolved.default_max_goal_rounds)?;
        let cache = self.prepare_mutation(agent)?;
        let current = cache.lock().state.goal.clone();
        if let Some(current) = current.filter(|goal| goal.phase != GoalPhase::Complete) {
            return Err(goal_error(
                format!(
                    "goal \"{}\" already exists with phase \"{}\"",
                    current.id,
                    phase_name(current.phase)
                ),
                GoalErrorCode::GoalAlreadyExists,
            ));
        }
        let now = self.environment.now_millis();
        let goal = GoalSnapshot {
            id: self.environment.goal_id(agent.session(), now),
            revision: 1,
            objective: spec.objective,
            phase: GoalPhase::Active,
            blocked_reason: None,
            max_goal_rounds: spec.max_goal_rounds,
        };
        self.commit_snapshot(
            agent,
            &cache,
            GoalOperation::Create,
            goal,
            0,
            now,
            now,
            GoalActivation::Armed,
        )
    }

    /// Edits objective and/or round cap without changing phase.
    ///
    /// # Errors
    ///
    /// Returns a stale-ref, invalid-edit, live-agent, fold, or commit failure.
    pub fn edit(
        &self,
        agent: &Arc<Agent>,
        goal_ref: &GoalRef,
        request: &EditGoalRequest,
    ) -> anyhow::Result<GoalView> {
        let cache = self.prepare_mutation(agent)?;
        let current = Self::expect_current(&cache, goal_ref)?;
        if request.objective.is_none() && request.max_goal_rounds.is_none() {
            return Err(goal_error(
                "goal edit requires objective and/or maxGoalRounds",
                GoalErrorCode::GoalInvalidEdit,
            ));
        }
        let objective = request
            .objective
            .as_deref()
            .map(resolve_objective)
            .transpose()?
            .unwrap_or_else(|| current.objective.clone());
        let max_goal_rounds = request
            .max_goal_rounds
            .map(resolve_max_goal_rounds)
            .transpose()?;
        let goal = GoalSnapshot {
            id: current.id.clone(),
            revision: current.revision + 1,
            objective,
            phase: current.phase,
            blocked_reason: current.blocked_reason.clone(),
            max_goal_rounds: max_goal_rounds.unwrap_or(current.max_goal_rounds),
        };
        let activation = cache.lock().activation;
        self.commit_current(agent, &cache, GoalOperation::Edit, goal, activation)
    }

    /// Pauses an active goal and disarms automatic continuation.
    ///
    /// # Errors
    ///
    /// Returns a stale-ref, transition, live-agent, fold, or commit failure.
    pub fn pause(&self, agent: &Arc<Agent>, goal_ref: &GoalRef) -> anyhow::Result<GoalView> {
        self.transition(
            agent,
            goal_ref,
            GoalOperation::Pause,
            &[GoalPhase::Active],
            GoalPhase::Paused,
            GoalActivation::Disarmed,
        )
    }

    /// Resumes and arms a stopped goal, or rearms an active goal after a
    /// session-start edge, while its round budget still has capacity.
    ///
    /// # Errors
    ///
    /// Returns a stale-ref, transition, exhaustion, live-agent, fold, or commit
    /// failure.
    pub fn resume(&self, agent: &Arc<Agent>, goal_ref: &GoalRef) -> anyhow::Result<GoalView> {
        const RESUMABLE: [GoalPhase; 3] =
            [GoalPhase::Active, GoalPhase::Paused, GoalPhase::Blocked];
        let cache = self.prepare_mutation(agent)?;
        let current = Self::expect_current(&cache, goal_ref)?;
        if !RESUMABLE.contains(&current.phase) {
            return Err(Self::transition_error(&current, GoalOperation::Resume, &RESUMABLE).into());
        }
        let activation = cache.lock().activation;
        if current.phase == GoalPhase::Active && activation == GoalActivation::Armed {
            return Err(goal_error(
                format!("goal \"{}\" is already active and armed", current.id),
                GoalErrorCode::GoalInvalidTransition,
            ));
        }
        let rounds_started = cache.lock().state.rounds_started;
        if rounds_started >= current.max_goal_rounds {
            return Err(goal_error(
                format!(
                    "goal \"{}\" exhausted {} goal rounds; increase maxGoalRounds before resuming",
                    current.id, current.max_goal_rounds
                ),
                GoalErrorCode::GoalInvalidTransition,
            ));
        }
        self.commit_current(
            agent,
            &cache,
            GoalOperation::Resume,
            Self::with_phase(&current, GoalPhase::Active),
            GoalActivation::Armed,
        )
    }

    /// Marks a current non-complete goal complete and disarms it.
    ///
    /// # Errors
    ///
    /// Returns a stale-ref, transition, live-agent, fold, or commit failure.
    pub fn complete(&self, agent: &Arc<Agent>, goal_ref: &GoalRef) -> anyhow::Result<GoalView> {
        self.transition(
            agent,
            goal_ref,
            GoalOperation::Complete,
            &[GoalPhase::Active, GoalPhase::Paused, GoalPhase::Blocked],
            GoalPhase::Complete,
            GoalActivation::Disarmed,
        )
    }

    /// Marks an active goal blocked and disarms it.
    ///
    /// # Errors
    ///
    /// Returns a stale-ref, transition, block-reason, live-agent, fold, or
    /// commit failure.
    pub fn block(
        &self,
        agent: &Arc<Agent>,
        goal_ref: &GoalRef,
        reason: &Value,
    ) -> anyhow::Result<GoalView> {
        let cache = self.prepare_mutation(agent)?;
        let current = Self::expect_current(&cache, goal_ref)?;
        if current.phase != GoalPhase::Active {
            return Err(Self::transition_error(
                &current,
                GoalOperation::Block,
                &[GoalPhase::Active],
            )
            .into());
        }
        let mut goal = Self::with_phase(&current, GoalPhase::Blocked);
        goal.blocked_reason = Some(resolve_block_reason(reason)?);
        self.commit_current(
            agent,
            &cache,
            GoalOperation::Block,
            goal,
            GoalActivation::Disarmed,
        )
    }

    /// Clears the current goal while retaining a durable tombstone and history.
    ///
    /// # Errors
    ///
    /// Returns a stale-ref, live-agent, fold, or commit failure.
    pub fn clear(&self, agent: &Arc<Agent>, goal_ref: &GoalRef) -> anyhow::Result<GoalRef> {
        let cache = self.prepare_mutation(agent)?;
        let current = Self::expect_current(&cache, goal_ref)?;
        let tombstone = GoalRef {
            id: current.id.clone(),
            revision: current.revision + 1,
        };
        let cleared_at = self.next_mutation_time(&cache)?;
        let change = GoalChangeMeta::Clear(GoalClearChangeMeta {
            kind: GoalChangeKind::GoalChange,
            version: GOAL_CHANGE_VERSION,
            operation: GoalClearOperation::Clear,
            cleared: tombstone.clone(),
            cleared_at,
        });
        self.commit(agent, &cache, &change, GoalActivation::Disarmed)?;
        Ok(tombstone)
    }

    /// Creates one goal through the remote boundary, returning its identity.
    ///
    /// # Errors
    ///
    /// Returns the same failures as create.
    pub fn create_remote(
        &self,
        agent: &Arc<Agent>,
        request: &CreateGoalRequest,
    ) -> anyhow::Result<CreateGoalResult> {
        let view = self.create(agent, request)?;
        Ok(CreateGoalResult {
            goal_ref: GoalRef {
                id: view.id,
                revision: view.revision,
            },
        })
    }

    /// Resolves and validates the cache used by a mutation.
    fn prepare_mutation(&self, agent: &Arc<Agent>) -> anyhow::Result<Arc<Mutex<GoalCache>>> {
        self.assert_live(agent)?;
        let cache = self.cache(agent.session())?;
        Self::sync(agent.session(), &cache)?;
        Ok(cache)
    }

    /// Rejects stale or missing current-state refs.
    fn expect_current(
        cache: &Arc<Mutex<GoalCache>>,
        goal_ref: &GoalRef,
    ) -> anyhow::Result<GoalSnapshot> {
        let current = cache.lock().state.goal.clone();
        let Some(current) = current else {
            return Err(goal_error("no current goal", GoalErrorCode::GoalNotFound));
        };
        if goal_ref.id != current.id || goal_ref.revision != current.revision {
            return Err(goal_error(
                format!(
                    "stale goal ref \"{}\" revision {}; current is \"{}\" revision {}",
                    goal_ref.id, goal_ref.revision, current.id, current.revision
                ),
                GoalErrorCode::GoalStaleRevision,
            ));
        }
        Ok(current)
    }

    /// Enforces exact live-agent identity rather than trusting a matching id.
    fn assert_live(&self, agent: &Arc<Agent>) -> anyhow::Result<()> {
        let live = self
            .context
            .get(AGENTS)
            .and_then(|registry| registry.get(agent.id()))
            .is_some_and(|registered| Arc::ptr_eq(&registered, agent));
        if !live {
            return Err(goal_error(
                format!("agent \"{}\" is not live in this registry", agent.id()),
                GoalErrorCode::GoalAgentNotLive,
            ));
        }
        Ok(())
    }

    /// Returns the per-session cache, folding a seed once with activation disarmed.
    fn cache(&self, session: &Arc<Session>) -> anyhow::Result<Arc<Mutex<GoalCache>>> {
        let key = session_key(session);
        {
            let caches = self.caches.lock();
            if let Some(cache) = caches.get(&key) {
                return Ok(cache.clone());
            }
        }
        let mut state = empty_goal_fold_state();
        let events = session.events();
        for event in &events {
            apply_goal_event(&mut state, event)?;
        }
        let cache = Arc::new(Mutex::new(GoalCache {
            state,
            activation: GoalActivation::Disarmed,
            observed_seq: session.seq(),
            pending_activation: None,
        }));
        self.caches.lock().insert(key, cache.clone());
        Ok(cache)
    }

    /// Incrementally observes durable events and reconciles activation intent.
    fn sync(session: &Arc<Session>, cache: &Arc<Mutex<GoalCache>>) -> anyhow::Result<()> {
        let events = session.events();
        let mut guard = cache.lock();
        let start = usize::try_from(guard.observed_seq).unwrap_or(usize::MAX);
        for event in events.iter().skip(start) {
            apply_goal_event(&mut guard.state, event)?;
            if event.event_type == "goal/change" {
                guard.activation = guard
                    .pending_activation
                    .filter(|pending| pending.seq == event.seq)
                    .map_or(GoalActivation::Disarmed, |pending| pending.activation);
            }
            guard.observed_seq += 1;
        }
        Ok(())
    }

    /// Builds a new revision with one replacement phase.
    fn with_phase(current: &GoalSnapshot, phase: GoalPhase) -> GoalSnapshot {
        GoalSnapshot {
            id: current.id.clone(),
            revision: current.revision + 1,
            objective: current.objective.clone(),
            phase,
            blocked_reason: None,
            max_goal_rounds: current.max_goal_rounds,
        }
    }

    /// Shared validated phase transition.
    fn transition(
        &self,
        agent: &Arc<Agent>,
        goal_ref: &GoalRef,
        operation: GoalOperation,
        allowed: &[GoalPhase],
        phase: GoalPhase,
        activation: GoalActivation,
    ) -> anyhow::Result<GoalView> {
        let cache = self.prepare_mutation(agent)?;
        let current = Self::expect_current(&cache, goal_ref)?;
        if !allowed.contains(&current.phase) {
            return Err(Self::transition_error(&current, operation, allowed).into());
        }
        self.commit_current(
            agent,
            &cache,
            operation,
            Self::with_phase(&current, phase),
            activation,
        )
    }

    /// Renders a stable invalid-transition error.
    fn transition_error(
        current: &GoalSnapshot,
        operation: GoalOperation,
        allowed: &[GoalPhase],
    ) -> GoalError {
        GoalError::new(
            format!(
                "cannot {} goal \"{}\" from phase \"{}\"; expected {}",
                operation_name(operation),
                current.id,
                phase_name(current.phase),
                allowed
                    .iter()
                    .map(|phase| phase_name(*phase))
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
            GoalErrorCode::GoalInvalidTransition,
        )
    }

    /// Commits a mutation that retains the current goal's derived counters/times.
    fn commit_current(
        &self,
        agent: &Arc<Agent>,
        cache: &Arc<Mutex<GoalCache>>,
        operation: GoalOperation,
        goal: GoalSnapshot,
        activation: GoalActivation,
    ) -> anyhow::Result<GoalView> {
        let created_at = cache
            .lock()
            .state
            .created_at
            .ok_or_else(|| anyhow::anyhow!("current goal cache lacks createdAt"))?;
        let rounds_started = cache.lock().state.rounds_started;
        let updated_at = self.next_mutation_time(cache)?;
        self.commit_snapshot(
            agent,
            cache,
            operation,
            goal,
            rounds_started,
            created_at,
            updated_at,
            activation,
        )
    }

    /// Clamps a current goal's next timestamp across backward wall-clock movement.
    fn next_mutation_time(&self, cache: &Arc<Mutex<GoalCache>>) -> anyhow::Result<u64> {
        let updated_at = cache
            .lock()
            .state
            .updated_at
            .ok_or_else(|| anyhow::anyhow!("current goal cache lacks updatedAt"))?;
        Ok(self.environment.now_millis().max(updated_at))
    }

    /// Builds and commits one full-snapshot mutation.
    #[allow(clippy::too_many_arguments)]
    fn commit_snapshot(
        &self,
        agent: &Arc<Agent>,
        cache: &Arc<Mutex<GoalCache>>,
        operation: GoalOperation,
        goal: GoalSnapshot,
        rounds_started: u64,
        created_at: u64,
        updated_at: u64,
        activation: GoalActivation,
    ) -> anyhow::Result<GoalView> {
        let change = GoalChangeMeta::Snapshot(GoalSnapshotChangeMeta {
            kind: GoalChangeKind::GoalChange,
            version: GOAL_CHANGE_VERSION,
            operation,
            goal,
            rounds_started,
            created_at,
            updated_at,
        });
        self.commit(agent, cache, &change, activation)?;
        Self::view(cache)?
            .ok_or_else(|| anyhow::anyhow!("snapshot commit cleared the goal unexpectedly"))
    }

    /// Commits one mutation into the goal log, cache, and live event stream.
    fn commit(
        &self,
        agent: &Arc<Agent>,
        cache: &Arc<Mutex<GoalCache>>,
        change: &GoalChangeMeta,
        activation: GoalActivation,
    ) -> anyhow::Result<()> {
        let reference = goal_change_ref(change);
        cache.lock().pending_activation = Some(PendingActivation {
            seq: agent.session().seq(),
            activation,
        });
        let committed = (|| -> anyhow::Result<()> {
            agent.session().append(
                "goal/change",
                serialize_change(change)?,
                AppendOptions::default(),
            )?;
            Self::sync(agent.session(), cache)?;
            Ok(())
        })();
        cache.lock().pending_activation = None;
        committed?;
        let notification = GoalChanged {
            operation: change_operation(change),
            goal_ref: reference,
            goal: Self::view(cache)?,
        };
        AgentEvents::new(self.context.clone(), agent.clone()).emit(
            "goal/changed",
            GoalChangedEvent {
                change: notification,
            },
        );
        Ok(())
    }

    /// Builds a detached current view.
    fn view(cache: &Arc<Mutex<GoalCache>>) -> anyhow::Result<Option<GoalView>> {
        let guard = cache.lock();
        let Some(goal) = &guard.state.goal else {
            return Ok(None);
        };
        let created_at = guard
            .state
            .created_at
            .ok_or_else(|| anyhow::anyhow!("goal \"{}\" cache lacks timestamps", goal.id))?;
        let updated_at = guard
            .state
            .updated_at
            .ok_or_else(|| anyhow::anyhow!("goal \"{}\" cache lacks timestamps", goal.id))?;
        Ok(Some(GoalView {
            id: goal.id.clone(),
            revision: goal.revision,
            objective: goal.objective.clone(),
            phase: goal.phase,
            blocked_reason: goal.blocked_reason.clone(),
            max_goal_rounds: goal.max_goal_rounds,
            rounds_started: guard.state.rounds_started,
            created_at,
            updated_at,
            activation: guard.activation,
        }))
    }
}

impl TypertRemoteService for GoalService {
    fn typert_service_key(&self) -> &'static str {
        "goals"
    }

    fn remote_methods(&self) -> Vec<RemoteMethodMarker> {
        vec![
            typert_remote_method!(GoalService, create_remote => "create"),
            typert_remote_method!(GoalService, edit),
            typert_remote_method!(GoalService, pause),
            typert_remote_method!(GoalService, resume),
            typert_remote_method!(GoalService, complete),
            typert_remote_method!(GoalService, clear),
        ]
    }
}

impl TypertInvocableService for GoalService {
    fn service_key(&self) -> &'static str {
        "goals"
    }

    fn namespace(&self) -> &'static str {
        "goals"
    }

    fn remote_methods(&self) -> Vec<RemoteMethodMarker> {
        <Self as TypertRemoteService>::remote_methods(self)
    }

    fn parameter_names(&self, implementation: &str) -> Option<Vec<String>> {
        match implementation {
            "create_remote" => Some(vec!["agent".to_owned(), "request".to_owned()]),
            "edit" => Some(vec![
                "agent".to_owned(),
                "ref".to_owned(),
                "request".to_owned(),
            ]),
            "pause" | "resume" | "complete" | "clear" => {
                Some(vec!["agent".to_owned(), "ref".to_owned()])
            }
            _ => None,
        }
    }

    fn has_method(&self, implementation: &str) -> bool {
        matches!(
            implementation,
            "create_remote" | "edit" | "pause" | "resume" | "complete" | "clear"
        )
    }

    fn invoke(
        self: Arc<Self>,
        implementation: &str,
        arguments: Vec<TypertHostArgument>,
    ) -> TypertInvocationFuture {
        let implementation = implementation.to_owned();
        Box::pin(async move {
            match implementation.as_str() {
                "create_remote" => {
                    anyhow::ensure!(arguments.len() == 2, "goals/create expects two arguments");
                    let agent = agent_argument(&arguments[0])?;
                    let request = json_argument::<CreateGoalRequest>(&arguments[1])?;
                    Ok(TypertBoundaryValue::Json(serde_json::to_value(
                        self.create_remote(&agent, &request)?,
                    )?))
                }
                "edit" => {
                    anyhow::ensure!(arguments.len() == 3, "goals/edit expects three arguments");
                    let agent = agent_argument(&arguments[0])?;
                    let goal_ref = json_argument::<GoalRef>(&arguments[1])?;
                    let request = json_argument::<EditGoalRequest>(&arguments[2])?;
                    Ok(TypertBoundaryValue::Json(serde_json::to_value(
                        self.edit(&agent, &goal_ref, &request)?,
                    )?))
                }
                "pause" | "resume" | "complete" | "clear" => {
                    anyhow::ensure!(
                        arguments.len() == 2,
                        "goals/{implementation} expects two arguments"
                    );
                    let agent = agent_argument(&arguments[0])?;
                    let goal_ref = json_argument::<GoalRef>(&arguments[1])?;
                    let value = match implementation.as_str() {
                        "pause" => serde_json::to_value(self.pause(&agent, &goal_ref)?)?,
                        "resume" => serde_json::to_value(self.resume(&agent, &goal_ref)?)?,
                        "complete" => serde_json::to_value(self.complete(&agent, &goal_ref)?)?,
                        "clear" => serde_json::to_value(self.clear(&agent, &goal_ref)?)?,
                        _ => anyhow::bail!("goals has no callable method {implementation:?}"),
                    };
                    Ok(TypertBoundaryValue::Json(value))
                }
                _ => anyhow::bail!("goals has no callable method {implementation:?}"),
            }
        })
    }
}

fn agent_argument(argument: &TypertHostArgument) -> anyhow::Result<Arc<Agent>> {
    let TypertHostArgument::Lookup(agent) = argument else {
        anyhow::bail!("goals expected an Agent lookup argument");
    };
    agent
        .clone()
        .downcast::<Agent>()
        .map_err(|_| anyhow::anyhow!("goals lookup argument is not an Agent"))
}

fn json_argument<T: serde::de::DeserializeOwned>(
    argument: &TypertHostArgument,
) -> anyhow::Result<T> {
    let TypertHostArgument::Boundary(TypertBoundaryValue::Json(value)) = argument else {
        anyhow::bail!("goals expected a JSON boundary argument");
    };
    Ok(serde_json::from_value(value.clone())?)
}

fn serialize_change(change: &GoalChangeMeta) -> anyhow::Result<Value> {
    match change {
        GoalChangeMeta::Snapshot(snapshot) => Ok(serde_json::to_value(snapshot)?),
        GoalChangeMeta::Clear(clear) => Ok(serde_json::to_value(clear)?),
    }
}

fn change_operation(change: &GoalChangeMeta) -> GoalOperation {
    match change {
        GoalChangeMeta::Snapshot(snapshot) => snapshot.operation,
        GoalChangeMeta::Clear(_) => GoalOperation::Clear,
    }
}

fn phase_name(phase: GoalPhase) -> &'static str {
    match phase {
        GoalPhase::Active => "active",
        GoalPhase::Paused => "paused",
        GoalPhase::Blocked => "blocked",
        GoalPhase::Complete => "complete",
    }
}

fn operation_name(operation: GoalOperation) -> &'static str {
    match operation {
        GoalOperation::Create => "create",
        GoalOperation::Edit => "edit",
        GoalOperation::Pause => "pause",
        GoalOperation::Resume => "resume",
        GoalOperation::Complete => "complete",
        GoalOperation::Block => "block",
        GoalOperation::Clear => "clear",
    }
}

fn goal_error(message: impl Into<String>, code: GoalErrorCode) -> anyhow::Error {
    GoalError::new(message, code).into()
}

fn session_key(session: &Arc<Session>) -> usize {
    Arc::as_ptr(session) as usize
}

fn global_events() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}

fn goal_projection_definition() -> ProjectionDefinition {
    ProjectionDefinition::new(
        "goal",
        4,
        || Ok(Value::Null),
        |_state, event| {
            if event.event_type != "goal/change" {
                return Ok(ProjectionTransition::Unchanged);
            }
            let Some(change) = decode_goal_change(&event.data).ok().flatten() else {
                return Ok(ProjectionTransition::Unchanged);
            };
            match change {
                GoalChangeMeta::Clear(_) => Ok(ProjectionTransition::Changed(Value::Null)),
                GoalChangeMeta::Snapshot(snapshot) => {
                    Ok(ProjectionTransition::changed(GoalProjection {
                        goal: snapshot.goal,
                        rounds_started: snapshot.rounds_started,
                        created_at: snapshot.created_at,
                        updated_at: snapshot.updated_at,
                    })?)
                }
            }
        },
        |state| Ok(state.clone()),
    )
}

/// Cordis plugin name.
pub const NAME: &str = "goal";
/// The agent registry owns live-agent identity checks.
pub const INJECT: &[&str] = &["agents"];

/// Parses the optional plugin configuration into deployment defaults.
fn parse_config(config: &Value) -> anyhow::Result<Config> {
    if config.is_null() {
        return Ok(Config::default());
    }
    Ok(serde_json::from_value(config.clone())?)
}

/// Builds the loader-compatible goal plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            GoalService::install(&context, parse_config(&config)?)?;
            Ok(())
        })
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::types::{GoalId, GoalPhase, GoalSnapshot};

    fn snapshot() -> GoalSnapshot {
        GoalSnapshot {
            id: GoalId::new("g1"),
            revision: 1,
            objective: "port it".to_owned(),
            phase: GoalPhase::Active,
            blocked_reason: None,
            max_goal_rounds: 10,
        }
    }

    #[test]
    fn projection_folds_snapshot_and_clear() {
        let create = SessionEvent {
            event_type: "goal/change".to_owned(),
            seq: 0,
            time: 0,
            data: json!({
                "kind": "goal/change", "version": 1, "operation": "create",
                "goal": {"id": "g1", "revision": 1, "objective": "port it", "phase": "active", "maxGoalRounds": 10},
                "roundsStarted": 0, "createdAt": 100, "updatedAt": 100,
            }),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        };
        let projected = apply_goal_projection(None, &create).expect("projected");
        assert_eq!(projected.goal.id, snapshot().id);
        assert_eq!(projected.rounds_started, 0);

        let clear = SessionEvent {
            event_type: "goal/change".to_owned(),
            seq: 1,
            time: 0,
            data: json!({
                "kind": "goal/change", "version": 1, "operation": "clear",
                "cleared": {"id": "g1", "revision": 2}, "clearedAt": 200,
            }),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        };
        assert!(apply_goal_projection(Some(projected), &clear).is_none());
    }

    #[test]
    fn validates_objective_and_round_cap() {
        assert!(resolve_objective("  ").is_err());
        assert!(resolve_max_goal_rounds(0).is_err());
        let resolved = resolve_create_goal(
            &CreateGoalRequest {
                objective: "  port it  ".to_owned(),
                max_goal_rounds: None,
            },
            256,
        )
        .expect("resolve");
        assert_eq!(resolved.objective, "port it");
        assert_eq!(resolved.max_goal_rounds, 256);
    }

    #[test]
    fn validates_block_reason() {
        assert!(resolve_block_reason(&json!({"code": "Bad-Code", "message": "x"})).is_err());
        assert!(resolve_block_reason(&json!({"code": "ok", "message": "  "})).is_err());
        let reason = resolve_block_reason(&json!({"code": "needs-approval", "message": "  hi  "}))
            .expect("valid");
        assert_eq!(reason.code, "needs-approval");
        assert_eq!(reason.message, "hi");
    }
}

#[cfg(test)]
mod service_tests {
    use std::collections::VecDeque;

    use seekdeep_agent::{
        AgentOptions, AgentRegistry, Inbox, InboxNotifications, NoopInboxNotifications,
        SessionStartSource,
    };
    use seekdeep_core::session::SessionId;
    use seekdeep_scope::ScopeKey;
    use serde_json::json;

    use super::*;

    #[derive(Debug)]
    struct TestEnvironment {
        times: Mutex<VecDeque<u64>>,
        id: GoalId,
    }

    impl TestEnvironment {
        fn new(times: impl IntoIterator<Item = u64>, id: &str) -> Arc<Self> {
            Arc::new(Self {
                times: Mutex::new(times.into_iter().collect()),
                id: GoalId::new(id),
            })
        }
    }

    impl GoalEnvironment for TestEnvironment {
        fn now_millis(&self) -> u64 {
            self.times.lock().pop_front().expect("scripted goal time")
        }

        fn goal_id(&self, _session: &Session, _now: u64) -> GoalId {
            self.id.clone()
        }
    }

    fn agent(context: &Context, id: &str) -> Arc<Agent> {
        let id = SessionId::new(id);
        let session = Session::create(&id, None, None).expect("session");
        let notifications: Arc<dyn InboxNotifications> = Arc::new(NoopInboxNotifications);
        let inbox = Arc::new(Inbox::new(session.clone(), notifications).expect("inbox"));
        Arc::new(Agent::new(
            id,
            AgentOptions::default(),
            session,
            inbox,
            context.clone(),
            ScopeKey::new(),
        ))
    }

    fn setup(id: &str) -> (Context, Arc<Agent>, Arc<GoalService>) {
        let context = Context::new();
        let registry = Arc::new(AgentRegistry::new(context.clone()));
        registry.provide(&context).expect("provide agents");
        let subject = agent(&context, id);
        registry
            .register(&context, &subject, None)
            .expect("register agent");
        let service = GoalService::install(&context, Config::default()).expect("install goal");
        (context, subject, service)
    }

    fn setup_with_environment(
        id: &str,
        environment: Arc<dyn GoalEnvironment>,
    ) -> (Context, Arc<Agent>, Arc<GoalService>) {
        let context = Context::new();
        let registry = Arc::new(AgentRegistry::new(context.clone()));
        registry.provide(&context).expect("provide agents");
        let subject = agent(&context, id);
        registry
            .register(&context, &subject, None)
            .expect("register agent");
        let service = GoalService::new_with_environment(&context, Config::default(), environment)
            .expect("goal service");
        service.provide(&context).expect("provide goal");
        (context, subject, service)
    }

    #[test]
    fn exposes_expected_remote_markers() {
        let (context, _subject, service) = setup("markers");
        assert!(context.get(GOAL).is_some());
        let methods = TypertRemoteService::remote_methods(service.as_ref());
        let names = methods
            .iter()
            .map(|marker| (marker.method.as_str(), marker.export_name.as_deref()))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                ("create_remote", Some("create")),
                ("edit", None),
                ("pause", None),
                ("resume", None),
                ("complete", None),
                ("clear", None),
            ]
        );
    }

    #[test]
    fn service_lifecycle_create_edit_pause_resume_complete_clear() {
        let (_context, subject, service) = setup("lifecycle");

        let created = service
            .create(
                &subject,
                &CreateGoalRequest {
                    objective: "port it".to_owned(),
                    max_goal_rounds: Some(4),
                },
            )
            .expect("create");
        assert_eq!(created.revision, 1);
        assert_eq!(created.phase, GoalPhase::Active);
        assert_eq!(created.activation, GoalActivation::Armed);
        assert_eq!(created.max_goal_rounds, 4);

        let view = service.get(&subject).expect("get").expect("present");
        assert_eq!(view.id, created.id);
        assert_eq!(view.phase, GoalPhase::Active);
        assert_eq!(view.activation, GoalActivation::Armed);

        let edited = service
            .edit(
                &subject,
                &GoalRef {
                    id: view.id.clone(),
                    revision: view.revision,
                },
                &EditGoalRequest {
                    objective: Some("port it fully".to_owned()),
                    max_goal_rounds: None,
                },
            )
            .expect("edit");
        assert_eq!(edited.revision, 2);
        assert_eq!(edited.objective, "port it fully");
        assert_eq!(edited.activation, GoalActivation::Armed);

        let paused = service
            .pause(
                &subject,
                &GoalRef {
                    id: edited.id.clone(),
                    revision: edited.revision,
                },
            )
            .expect("pause");
        assert_eq!(paused.revision, 3);
        assert_eq!(paused.phase, GoalPhase::Paused);
        assert_eq!(paused.activation, GoalActivation::Disarmed);

        let resumed = service
            .resume(
                &subject,
                &GoalRef {
                    id: paused.id.clone(),
                    revision: paused.revision,
                },
            )
            .expect("resume");
        assert_eq!(resumed.revision, 4);
        assert_eq!(resumed.phase, GoalPhase::Active);
        assert_eq!(resumed.activation, GoalActivation::Armed);

        let completed = service
            .complete(
                &subject,
                &GoalRef {
                    id: resumed.id.clone(),
                    revision: resumed.revision,
                },
            )
            .expect("complete");
        assert_eq!(completed.revision, 5);
        assert_eq!(completed.phase, GoalPhase::Complete);
        assert_eq!(completed.activation, GoalActivation::Disarmed);

        let tombstone = service
            .clear(
                &subject,
                &GoalRef {
                    id: completed.id.clone(),
                    revision: completed.revision,
                },
            )
            .expect("clear");
        assert_eq!(tombstone.id, completed.id);
        assert_eq!(tombstone.revision, 6);
        assert!(service.get(&subject).expect("get after clear").is_none());
    }

    #[test]
    fn rejects_already_exists_stale_and_invalid_transitions() {
        let (_context, subject, service) = setup("rejections");

        let created = service
            .create(
                &subject,
                &CreateGoalRequest {
                    objective: "first".to_owned(),
                    max_goal_rounds: None,
                },
            )
            .expect("create");

        let error = service
            .create(
                &subject,
                &CreateGoalRequest {
                    objective: "second".to_owned(),
                    max_goal_rounds: None,
                },
            )
            .expect_err("already exists");
        assert!(error.to_string().contains("already exists"));

        let error = service
            .complete(
                &subject,
                &GoalRef {
                    id: created.id.clone(),
                    revision: 99,
                },
            )
            .expect_err("stale");
        assert!(error.to_string().contains("stale goal ref"));

        let error = service
            .resume(
                &subject,
                &GoalRef {
                    id: created.id.clone(),
                    revision: created.revision,
                },
            )
            .expect_err("armed");
        assert!(error.to_string().contains("already active and armed"));
    }

    #[test]
    fn injected_time_identity_block_resume_and_session_start_are_deterministic() {
        let environment = TestEnvironment::new([100, 90, 110, 120], "goal-fixed");
        let (context, subject, service) = setup_with_environment("deterministic", environment);
        let created = service
            .create(
                &subject,
                &CreateGoalRequest {
                    objective: "finish".into(),
                    max_goal_rounds: Some(3),
                },
            )
            .unwrap();
        assert_eq!(created.id, GoalId::new("goal-fixed"));
        assert_eq!((created.created_at, created.updated_at), (100, 100));
        let edited = service
            .edit(
                &subject,
                &GoalRef {
                    id: created.id.clone(),
                    revision: created.revision,
                },
                &EditGoalRequest {
                    objective: Some("finish fully".into()),
                    max_goal_rounds: None,
                },
            )
            .unwrap();
        assert_eq!(
            edited.updated_at, 100,
            "backward wall time clamps monotonically"
        );
        let blocked = service
            .block(
                &subject,
                &GoalRef {
                    id: edited.id.clone(),
                    revision: edited.revision,
                },
                &json!({"code":"waiting", "message":"Need approval"}),
            )
            .unwrap();
        assert_eq!(blocked.phase, GoalPhase::Blocked);
        assert_eq!(blocked.blocked_reason.as_ref().unwrap().code, "waiting");
        let resumed = service
            .resume(
                &subject,
                &GoalRef {
                    id: blocked.id.clone(),
                    revision: blocked.revision,
                },
            )
            .unwrap();
        assert_eq!(resumed.activation, GoalActivation::Armed);

        AgentEvents::new(context, subject.clone()).emit(
            "agent/session-start",
            SessionStartEvent {
                source: SessionStartSource::Resume,
            },
        );
        let disarmed = service.get(&subject).unwrap().unwrap();
        assert_eq!(disarmed.revision, resumed.revision);
        assert_eq!(disarmed.activation, GoalActivation::Disarmed);
    }

    #[test]
    fn seeded_session_reconstructs_goal_disarmed_and_corruption_repeats_after_valid_prefix() {
        let (_context, subject, service) = setup("seed-source");
        let created = service
            .create(
                &subject,
                &CreateGoalRequest {
                    objective: "persist".into(),
                    max_goal_rounds: Some(5),
                },
            )
            .unwrap();
        let paused = service
            .pause(
                &subject,
                &GoalRef {
                    id: created.id.clone(),
                    revision: created.revision,
                },
            )
            .unwrap();
        let seed = subject.session().events();

        let context = Context::new();
        let registry = Arc::new(AgentRegistry::new(context.clone()));
        registry.provide(&context).unwrap();
        let id = SessionId::new("seed-child");
        let session = Session::create(&id, Some(seed), None).unwrap();
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
        let child = Arc::new(Agent::new(
            id,
            AgentOptions::default(),
            session.clone(),
            inbox,
            context.clone(),
            ScopeKey::new(),
        ));
        registry.register(&context, &child, None).unwrap();
        let child_service = GoalService::install(&context, Config::default()).unwrap();
        let inherited = child_service.get(&child).unwrap().unwrap();
        assert_eq!(inherited.id, paused.id);
        assert_eq!(inherited.revision, paused.revision);
        assert_eq!(inherited.activation, GoalActivation::Disarmed);

        session
            .append(
                "goal/change",
                json!({"kind":"goal/change", "version":999}),
                AppendOptions::default(),
            )
            .unwrap();
        let first = child_service.get(&child).unwrap_err().to_string();
        let second = child_service.get(&child).unwrap_err().to_string();
        assert_eq!(first, second);
    }
}
