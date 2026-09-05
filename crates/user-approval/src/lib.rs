//! Turn-enclosed user approval, audit pairing, scoped answerers, and policy.

/// Package-owned durable approval invariants.
pub mod invariant;

use std::{
    future::Future,
    ops::Deref,
    panic::AssertUnwindSafe,
    pin::Pin,
    sync::{Arc, Weak},
};

use futures::{FutureExt, future::Either};
use parking_lot::Mutex;
use seekdeep_agent::Agent;
use seekdeep_cordis::{
    Context, CordisError, EventArgs, EventOptions, EventReply, Fiber, Plugin, ServiceKey,
    events::Next, fiber::EffectHandle,
};
use seekdeep_core::session::{AppendOptions, Session, SessionEvent};
use seekdeep_llm::{AbortSignal, CallId, ContentBlock, MessageSource, UserMessage};
use seekdeep_scope::{scope_target, scoped_event_args};
use seekdeep_system_prompt::{PromptContext, PromptText, SYSTEM_PROMPT, SystemPrompt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use uuid::Uuid;

/// Typed Cordis slot corresponding to `ctx.approval`.
pub const APPROVAL: ServiceKey<ApprovalService> = ServiceKey::new("approval");
/// Loader plugin identity.
pub const PLUGIN_NAME: &str = "user-approval";
/// Approval operates independently of its optional prompt presentation.
pub const PLUGIN_INJECT: &[&str] = &[];

/// Model-facing statement for the deterministic never policy.
pub const NEVER_SENTENCE: &str = "Approval prompts are disabled in this session: actions that require approval are rejected automatically — do not request sandbox escalation (do not set `sandbox_permissions`).";
/// Model-facing statement for the interactive policy.
pub const ASK_SENTENCE: &str = "Approval policy: ask. Operations that require approval may ask through the configured answerers; without an available answerer, the request fails closed.";

/// Correlates one durable `approval/asked` event with its decision.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApprovalRequestId(String);

impl ApprovalRequestId {
    /// Brands one wire identifier without changing or validating it.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    fn fresh() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// Wire string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Closed, fail-closed approval outcomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalOutcome {
    /// Grants this request only.
    AllowedOnce,
    /// Explicit user rejection.
    Rejected,
    /// Request withdrawn by cancellation.
    Cancelled,
    /// No conforming answerer was available.
    Unavailable,
}

impl ApprovalOutcome {
    /// Exact wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AllowedOnce => "allowed-once",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Answerer return boundary, including a deliberately non-conforming value
/// that the service normalizes fail-closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApprovalAnswer {
    /// One conforming closed outcome.
    Outcome(ApprovalOutcome),
    /// Hostile or stale implementation value.
    Unknown(String),
}

impl From<ApprovalOutcome> for ApprovalAnswer {
    fn from(value: ApprovalOutcome) -> Self {
        Self::Outcome(value)
    }
}

impl ApprovalAnswer {
    fn normalized(self) -> ApprovalOutcome {
        match self {
            Self::Outcome(outcome) => outcome,
            Self::Unknown(_) => ApprovalOutcome::Unavailable,
        }
    }
}

/// Per-session pre-answerer policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApprovalPolicy {
    /// Ask composed answerers and fail closed when none claims the request.
    #[default]
    Ask,
    /// Deterministically reject without dispatching to an answerer.
    Never,
}

impl ApprovalPolicy {
    /// Exact wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Never => "never",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "ask" => Some(Self::Ask),
            "never" => Some(Self::Never),
            _ => None,
        }
    }
}

/// Approval service configuration.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApprovalConfig {
    /// Deployment default for sessions without a durable override.
    pub policy: ApprovalPolicy,
}

/// Readonly same-process approval request.
#[derive(Clone, Debug)]
pub struct ApprovalRequest {
    /// Exact live agent requesting the operation.
    pub agent: Arc<Agent>,
    /// Tool or operation name.
    pub tool_name: String,
    /// Exact tool call, when present.
    pub call_id: Option<CallId>,
    /// Human-readable reason.
    pub reason: Option<String>,
    /// Optional withdrawal signal.
    pub signal: Option<AbortSignal>,
}

impl ApprovalRequest {
    /// Builds the mandatory request fields.
    #[must_use]
    pub fn new(agent: Arc<Agent>, tool_name: impl Into<String>) -> Self {
        Self {
            agent,
            tool_name: tool_name.into(),
            call_id: None,
            reason: None,
            signal: None,
        }
    }

    /// Attaches a tool-call identity.
    #[must_use]
    pub fn with_call_id(mut self, call_id: CallId) -> Self {
        self.call_id = Some(call_id);
        self
    }

    /// Attaches a human-readable reason.
    #[must_use]
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    /// Attaches a withdrawal signal.
    #[must_use]
    pub fn with_signal(mut self, signal: AbortSignal) -> Self {
        self.signal = Some(signal);
        self
    }
}

/// Typed continuation for the `approval/request` answerer waterfall.
pub struct ApprovalNext(Next);

impl ApprovalNext {
    /// Delegates to the remaining answerers or the unavailable default.
    ///
    /// # Errors
    ///
    /// Returns a downstream failure or invalid reply type.
    pub async fn run(self) -> anyhow::Result<ApprovalAnswer> {
        self.0
            .run()
            .await?
            .downcast::<ApprovalAnswer>()
            .map(|answer| (*answer).clone())
            .ok_or_else(|| anyhow::anyhow!("approval/request returned an invalid answer"))
    }
}

/// Stable service failures that must not be normalized into an outcome.
#[derive(Debug, Error)]
pub enum ApprovalError {
    /// The audit pair would sit outside a durable turn boundary.
    #[error(
        "approval.request() outside an open turn: the approval/asked + approval/decided audit pair must be turn-enclosed (a bare event between turns is crash-tail garbage on reload). Ask from inside the turn that needs the decision."
    )]
    OutsideOpenTurn,
}

/// Scoped answerer service with durable audit and policy enforcement.
pub struct ApprovalService {
    context: Context,
    config: ApprovalConfig,
}

impl std::fmt::Debug for ApprovalService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ApprovalService")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl ApprovalService {
    /// Constructs an unprovided service. The default policy is ask.
    #[must_use]
    pub fn new(context: Context, config: ApprovalConfig) -> Arc<Self> {
        Arc::new(Self { context, config })
    }

    /// Registers this exact service on `ctx.approval`.
    ///
    /// # Errors
    ///
    /// Returns standard duplicate-service or inactive-owner failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> Result<EffectHandle, CordisError> {
        context.provide(APPROVAL, self.clone())
    }

    /// Deployment default approval policy.
    #[must_use]
    pub const fn policy(&self) -> ApprovalPolicy {
        self.config.policy
    }

    /// Registers a typed scoped answerer.
    ///
    /// # Errors
    ///
    /// Returns when the owner is inactive.
    pub fn on_request<F, Fut>(
        &self,
        context: &Context,
        answerer: F,
        options: EventOptions,
    ) -> Result<EffectHandle, CordisError>
    where
        F: Fn(ApprovalRequest, ApprovalNext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = anyhow::Result<ApprovalAnswer>> + Send + 'static,
    {
        register_approval_answerer_on(&self.context, context, answerer, options)
    }

    /// Asks one question, appending the sole durable asked/decided pair.
    ///
    /// # Errors
    ///
    /// Returns for an unenclosed request or a failed audit append.
    pub async fn request(&self, request: ApprovalRequest) -> anyhow::Result<ApprovalOutcome> {
        if !has_open_turn(&request.agent.session().events()) {
            return Err(ApprovalError::OutsideOpenTurn.into());
        }
        let id = ApprovalRequestId::fresh();
        let mut asked = serde_json::Map::from_iter([
            ("id".to_owned(), Value::String(id.as_str().to_owned())),
            (
                "toolName".to_owned(),
                Value::String(request.tool_name.clone()),
            ),
        ]);
        if let Some(call_id) = &request.call_id {
            asked.insert(
                "callId".to_owned(),
                Value::String(call_id.as_str().to_owned()),
            );
        }
        if let Some(reason) = &request.reason {
            asked.insert("reason".to_owned(), Value::String(reason.clone()));
        }
        request.agent.session().append(
            "approval/asked",
            Value::Object(asked),
            AppendOptions::default(),
        )?;
        let outcome = self.decide(&request).await;
        request.agent.session().append(
            "approval/decided",
            json!({"id": id.as_str(), "outcome": outcome.as_str()}),
            AppendOptions::default(),
        )?;
        Ok(outcome)
    }

    async fn decide(&self, request: &ApprovalRequest) -> ApprovalOutcome {
        if request.signal.as_ref().is_some_and(AbortSignal::is_aborted) {
            return ApprovalOutcome::Cancelled;
        }
        if self.effective_policy(request.agent.session()) == ApprovalPolicy::Never {
            return ApprovalOutcome::Rejected;
        }
        let args = scoped_event_args(request.agent.scope_key(), EventArgs::one(request.clone()));
        let dispatch = scope_target(&self.context, Some(request.agent.scope_key()));
        let context = self.context.clone();
        let answer = Box::pin(async move {
            let answer = context
                .events()
                .waterfall(&dispatch, "approval/request", &args, || {
                    Box::pin(async {
                        Ok(EventReply::Value(Arc::new(ApprovalAnswer::Outcome(
                            ApprovalOutcome::Unavailable,
                        ))))
                    })
                });
            AssertUnwindSafe(answer)
                .catch_unwind()
                .await
                .ok()
                .and_then(Result::ok)
                .and_then(|reply| reply.downcast::<ApprovalAnswer>())
                .map_or(ApprovalOutcome::Unavailable, |answer| {
                    (*answer).clone().normalized()
                })
        });
        let Some(signal) = &request.signal else {
            return answer.await;
        };
        let cancelled = signal.cancelled();
        match futures::future::select(answer, cancelled).await {
            Either::Left((outcome, _)) => outcome,
            Either::Right(((), late_answer)) => {
                detach_late_answer(late_answer);
                ApprovalOutcome::Cancelled
            }
        }
    }

    /// Session override, excluding the deployment default.
    #[must_use]
    pub fn override_of(&self, session: &Session) -> Option<ApprovalPolicy> {
        effective_approval_policy(&session.events())
    }

    /// Current policy after applying the durable override.
    #[must_use]
    pub fn effective_policy(&self, session: &Session) -> ApprovalPolicy {
        self.override_of(session).unwrap_or(self.config.policy)
    }

    /// Switches one live agent and queues the transition for its next step.
    ///
    /// # Errors
    ///
    /// Returns a durable append or agent-injection failure.
    pub fn set_policy(&self, agent: &Agent, policy: ApprovalPolicy) -> anyhow::Result<()> {
        self.set_policy_with_inject(agent.session(), policy, |message| {
            agent.inject(message).map_err(Into::into)
        })
    }

    /// Switches a live subject through an explicit injection seam.
    ///
    /// This is the structural core used by [`Self::set_policy`] and by hosts
    /// whose live-agent facade owns message injection outside this crate.
    ///
    /// # Errors
    ///
    /// Returns a durable append or injection failure.
    pub fn set_policy_with_inject(
        &self,
        session: &Session,
        policy: ApprovalPolicy,
        inject: impl FnOnce(UserMessage) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let previous = self.effective_policy(session);
        if previous == policy {
            return Ok(());
        }
        set_approval_policy(session, policy)?;
        inject(UserMessage::new(
            vec![ContentBlock::Text {
                text: format!(
                    "The approval policy changed from \"{}\" to \"{}\" (changed by the user).",
                    previous.as_str(),
                    policy.as_str()
                ),
            }],
            MessageSource::plugin("user-approval"),
        ))?;
        Ok(())
    }

    fn watch_prompt_context(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let binding = Arc::new(Mutex::new(ApprovalPromptBinding::default()));
        let watched = context.clone();
        let watched_binding = binding.clone();
        let weak = Arc::downgrade(self);
        context.on_service_change(move || {
            if let Some(service) = weak.upgrade()
                && let Err(error) = service.reconcile_prompt_context(&watched, &watched_binding)
            {
                watched
                    .logger(Some(PLUGIN_NAME))
                    .error([json!(format!("{error:#}"))]);
            }
        })?;
        self.reconcile_prompt_context(context, &binding)?;
        context.own(EffectHandle::new("approval.prompt-binding", move || {
            Box::pin(async move {
                let handle = {
                    let mut binding = binding.lock();
                    binding.provider = None;
                    binding.handle.take()
                };
                if let Some(handle) = handle {
                    handle.dispose().await?;
                }
                Ok(())
            })
        }))?;
        Ok(())
    }

    fn reconcile_prompt_context(
        self: &Arc<Self>,
        context: &Context,
        binding: &Mutex<ApprovalPromptBinding>,
    ) -> anyhow::Result<()> {
        {
            let mut state = binding.lock();
            if state.reconciling {
                state.changed = true;
                return Ok(());
            }
            state.reconciling = true;
        }
        loop {
            binding.lock().changed = false;
            let result = self.reconcile_prompt_once(context, binding);
            let mut state = binding.lock();
            if result.is_err() || !state.changed {
                state.reconciling = false;
                return result;
            }
        }
    }

    fn reconcile_prompt_once(
        self: &Arc<Self>,
        context: &Context,
        binding: &Mutex<ApprovalPromptBinding>,
    ) -> anyhow::Result<()> {
        let prompt = (!context.fiber().is_disposal_requested())
            .then(|| context.get(SYSTEM_PROMPT))
            .flatten();
        let old_handle = {
            let mut state = binding.lock();
            let previous = state.provider.as_ref().and_then(Weak::upgrade);
            let unchanged = match (&previous, &prompt) {
                (Some(previous), Some(prompt)) => Arc::ptr_eq(previous, prompt),
                (None, None) => state.handle.is_none(),
                _ => false,
            };
            if unchanged {
                return Ok(());
            }
            state.provider = None;
            state.handle.take()
        };
        // Layer notifications can publish services reentrantly; never hold the binding lock here.
        if let Some(handle) = old_handle {
            futures::executor::block_on(handle.dispose())?;
        }
        if let Some(prompt) = prompt {
            let handle = self.contribute_prompt_context(context, &prompt)?;
            let mut state = binding.lock();
            state.provider = Some(Arc::downgrade(&prompt));
            state.handle = Some(handle);
        }
        Ok(())
    }

    fn contribute_prompt_context(
        self: &Arc<Self>,
        context: &Context,
        prompt: &SystemPrompt,
    ) -> anyhow::Result<EffectHandle> {
        let weak = Arc::downgrade(self);
        let effect = prompt.prompt_context(
            context,
            PromptContext::new(
                "approval:policy",
                115.0,
                PromptText::Dynamic(Arc::new(move |assemble_context| {
                    let Some(session) = &assemble_context.agent_session else {
                        return Ok(String::new());
                    };
                    let service = weak.upgrade().ok_or_else(|| {
                        anyhow::anyhow!("approval service disposed before prompt assembly")
                    })?;
                    Ok(match service.effective_policy(session) {
                        ApprovalPolicy::Ask => ASK_SENTENCE.to_owned(),
                        ApprovalPolicy::Never => NEVER_SENTENCE.to_owned(),
                    })
                })),
            ),
        )?;
        Ok(effect)
    }
}

#[derive(Default)]
struct ApprovalPromptBinding {
    provider: Option<Weak<SystemPrompt>>,
    handle: Option<EffectHandle>,
    reconciling: bool,
    changed: bool,
}

/// Registers a typed approval answerer without requiring the approval service
/// to have been published yet.
///
/// This is the event-level contract used by hosts that load before the service
/// provider. The eventual service dispatches through the same root event bus.
///
/// # Errors
///
/// Returns when the owner context is inactive.
pub fn register_approval_answerer<F, Fut>(
    context: &Context,
    answerer: F,
    options: EventOptions,
) -> Result<EffectHandle, CordisError>
where
    F: Fn(ApprovalRequest, ApprovalNext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = anyhow::Result<ApprovalAnswer>> + Send + 'static,
{
    register_approval_answerer_on(context, context, answerer, options)
}

fn register_approval_answerer_on<F, Fut>(
    event_context: &Context,
    owner_context: &Context,
    answerer: F,
    options: EventOptions,
) -> Result<EffectHandle, CordisError>
where
    F: Fn(ApprovalRequest, ApprovalNext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = anyhow::Result<ApprovalAnswer>> + Send + 'static,
{
    event_context.events().on_waterfall(
        owner_context,
        "approval/request",
        move |_, args, next| {
            let Some(request) = args.get::<ApprovalRequest>(0) else {
                return Box::pin(async {
                    Err(anyhow::anyhow!("approval/request is missing its request"))
                });
            };
            let future = answerer((*request).clone(), ApprovalNext(next));
            Box::pin(async move { Ok(EventReply::Value(Arc::new(future.await?))) })
        },
        options,
    )
}

/// Installed approval service plus its reversible composition boundary.
pub struct ApprovalInstallation {
    service: Arc<ApprovalService>,
    effect: EffectHandle,
}

impl ApprovalInstallation {
    /// Exact service instance.
    #[must_use]
    pub fn service(&self) -> Arc<ApprovalService> {
        self.service.clone()
    }

    /// Disposes the service and prompt contribution together.
    ///
    /// # Errors
    ///
    /// Returns aggregate cleanup failures.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        self.effect.dispose().await
    }
}

impl Deref for ApprovalInstallation {
    type Target = ApprovalService;

    fn deref(&self) -> &Self::Target {
        &self.service
    }
}

/// Installs the service and its opportunistic system-prompt contribution.
///
/// # Errors
///
/// Returns duplicate-service, prompt registration, or ownership failures.
pub fn install(context: &Context, config: ApprovalConfig) -> anyhow::Result<ApprovalInstallation> {
    let fiber = Fiber::active_child("user-approval");
    let child = context.with_fiber(fiber.clone());
    let service = ApprovalService::new(context.clone(), config);
    let install_result = (|| {
        service.provide(&child)?;
        service.watch_prompt_context(&child)?;
        Ok::<(), anyhow::Error>(())
    })();
    if let Err(error) = install_result {
        return match futures::executor::block_on(fiber.dispose()) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(anyhow::anyhow!("{error:#}: cleanup failed: {cleanup:#}")),
        };
    }
    let cleanup_fiber = fiber.clone();
    let effect = EffectHandle::new("user-approval", move || {
        Box::pin(async move { cleanup_fiber.dispose().await })
    });
    if let Err(error) = context.own(effect.clone()) {
        return match futures::executor::block_on(fiber.dispose()) {
            Ok(()) => Err(error.into()),
            Err(cleanup) => Err(anyhow::anyhow!("{error}: cleanup failed: {cleanup:#}")),
        };
    }
    Ok(ApprovalInstallation { service, effect })
}

/// Builds the Loader-compatible approval plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(
        PLUGIN_NAME,
        PLUGIN_INJECT.iter().copied(),
        |context, config| {
            Box::pin(async move {
                install(&context, serde_json::from_value(config)?)?;
                Ok(())
            })
        },
    )
}

/// Folds the last durable approval policy override.
#[must_use]
pub fn effective_approval_policy(events: &[SessionEvent]) -> Option<ApprovalPolicy> {
    events.iter().rev().find_map(|event| {
        (event.event_type == "approval/policy")
            .then(|| event.data.get("policy").and_then(Value::as_str))
            .flatten()
            .and_then(ApprovalPolicy::parse)
    })
}

/// Appends one validated durable policy override.
///
/// # Errors
///
/// Returns an append failure.
pub fn set_approval_policy(
    session: &Session,
    policy: ApprovalPolicy,
) -> anyhow::Result<SessionEvent> {
    Ok(session.append(
        "approval/policy",
        json!({"policy": policy.as_str()}),
        AppendOptions::default(),
    )?)
}

/// Validates an untrusted policy string before appending.
///
/// # Errors
///
/// Returns the source diagnostic for an unknown policy or an append failure.
pub fn set_approval_policy_str(session: &Session, policy: &str) -> anyhow::Result<SessionEvent> {
    let policy = ApprovalPolicy::parse(policy)
        .ok_or_else(|| anyhow::anyhow!("approval policy must be one of \"ask\" or \"never\""))?;
    set_approval_policy(session, policy)
}

fn has_open_turn(events: &[SessionEvent]) -> bool {
    events
        .iter()
        .rev()
        .find_map(|event| match event.event_type.as_str() {
            "turn/start" => Some(true),
            "turn/end" => Some(false),
            _ => None,
        })
        .unwrap_or(false)
}

fn detach_late_answer(answer: Pin<Box<dyn Future<Output = ApprovalOutcome> + Send + 'static>>) {
    let task = async move {
        let _ = answer.await;
    };
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(task);
    } else {
        std::thread::spawn(move || futures::executor::block_on(task));
    }
}

#[async_trait::async_trait]
impl seekdeep_sandbox::EscalationApprover<Arc<Agent>, CallId> for ApprovalService {
    async fn request(
        &self,
        ask: seekdeep_sandbox::EscalationAsk<Arc<Agent>, CallId>,
    ) -> seekdeep_sandbox::EscalationOutcome {
        let request = ApprovalRequest {
            agent: ask.agent,
            tool_name: ask.tool_name,
            call_id: Some(ask.call_id),
            reason: Some(ask.reason),
            signal: ask.signal,
        };
        match self.request(request).await {
            Ok(ApprovalOutcome::AllowedOnce) => seekdeep_sandbox::EscalationOutcome::AllowedOnce,
            Ok(ApprovalOutcome::Rejected) => seekdeep_sandbox::EscalationOutcome::Rejected,
            Ok(ApprovalOutcome::Cancelled) => seekdeep_sandbox::EscalationOutcome::Cancelled,
            Ok(ApprovalOutcome::Unavailable) | Err(_) => {
                seekdeep_sandbox::EscalationOutcome::Unavailable
            }
        }
    }
}
