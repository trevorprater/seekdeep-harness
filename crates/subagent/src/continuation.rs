//! Internal continuable-subagent manager: stable child ids, descriptor
//! persistence, activation admission, the live ownership graph, cold resume,
//! child-first disposal, and settlement delivery to the parent, behind
//! ctx.subagents.
//!
//! A continuable child has one durable Session and at most one process-local
//! Activation - one residency epoch for a reconstructed child Agent. An
//! Activation is not a request, result, cancellation, or task boundary: it may
//! execute many FIFO turns and stays resident while descendants it created are
//! still running. The Agent inbox is the only turn queue, so this manager owns
//! residency while the Agent loop owns all turn ordering and execution.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_agent::{
    AGENT, AGENTS, Agent, AgentCancelCause, AgentHandle, AgentOptions, AgentRegistry, AgentSetup,
    AgentStatus, CancelOptions, CreateAgentMeta, CreateAgentOptions, ResumeAgentOptions,
};
use seekdeep_agent_loop::{AgentInboxClaimed, AgentInboxMessage};
use seekdeep_cordis::{
    Context, EventOptions, EventReply,
    fiber::{DisposeFuture, EffectHandle},
};
use seekdeep_core::{
    session::{SessionEvent, SessionId},
    session_store::SESSIONS,
};
use seekdeep_llm::{
    AbortSignal, ContentBlock, MessageId, MessageSource, ModelId, ProviderId, UserMessage,
    bound_context_summary, error_chain,
};
use seekdeep_session_persistence::{SESSION_PERSISTENCE, SessionPersistence};
use seekdeep_tools::ToolRestriction;
use serde_json::{Map, Value};
use tokio::sync::{Notify, OnceCell, watch};
use uuid::Uuid;

use crate::{
    activation_setup_registry::SubagentActivationSetupRegistry,
    child_agent::{
        ChildComposition, DelegatedPolicyOverrides, append_delegated_policy_overrides,
        apply_child_composition, capture_delegated_policy_overrides, child_session_meta,
        resolve_child_agent_options, resolve_child_depth,
    },
    depth::assert_subagent_max_depth,
    descriptor::{
        SubagentDescriptorData, SubagentDescriptorInput, fold_subagent_descriptor,
        snapshot_subagent_descriptor,
    },
    descriptor_seed::seed_descriptor_turn,
    error::SubagentError,
    lifecycle::{ActivationObserver, ActivationTerminal},
    types::{ContinuableCreateRequest, ContinuableCreateSpec, SubagentStopReason},
};

/// Deployment scheduling policy for accepted child reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentReportDelivery {
    /// Deliver without waking an idle parent.
    Quiet,
    /// Deliver as an ordinary waking turn.
    Wakeup,
}

/// Options for one continuable child's report to its direct parent.
#[derive(Clone, Debug)]
pub struct SubagentReportOptions {
    /// Already-resolved parent scheduling policy.
    pub delivery: SubagentReportDelivery,
    /// Caller cancellation, owning authorization and admission until acceptance.
    pub signal: AbortSignal,
}

/// The delegation request for a continuable child (no label/signal/outputSchema).
#[derive(Clone, Debug)]
pub struct ContinuableStartRequest {
    /// Content delivered as the child's user message.
    pub prompt: Vec<ContentBlock>,
    /// The spawning agent.
    pub parent: Arc<Agent>,
    /// Optional per-child agent options.
    pub agent_options: Option<AgentOptions>,
    /// Optional absolute delegation-depth cap.
    pub max_depth: Option<u64>,
    /// Optional child tool scoping.
    pub tool_filter: Option<ToolRestriction>,
    /// Optional per-child persona.
    pub persona: Option<String>,
}

/// What a caller asks for when starting a continuable background child.
#[derive(Clone, Debug)]
pub struct ContinuableStartSpec {
    /// The provider whose continuable-creation capability establishes the child.
    pub provider: String,
    /// The initial delegation's short description, persisted as the creation label.
    pub label: String,
    /// The delegation request.
    pub request: ContinuableStartRequest,
    /// Caller cancellation, owning the operation only until inbox acceptance.
    pub signal: AbortSignal,
}

/// Identities returned once a continuable child accepted its initial prompt.
#[derive(Clone, Debug)]
pub struct ContinuableStart {
    /// The durable child session id, stable across activations.
    pub child_id: SessionId,
    /// The accepted initial prompt's inbox message id.
    pub message_id: MessageId,
}

/// Authority under which one interrupt request is admitted.
#[derive(Clone, Debug)]
pub enum SubagentInterruptAuthority {
    /// The durable direct-parent address a human client presented.
    User {
        /// Durable direct-parent session id.
        parent_session_id: SessionId,
    },
    /// The exact live Agent whose recorded lineage must contain the caller.
    Ancestor {
        /// Exact live ancestor agent.
        agent: Arc<Agent>,
    },
}

/// Options for following up with one continuable child.
#[derive(Clone, Debug)]
pub struct SubagentFollowupOptions {
    /// Durable attribution retained on the delivered message; it grants no authority.
    pub source: MessageSource,
    /// Caller cancellation, owning the operation only until inbox acceptance.
    pub signal: AbortSignal,
}

/// The residency state of one continuable child.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivationState {
    Running,
    Waiting,
    Settled,
}

/// Hooks the manager needs from the owning service.
pub trait ContinuationHost: Send + Sync {
    /// Resolve one provider's continuable-creation contribution.
    fn prepare_continuable(
        &self,
        name: &str,
        request: ContinuableCreateRequest,
    ) -> BoxFuture<'static, anyhow::Result<ContinuableCreateSpec>>;
    /// Build the lifecycle observer for one Activation's residency epoch.
    fn observe_activation(
        &self,
        provider: &str,
        child_id: &SessionId,
        parent: &Arc<Agent>,
    ) -> ActivationObserver;
}

/// Creation inputs for a fresh Activation materialization.
#[derive(Clone)]
struct CreateInputs {
    seed: Vec<SessionEvent>,
    meta: CreateAgentMeta,
    delegated_policies: DelegatedPolicyOverrides,
}

/// Inputs shared by fresh and resumed Activation materialization.
struct MaterializeInputs {
    child_id: SessionId,
    provider: String,
    parent: Arc<Agent>,
    create: Option<CreateInputs>,
    agent_options: AgentOptions,
    composition: ChildComposition,
    signal: AbortSignal,
}

/// One admitted materialization and the exact live ancestry observed at its
/// synchronous admission boundary.
struct Materialization {
    lineage: Vec<Arc<Agent>>,
    settled: Notify,
    settled_flag: AtomicBool,
}

impl Materialization {
    fn mark_settled(&self) {
        self.settled_flag.store(true, Ordering::Release);
        self.settled.notify_waiters();
    }

    async fn wait_settled(&self) {
        loop {
            if self.settled_flag.load(Ordering::Acquire) {
                return;
            }
            let notified = self.settled.notified();
            if self.settled_flag.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

/// Resettable settlement-watcher wake latch.
struct Poke {
    tx: watch::Sender<u64>,
}

impl Poke {
    fn new() -> Self {
        Self {
            tx: watch::channel(0).0,
        }
    }

    fn wake(&self) {
        let next = self.tx.borrow().wrapping_add(1);
        let _ = self.tx.send(next);
    }

    fn generation(&self) -> u64 {
        *self.tx.borrow()
    }

    async fn wait_after(&self, generation: u64) {
        let mut rx = self.tx.subscribe();
        loop {
            if *rx.borrow() != generation {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

/// Shared disposal completion for one Activation.
struct DisposalState {
    notify: Notify,
    result: OnceCell<Result<(), String>>,
}

/// One residency epoch for a reconstructed continuable child Agent.
struct Activation {
    child_id: SessionId,
    parent_session: SessionId,
    handle: AgentHandle,
    ancestry: Vec<Weak<Agent>>,
    owned_children: Mutex<HashSet<SessionId>>,
    observer: ActivationObserver,
    disposal: Mutex<Option<Arc<DisposalState>>>,
    accepted: Mutex<HashSet<MessageId>>,
    announced: AtomicBool,
    poke: Poke,
}

impl Activation {
    /// Resolves the current poke and installs a fresh one.
    fn wake(&self) {
        self.poke.wake();
    }
}

/// Whether one settlement attempt opened the disposal transaction.
enum SettleAttempt {
    No,
    Yes {
        done: BoxFuture<'static, anyhow::Result<()>>,
    },
}

/// Serialize each durable child's delivery, release, and disposal.
struct ChildLock {
    locks: Mutex<HashMap<SessionId, Arc<tokio::sync::Mutex<()>>>>,
}

impl ChildLock {
    fn new() -> Self {
        Self {
            locks: Mutex::new(HashMap::new()),
        }
    }

    async fn run<T, F, Fut>(&self, child_id: &SessionId, operation: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let lock = {
            let mut locks = self.locks.lock();
            locks
                .entry(child_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let guard = lock.lock().await;
        let result = operation().await;
        drop(guard);
        let mut locks = self.locks.lock();
        if let Some(existing) = locks.get(child_id)
            && Arc::ptr_eq(existing, &lock)
            && Arc::strong_count(&lock) == 2
        {
            locks.remove(child_id);
        }
        result
    }
}

/// One scoped-teardown root and the exact live lineage members retained under it.
struct ClosingScope {
    root: Arc<Agent>,
    members: Mutex<Vec<Arc<Agent>>>,
}

impl ClosingScope {
    fn contains(&self, agent: &Arc<Agent>) -> bool {
        self.members.lock().iter().any(|m| Arc::ptr_eq(m, agent))
    }
}

/// The teardown that closed continuable admission for an agent's lineage.
enum Closing {
    Manager,
    Root(Arc<Agent>),
}

/// The continuable-subagent orchestration service behind ctx.subagents.
pub struct SubagentContinuationManager {
    context: Context,
    host: Arc<dyn ContinuationHost>,
    setup_registry: Arc<SubagentActivationSetupRegistry>,
    self_weak: Weak<Self>,
    activations: Mutex<HashMap<SessionId, Arc<Activation>>>,
    materializations: Mutex<Vec<Arc<Materialization>>>,
    locks: ChildLock,
    closing_scopes: Mutex<HashMap<usize, Arc<ClosingScope>>>,
    draining: AtomicBool,
}

fn agent_ptr(agent: &Arc<Agent>) -> usize {
    Arc::as_ptr(agent) as usize
}

fn throw_if_aborted(signal: &AbortSignal) -> anyhow::Result<()> {
    if signal.is_aborted() {
        return Err(anyhow::anyhow!(
            "aborted: {}",
            signal.reason().unwrap_or(Value::Null)
        ));
    }
    Ok(())
}

fn settlement_summary(child_id: &SessionId, stop_reason: SubagentStopReason) -> String {
    let subject = format!("Background subagent {child_id}");
    match stop_reason {
        SubagentStopReason::Completed => {
            format!("{subject} finished and will do no further work unless you send it more.")
        }
        SubagentStopReason::Aborted => format!("{subject} was stopped before it finished."),
        SubagentStopReason::MaxTokens => format!("{subject} ran out of room before it finished."),
        SubagentStopReason::Refusal => format!("{subject} declined the task."),
        SubagentStopReason::Error => format!("{subject} failed before it finished."),
    }
}

fn report_source(sender: &SessionId) -> MessageSource {
    let mut fields = Map::new();
    fields.insert("form".to_owned(), Value::String("relay".to_owned()));
    fields.insert(
        "senderSessionId".to_owned(),
        serde_json::to_value(sender).unwrap_or(Value::Null),
    );
    MessageSource {
        kind: "subagent-report".to_owned(),
        fields,
    }
}

fn settled_source(summary: &str, sender: &SessionId) -> MessageSource {
    let mut fields = Map::new();
    fields.insert("form".to_owned(), Value::String("notice".to_owned()));
    fields.insert(
        "summary".to_owned(),
        Value::String(bound_context_summary(summary)),
    );
    fields.insert(
        "senderSessionId".to_owned(),
        serde_json::to_value(sender).unwrap_or(Value::Null),
    );
    MessageSource {
        kind: "subagent-settled".to_owned(),
        fields,
    }
}

impl SubagentContinuationManager {
    /// Constructs the manager and registers its private teardown effects.
    ///
    /// # Errors
    ///
    /// Returns event-listener or effect-ownership failures.
    pub fn new(
        context: &Context,
        host: Arc<dyn ContinuationHost>,
        setup_registry: Arc<SubagentActivationSetupRegistry>,
    ) -> anyhow::Result<Arc<Self>> {
        let manager = Arc::new_cyclic(|weak| Self {
            context: context.clone(),
            host,
            setup_registry,
            self_weak: weak.clone(),
            activations: Mutex::new(HashMap::new()),
            materializations: Mutex::new(Vec::new()),
            locks: ChildLock::new(),
            closing_scopes: Mutex::new(HashMap::new()),
            draining: AtomicBool::new(false),
        });

        {
            let manager = Arc::clone(&manager);
            context.events().on_sync(
                context,
                "agent/disposed",
                move |_ctx, args| {
                    if let Some(payload) = args.get::<seekdeep_agent::AgentLifecycleEvent>(0) {
                        manager
                            .closing_scopes
                            .lock()
                            .remove(&agent_ptr(&payload.agent));
                    }
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )?;
        }

        {
            let manager = Arc::clone(&manager);
            let effect =
                EffectHandle::new("subagents.continuations()", move || -> DisposeFuture {
                    let manager = Arc::clone(&manager);
                    Box::pin(async move { manager.drain().await })
                });
            context.own(effect)?;
        }

        Ok(manager)
    }

    fn arc_self(&self) -> Arc<Self> {
        self.self_weak
            .upgrade()
            .expect("continuation manager alive")
    }

    fn agents(&self) -> Option<Arc<AgentRegistry>> {
        self.context.get(AGENTS)
    }

    /// Start one continuable background child.
    ///
    /// # Errors
    ///
    /// Returns when continuation services are unavailable or materialization fails.
    pub async fn start_continuable(
        &self,
        spec: ContinuableStartSpec,
    ) -> anyhow::Result<ContinuableStart> {
        let request = &spec.request;
        let parent = Arc::clone(&request.parent);
        self.assert_admitting(&parent)?;
        self.require_persistence()?;
        assert_subagent_max_depth(request.max_depth);
        let child_id = SessionId::new(Uuid::new_v4().to_string());
        let child_depth = resolve_child_depth(&parent, request.max_depth)?;
        let agent_provider = request
            .agent_options
            .as_ref()
            .and_then(|o| o.provider.clone())
            .or_else(|| parent.options().provider.clone())
            .map(|provider| provider.as_str().to_owned());
        let agent_model = request
            .agent_options
            .as_ref()
            .and_then(|o| o.model.clone())
            .or_else(|| parent.options().model.clone())
            .map(|model| model.as_str().to_owned());
        let descriptor = snapshot_subagent_descriptor(&SubagentDescriptorInput::Continuable {
            provider: spec.provider.clone(),
            label: spec.label.clone(),
            agent_provider,
            agent_model,
            persona: request.persona.clone(),
            tool_filter: request.tool_filter.clone(),
        })?;
        let delegated_policies = capture_delegated_policy_overrides(&parent);

        let prepared = self
            .host
            .prepare_continuable(
                &spec.provider,
                ContinuableCreateRequest {
                    session_id: child_id.clone(),
                    parent: Arc::clone(&parent),
                    signal: spec.signal.clone(),
                },
            )
            .await?;
        throw_if_aborted(&spec.signal)?;
        self.assert_admitting(&parent)?;

        let lineage_seed_length = prepared.seed.as_ref().map_or(0, |seed| seed.len() as u64);
        let seed = seed_descriptor_turn(&child_id, prepared.seed, &descriptor)?;
        let message_id = self
            .locks
            .run(&child_id, || async {
                let activation = self
                    .materialize(MaterializeInputs {
                        child_id: child_id.clone(),
                        provider: spec.provider.clone(),
                        parent: Arc::clone(&parent),
                        create: Some(CreateInputs {
                            seed,
                            meta: child_session_meta(&parent, child_depth, lineage_seed_length),
                            delegated_policies,
                        }),
                        agent_options: resolve_child_agent_options(
                            &parent,
                            request.agent_options.clone(),
                            child_depth,
                        ),
                        composition: ChildComposition {
                            persona: request.persona.clone(),
                            tool_filter: request.tool_filter.clone(),
                        },
                        signal: spec.signal.clone(),
                    })
                    .await?;
                self.submit_materialized(
                    &activation,
                    request.prompt.clone(),
                    MessageSource::user(),
                    &parent,
                    &spec.signal,
                )
                .await
            })
            .await?;
        Ok(ContinuableStart {
            child_id,
            message_id,
        })
    }

    /// Deliver one later message to a known continuable child as its next FIFO turn.
    ///
    /// # Errors
    ///
    /// Returns when continuation services are unavailable, parent authority is
    /// rejected, or the message was not admitted.
    pub async fn followup(
        &self,
        parent: &Arc<Agent>,
        child_id: &SessionId,
        content: Vec<ContentBlock>,
        options: SubagentFollowupOptions,
    ) -> anyhow::Result<MessageId> {
        self.assert_admitting(parent)?;
        loop {
            let live: Option<MessageId> = self
                .locks
                .run(child_id, || async {
                    let activation = self.activations.lock().get(child_id).cloned();
                    match activation {
                        None => Ok::<_, anyhow::Error>(Some(
                            self.cold_resume(parent, child_id, &content, &options)
                                .await?,
                        )),
                        Some(activation) => {
                            if activation.disposal.lock().is_some() {
                                let disposal = self.dispose(&activation);
                                let _ = disposal.await;
                                Ok::<_, anyhow::Error>(None)
                            } else {
                                Ok::<_, anyhow::Error>(Some(self.submit_admitted(
                                    &activation,
                                    content.clone(),
                                    options.source.clone(),
                                    parent,
                                    &options.signal,
                                )?))
                            }
                        }
                    }
                })
                .await?;
            if let Some(message_id) = live {
                return Ok(message_id);
            }
            self.assert_admitting(parent)?;
            throw_if_aborted(&options.signal)?;
        }
    }

    /// Interrupt one live continuable child's current turn.
    ///
    /// # Errors
    ///
    /// Returns the UNAUTHORIZED code when the authority does not own the live target.
    #[allow(clippy::needless_pass_by_value)]
    pub fn interrupt(
        &self,
        target_session_id: &SessionId,
        authority: SubagentInterruptAuthority,
    ) -> anyhow::Result<()> {
        if let SubagentInterruptAuthority::Ancestor { agent: caller } = &authority {
            let current = self.agents().and_then(|r| r.get(caller.id()));
            if current.is_none_or(|current| !Arc::ptr_eq(&current, caller)) {
                return Err(SubagentError::new(
                    format!(
                        "interrupting \"{target_session_id}\" requires the exact live ancestor agent"
                    ),
                    "UNAUTHORIZED",
                )
                .into());
            }
            if caller.id() == target_session_id {
                return Err(SubagentError::new(
                    format!("agent \"{}\" cannot interrupt itself", caller.id()),
                    "UNAUTHORIZED",
                )
                .into());
            }
        }
        let activation = self.activations.lock().get(target_session_id).cloned();
        let Some(activation) = activation else {
            return Ok(());
        };
        match &authority {
            SubagentInterruptAuthority::User { parent_session_id } => {
                if activation
                    .handle
                    .agent
                    .session()
                    .header()
                    .parent_session
                    .as_ref()
                    != Some(parent_session_id)
                {
                    return Err(SubagentError::new(
                        format!(
                            "subagent \"{target_session_id}\" belongs to another parent session"
                        ),
                        "UNAUTHORIZED",
                    )
                    .into());
                }
            }
            SubagentInterruptAuthority::Ancestor { agent } => {
                if !activation
                    .ancestry
                    .iter()
                    .any(|weak| weak.upgrade().is_some_and(|a| Arc::ptr_eq(&a, agent)))
                {
                    return Err(SubagentError::new(
                        format!(
                            "subagent \"{target_session_id}\" is not a live descendant of agent \"{}\"",
                            agent.id()
                        ),
                        "UNAUTHORIZED",
                    )
                    .into());
                }
            }
        }
        if activation.disposal.lock().is_some() {
            return Ok(());
        }
        let cause = match authority {
            SubagentInterruptAuthority::User { .. } => AgentCancelCause::User,
            SubagentInterruptAuthority::Ancestor { .. } => AgentCancelCause::Parent,
        };
        let _ = activation
            .handle
            .agent
            .cancel(cause, CancelOptions { keep_inbox: true });
        Ok(())
    }

    /// Deliver explicitly selected content from one resident continuable child
    /// to its durable direct parent.
    ///
    /// # Errors
    ///
    /// Returns when continuation services are unavailable, sender authorization
    /// fails, or the direct parent is not live.
    #[allow(clippy::needless_pass_by_value)]
    pub fn report_from(
        &self,
        child: &Arc<Agent>,
        content: Vec<ContentBlock>,
        options: SubagentReportOptions,
    ) -> anyhow::Result<MessageId> {
        throw_if_aborted(&options.signal)?;
        self.assert_admitting(child)?;
        let activation = self.authorize_reporter(child)?;
        let parent = self.resolve_report_parent(child)?;
        self.deliver_report(&activation, &parent, content, options.delivery)
    }

    fn authorize_reporter(&self, child: &Arc<Agent>) -> anyhow::Result<Arc<Activation>> {
        let activation = self.activations.lock().get(child.id()).cloned();
        let Some(activation) = activation else {
            return Err(unauthorized_reporter(child.id()));
        };
        if !Arc::ptr_eq(&activation.handle.agent, child) {
            return Err(unauthorized_reporter(child.id()));
        }
        if activation.disposal.lock().is_some() {
            return Err(SubagentError::new(
                format!(
                    "subagent \"{}\" activation is being disposed; the report was not delivered",
                    child.id()
                ),
                "ACTIVATION_CLOSING",
            )
            .into());
        }
        Ok(activation)
    }

    fn resolve_report_parent(&self, child: &Arc<Agent>) -> anyhow::Result<Arc<Agent>> {
        let parent_id = child.session().header().parent_session.clone();
        let parent = parent_id.and_then(|id| self.agents().and_then(|r| r.get(&id)));
        parent.ok_or_else(|| {
            SubagentError::new(
                "direct parent is not live; report was not delivered",
                "PARENT_UNAVAILABLE",
            )
            .into()
        })
    }

    fn deliver_report(
        &self,
        activation: &Arc<Activation>,
        parent: &Arc<Agent>,
        content: Vec<ContentBlock>,
        delivery: SubagentReportDelivery,
    ) -> anyhow::Result<MessageId> {
        let mut blocks = vec![ContentBlock::Text {
            text: format!("Background subagent {} reported:", activation.child_id),
        }];
        blocks.extend(content);
        let message = UserMessage::new(blocks, report_source(&activation.child_id));
        let message_id = message.id().clone();
        if delivery == SubagentReportDelivery::Wakeup {
            self.send_waking(parent, &message, || {
                self.send_report(parent, &message, delivery)
            })?;
        } else {
            self.send_report(parent, &message, delivery)?;
        }
        Ok(message_id)
    }

    fn send_waking(
        &self,
        parent: &Arc<Agent>,
        message: &UserMessage,
        send: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let parent_activation = self.activations.lock().get(parent.id()).cloned();
        if let Some(parent_activation) =
            parent_activation.filter(|a| Arc::ptr_eq(&a.handle.agent, parent))
        {
            self.admit_waking(&parent_activation, message.id().clone(), send)?;
            Ok(())
        } else {
            send()
        }
    }

    #[allow(clippy::unused_self)]
    fn send_report(
        &self,
        parent: &Arc<Agent>,
        message: &UserMessage,
        delivery: SubagentReportDelivery,
    ) -> anyhow::Result<()> {
        let result = match delivery {
            SubagentReportDelivery::Wakeup => parent.followup(message.clone()),
            SubagentReportDelivery::Quiet => parent.inject(message.clone()),
        };
        result.map_err(|_error| {
            SubagentError::new(
                "direct parent is not live; report was not delivered",
                "PARENT_UNAVAILABLE",
            )
            .into()
        })
    }

    /// Close admission, await admitted materializations, then dispose the live
    /// Activation forest child-first.
    ///
    /// # Errors
    ///
    /// Returns an aggregate error when any branch failed to release.
    pub async fn drain(&self) -> anyhow::Result<()> {
        self.draining.store(true, Ordering::Release);
        let materializations = self.materializations.lock().clone();
        for materialization in materializations {
            materialization.wait_settled().await;
        }
        let owned: HashSet<SessionId> = {
            let activations = self.activations.lock();
            let mut owned = HashSet::new();
            for activation in activations.values() {
                for child in activation.owned_children.lock().iter() {
                    owned.insert(child.clone());
                }
            }
            owned
        };
        let roots: Vec<Arc<Activation>> = self
            .activations
            .lock()
            .values()
            .filter(|a| !owned.contains(&a.child_id))
            .cloned()
            .collect();
        self.dispose_roots(&roots, "activation(s)").await
    }

    /// Stop only the continuable descendants of exact live host-owned parents.
    ///
    /// # Errors
    ///
    /// Returns an aggregate error after all scoped branches settle when any failed.
    pub async fn drain_descendants(&self, parents: &[Arc<Agent>]) -> anyhow::Result<()> {
        let roots: Vec<Arc<Agent>> = parents
            .iter()
            .filter(|parent| {
                self.agents()
                    .and_then(|r| r.get(parent.id()))
                    .is_some_and(|current| Arc::ptr_eq(&current, parent))
            })
            .cloned()
            .collect();
        if roots.is_empty() {
            return Ok(());
        }

        for root in &roots {
            self.closing_members(root)
                .members
                .lock()
                .push(Arc::clone(root));
        }

        let mut targets: Vec<Arc<Activation>> = Vec::new();
        for activation in self.activations.lock().values() {
            let lineage = self.live_lineage(&activation.handle.agent);
            let owners: Vec<Arc<Agent>> = roots
                .iter()
                .filter(|root| {
                    !Arc::ptr_eq(&activation.handle.agent, root)
                        && activation
                            .ancestry
                            .iter()
                            .any(|weak| weak.upgrade().is_some_and(|a| Arc::ptr_eq(&a, root)))
                })
                .cloned()
                .collect();
            if owners.is_empty() {
                continue;
            }
            targets.push(Arc::clone(activation));
            for owner in &owners {
                let scope = self.closing_members(owner);
                scope
                    .members
                    .lock()
                    .push(Arc::clone(&activation.handle.agent));
                for agent in &lineage {
                    scope.members.lock().push(Arc::clone(agent));
                }
            }
        }

        let materializations: Vec<Arc<Materialization>> = self
            .materializations
            .lock()
            .iter()
            .filter(|materialization| {
                let owners: Vec<Arc<Agent>> = roots
                    .iter()
                    .filter(|root| {
                        materialization
                            .lineage
                            .iter()
                            .any(|agent| Arc::ptr_eq(agent, root))
                    })
                    .cloned()
                    .collect();
                for owner in &owners {
                    let scope = self.closing_members(owner);
                    for agent in &materialization.lineage {
                        scope.members.lock().push(Arc::clone(agent));
                    }
                }
                !owners.is_empty()
            })
            .cloned()
            .collect();

        let owned_targets: HashSet<SessionId> = {
            let mut set = HashSet::new();
            for activation in &targets {
                for child in activation.owned_children.lock().iter() {
                    set.insert(child.clone());
                }
            }
            set
        };
        let target_roots: Vec<Arc<Activation>> = targets
            .iter()
            .filter(|a| !owned_targets.contains(&a.child_id))
            .cloned()
            .collect();

        for activation in &targets {
            let disposal = self.dispose(activation);
            tokio::spawn(async move {
                let _ = disposal.await;
            });
        }

        for materialization in materializations {
            materialization.wait_settled().await;
        }
        self.dispose_roots(&target_roots, "scoped activation(s)")
            .await
    }

    async fn dispose_roots(
        &self,
        roots: &[Arc<Activation>],
        failure_subject: &'static str,
    ) -> anyhow::Result<()> {
        let results =
            futures::future::join_all(roots.iter().map(|activation| self.dispose(activation)))
                .await;
        let reasons: Vec<String> = results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .map(|error| error_chain(error.as_ref()))
            .collect();
        if !reasons.is_empty() {
            return Err(SubagentError::new(
                format!(
                    "continuable subagent teardown failed for {} {failure_subject}: {}",
                    reasons.len(),
                    reasons.join("; ")
                ),
                "ACTIVATION_TEARDOWN_FAILED",
            )
            .into());
        }
        Ok(())
    }

    fn closing_members(&self, root: &Arc<Agent>) -> Arc<ClosingScope> {
        let key = agent_ptr(root);
        let mut scopes = self.closing_scopes.lock();
        scopes
            .entry(key)
            .or_insert_with(|| {
                Arc::new(ClosingScope {
                    root: Arc::clone(root),
                    members: Mutex::new(Vec::new()),
                })
            })
            .clone()
    }

    /// Return the exact currently resolvable ancestry from `agent` upward.
    fn live_lineage(&self, agent: &Arc<Agent>) -> Vec<Arc<Agent>> {
        let mut lineage = vec![Arc::clone(agent)];
        let mut seen = HashSet::from([agent.id().clone()]);
        let mut parent_session = agent.session().header().parent_session.clone();
        while let Some(parent_id) = parent_session.clone() {
            let Some(parent) = self.agents().and_then(|r| r.get(&parent_id)) else {
                break;
            };
            if seen.contains(parent.id()) {
                break;
            }
            lineage.push(Arc::clone(&parent));
            seen.insert(parent.id().clone());
            parent_session.clone_from(&parent.session().header().parent_session);
        }
        lineage
    }

    fn closing_teardown_for(&self, agent: &Arc<Agent>) -> Option<Closing> {
        if self.draining.load(Ordering::Acquire) {
            return Some(Closing::Manager);
        }
        let lineage = self.live_lineage(agent);
        for scope in self.closing_scopes.lock().values() {
            if scope.contains(agent)
                || lineage
                    .iter()
                    .any(|ancestor| Arc::ptr_eq(ancestor, &scope.root))
            {
                return Some(Closing::Root(Arc::clone(&scope.root)));
            }
        }
        None
    }

    fn assert_admitting(&self, agent: &Arc<Agent>) -> anyhow::Result<()> {
        let Some(closing) = self.closing_teardown_for(agent) else {
            return Ok(());
        };
        let message = match closing {
            Closing::Manager => {
                "continuable subagents are draining; the operation was not admitted".to_owned()
            }
            Closing::Root(root) => format!(
                "continuable subagents below parent \"{}\" are draining; the operation was not admitted",
                root.id()
            ),
        };
        Err(SubagentError::new(message, "DRAINING").into())
    }

    #[allow(clippy::unused_self)]
    fn state_of(&self, activation: &Activation) -> ActivationState {
        if activation.handle.agent.status() == AgentStatus::Running
            || !activation.accepted.lock().is_empty()
        {
            ActivationState::Running
        } else if !activation.owned_children.lock().is_empty() {
            ActivationState::Waiting
        } else {
            ActivationState::Settled
        }
    }

    #[allow(clippy::cast_possible_truncation)] // seed length is a bounded event count on supported targets.
    async fn cold_resume(
        &self,
        parent: &Arc<Agent>,
        child_id: &SessionId,
        content: &[ContentBlock],
        options: &SubagentFollowupOptions,
    ) -> anyhow::Result<MessageId> {
        let persistence = self.require_persistence()?;
        let loaded = match persistence
            .inspect(child_id, Some(options.signal.clone()))
            .await
        {
            Ok(loaded) => loaded,
            Err(_error) => {
                throw_if_aborted(&options.signal)?;
                return Err(SubagentError::new(
                    format!("subagent \"{child_id}\" is unavailable"),
                    "NOT_RESUMABLE",
                )
                .into());
            }
        };
        throw_if_aborted(&options.signal)?;
        self.assert_admitting(parent)?;
        self.authorize_lineage(parent, child_id, loaded.meta.parent_session.as_ref())?;
        let start = loaded.meta.seed_length.unwrap_or(0) as usize;
        let suffix = &loaded.events[start.min(loaded.events.len())..];
        let descriptor = fold_subagent_descriptor(suffix)?;
        let (provider, agent_options, composition) = match descriptor {
            Some(SubagentDescriptorData::Continuable {
                provider,
                agent_provider,
                agent_model,
                persona,
                tool_filter,
                ..
            }) => (
                provider,
                AgentOptions {
                    provider: agent_provider.map(ProviderId::new),
                    model: agent_model.map(ModelId::new),
                    max_tokens: None,
                    subagent_depth: None,
                },
                ChildComposition {
                    persona,
                    tool_filter,
                },
            ),
            _ => {
                return Err(SubagentError::new(
                    format!(
                        "subagent \"{child_id}\" has no supported continuation state and cannot be resumed; do not retry send_message with this id"
                    ),
                    "NOT_RESUMABLE",
                )
                .into());
            }
        };
        let activation = match self
            .materialize(MaterializeInputs {
                child_id: child_id.clone(),
                provider,
                parent: Arc::clone(parent),
                create: None,
                agent_options,
                composition,
                signal: options.signal.clone(),
            })
            .await
        {
            Ok(activation) => activation,
            Err(error) => {
                throw_if_aborted(&options.signal)?;
                if error.downcast_ref::<SubagentError>().is_some() {
                    return Err(error);
                }
                return Err(SubagentError::new(
                    format!("subagent \"{child_id}\" is unavailable"),
                    "NOT_RESUMABLE",
                )
                .into());
            }
        };
        self.submit_materialized(
            &activation,
            content.to_vec(),
            options.source.clone(),
            parent,
            &options.signal,
        )
        .await
    }

    async fn submit_materialized(
        &self,
        activation: &Arc<Activation>,
        content: Vec<ContentBlock>,
        source: MessageSource,
        parent: &Arc<Agent>,
        signal: &AbortSignal,
    ) -> anyhow::Result<MessageId> {
        match self.submit_admitted(activation, content, source, parent, signal) {
            Ok(message_id) => Ok(message_id),
            Err(error) => {
                let _ = self.dispose(activation).await;
                Err(error)
            }
        }
    }

    async fn materialize(&self, inputs: MaterializeInputs) -> anyhow::Result<Arc<Activation>> {
        self.assert_admitting(&inputs.parent)?;
        let lineage = self.live_lineage(&inputs.parent);
        let materialization = Arc::new(Materialization {
            lineage: lineage.clone(),
            settled: Notify::new(),
            settled_flag: AtomicBool::new(false),
        });
        self.materializations
            .lock()
            .push(Arc::clone(&materialization));
        let result = self.materialize_tracked(inputs, &lineage).await;
        self.materializations
            .lock()
            .retain(|m| !Arc::ptr_eq(m, &materialization));
        materialization.mark_settled();
        result
    }

    #[allow(clippy::too_many_lines)] // One ordered materialization transaction mirrors the source.
    async fn materialize_tracked(
        &self,
        inputs: MaterializeInputs,
        parent_lineage: &[Arc<Agent>],
    ) -> anyhow::Result<Arc<Activation>> {
        let MaterializeInputs {
            child_id,
            provider,
            parent,
            create,
            agent_options,
            composition,
            signal,
        } = inputs;
        throw_if_aborted(&signal)?;

        let setup_registry = Arc::clone(&self.setup_registry);
        let setup_create = create.clone();
        let setup_composition = composition.clone();
        let setup: AgentSetup = Arc::new(move |child_ctx: Context| {
            let setup_create = setup_create.clone();
            let setup_composition = setup_composition.clone();
            let setup_registry = Arc::clone(&setup_registry);
            Box::pin(async move {
                if let Some(create) = &setup_create {
                    let agent = child_ctx
                        .get(AGENT)
                        .ok_or_else(|| anyhow::anyhow!("subagent child requires agent"))?;
                    append_delegated_policy_overrides(
                        agent.session().as_ref(),
                        &create.delegated_policies,
                    )?;
                }
                apply_child_composition(&child_ctx, &setup_composition)?;
                let commit = setup_registry.apply(&child_ctx)?;
                Ok(Some(commit))
            })
        });

        let observer = self.host.observe_activation(&provider, &child_id, &parent);
        let registry = self
            .agents()
            .ok_or_else(|| anyhow::anyhow!("continuable subagents require the agents service"))?;
        let handle = match create {
            None => {
                registry
                    .resume(ResumeAgentOptions {
                        resume_session_id: child_id.clone(),
                        agent_options,
                        signal: Some(signal.clone()),
                        setup: Some(setup),
                        owner_agent: None,
                    })
                    .await?
            }
            Some(create) => {
                registry
                    .create(CreateAgentOptions {
                        session_id: child_id.clone(),
                        meta: create.meta,
                        seed: Some(create.seed),
                        agent_options,
                        signal: Some(signal.clone()),
                        setup: Some(setup),
                        owner_agent: None,
                    })
                    .await?
            }
        };

        let child_agent = handle.agent.clone();
        let mut ancestry: Vec<Weak<Agent>> = parent_lineage.iter().map(Arc::downgrade).collect();
        ancestry.push(Arc::downgrade(&child_agent));
        let activation = Arc::new(Activation {
            child_id,
            parent_session: parent.id().clone(),
            handle,
            ancestry,
            owned_children: Mutex::new(HashSet::new()),
            observer,
            disposal: Mutex::new(None),
            accepted: Mutex::new(HashSet::new()),
            announced: AtomicBool::new(false),
            poke: Poke::new(),
        });
        self.activations
            .lock()
            .insert(activation.child_id.clone(), Arc::clone(&activation));

        let result = (|| -> anyhow::Result<()> {
            throw_if_aborted(&signal)?;
            self.assert_admitting(&parent)?;
            self.acquire_ownership(&parent, &activation.child_id)?;

            {
                let claimed = Arc::clone(&activation);
                child_agent.context().events().on_sync(
                    child_agent.context(),
                    "agent/inbox/claimed",
                    move |_ctx, args| {
                        if let Some(payload) = args.get::<AgentInboxClaimed>(0)
                            && claimed.accepted.lock().remove(payload.message.id())
                        {
                            claimed.wake();
                        }
                        Ok(EventReply::Undefined)
                    },
                    EventOptions::default(),
                )?;
                let discarded = Arc::clone(&activation);
                child_agent.context().events().on_sync(
                    child_agent.context(),
                    "agent/inbox/discarded",
                    move |_ctx, args| {
                        if let Some(payload) = args.get::<AgentInboxMessage>(0)
                            && discarded.accepted.lock().remove(payload.message.id())
                        {
                            discarded.wake();
                        }
                        Ok(EventReply::Undefined)
                    },
                    EventOptions::default(),
                )?;
            }

            activation.observer.start(child_agent.as_ref());
            Ok(())
        })();

        match result {
            Ok(()) => {
                self.watch_settlement(&activation);
                Ok(activation)
            }
            Err(error) => {
                let _ = self.rollback_unpublished(&activation).await;
                Err(error)
            }
        }
    }

    async fn rollback_unpublished(&self, activation: &Arc<Activation>) -> anyhow::Result<()> {
        self.dispose(activation).await
    }

    fn acquire_ownership(&self, parent: &Arc<Agent>, child_id: &SessionId) -> anyhow::Result<()> {
        let parent_activation = self.activations.lock().get(parent.id()).cloned();
        let Some(parent_activation) = parent_activation else {
            return Ok(());
        };
        if parent_activation.disposal.lock().is_some() {
            return Err(SubagentError::new(
                format!(
                    "subagent parent \"{}\" is being disposed; the child was not established",
                    parent.id()
                ),
                "ACTIVATION_CLOSING",
            )
            .into());
        }
        parent_activation
            .owned_children
            .lock()
            .insert(child_id.clone());
        Ok(())
    }

    fn release_ownership(&self, child_id: &SessionId) {
        let activations: Vec<Arc<Activation>> = self.activations.lock().values().cloned().collect();
        for candidate in activations {
            if candidate.owned_children.lock().remove(child_id) {
                candidate.wake();
            }
        }
    }

    fn submit_admitted(
        &self,
        activation: &Arc<Activation>,
        content: Vec<ContentBlock>,
        source: MessageSource,
        parent: &Arc<Agent>,
        signal: &AbortSignal,
    ) -> anyhow::Result<MessageId> {
        throw_if_aborted(signal)?;
        self.assert_admitting(parent)?;
        if activation.disposal.lock().is_some() {
            return Err(SubagentError::new(
                format!(
                    "subagent \"{}\" activation is being disposed; the message was not accepted",
                    activation.child_id
                ),
                "ACTIVATION_CLOSING",
            )
            .into());
        }
        self.authorize_lineage(
            parent,
            &activation.child_id,
            activation
                .handle
                .agent
                .session()
                .header()
                .parent_session
                .as_ref(),
        )?;
        self.submit(activation, content, source, parent)
    }

    fn submit(
        &self,
        activation: &Arc<Activation>,
        content: Vec<ContentBlock>,
        source: MessageSource,
        parent: &Arc<Agent>,
    ) -> anyhow::Result<MessageId> {
        self.acquire_ownership(parent, &activation.child_id)?;
        let message = UserMessage::new(content, source);
        let message_id = message.id().clone();
        self.admit_waking(activation, message_id.clone(), || {
            activation
                .handle
                .agent
                .followup(message.clone())
                .map_err(anyhow::Error::from)
        })?;
        activation.announced.store(true, Ordering::Release);
        Ok(message_id)
    }

    #[allow(clippy::unused_self)]
    fn admit_waking(
        &self,
        activation: &Arc<Activation>,
        message_id: MessageId,
        send: impl FnOnce() -> anyhow::Result<()>,
    ) -> anyhow::Result<MessageId> {
        activation.accepted.lock().insert(message_id.clone());
        if let Err(error) = send() {
            activation.accepted.lock().remove(&message_id);
            return Err(error);
        }
        activation.wake();
        Ok(message_id)
    }

    fn authorize_lineage(
        &self,
        parent: &Arc<Agent>,
        child_id: &SessionId,
        parent_session: Option<&SessionId>,
    ) -> anyhow::Result<()> {
        let current = self.agents().and_then(|r| r.get(parent.id()));
        if current.is_none_or(|current| !Arc::ptr_eq(&current, parent)) {
            return Err(SubagentError::new(
                format!("subagent \"{child_id}\" delivery requires the exact live parent agent"),
                "UNAUTHORIZED",
            )
            .into());
        }
        if parent_session != Some(parent.id()) {
            return Err(SubagentError::new(
                format!("subagent \"{child_id}\" belongs to another parent session"),
                "UNAUTHORIZED",
            )
            .into());
        }
        Ok(())
    }

    fn watch_settlement(&self, activation: &Arc<Activation>) {
        let manager = self.arc_self();
        let activation = Arc::clone(activation);
        tokio::spawn(async move {
            manager.watch_settlement_loop(&activation).await;
        });
    }

    async fn watch_settlement_loop(&self, activation: &Arc<Activation>) {
        while activation.disposal.lock().is_none() {
            let generation = activation.poke.generation();
            let idle = activation
                .handle
                .agent
                .when_idle()
                .unwrap_or_else(|error| Box::pin(async move { Err(error.into()) }));
            tokio::select! {
                result = idle => {
                    if result.is_err() {
                        return;
                    }
                },
                () = activation.poke.wait_after(generation) => {},
            }
            if activation.disposal.lock().is_some() {
                return;
            }
            let settling = self
                .locks
                .run(&activation.child_id, || async {
                    if activation.disposal.lock().is_some()
                        || self.state_of(activation) != ActivationState::Settled
                    {
                        return SettleAttempt::No;
                    }
                    SettleAttempt::Yes {
                        done: self.dispose(activation),
                    }
                })
                .await;
            match settling {
                SettleAttempt::No => {
                    if activation.handle.agent.status() != AgentStatus::Running {
                        activation.poke.wait_after(generation).await;
                    }
                }
                SettleAttempt::Yes { done } => {
                    if let Err(error) = done.await {
                        tracing::warn!(
                            child = %activation.child_id,
                            %error,
                            "subagent activation teardown failed"
                        );
                    }
                    return;
                }
            }
        }
    }

    fn dispose(&self, activation: &Arc<Activation>) -> BoxFuture<'static, anyhow::Result<()>> {
        let state = {
            let mut slot = activation.disposal.lock();
            if let Some(state) = slot.as_ref() {
                Arc::clone(state)
            } else {
                let state = Arc::new(DisposalState {
                    notify: Notify::new(),
                    result: OnceCell::new(),
                });
                *slot = Some(Arc::clone(&state));
                self.begin_disposal(activation, &state);
                state
            }
        };
        Box::pin(async move {
            loop {
                if let Some(result) = state.result.get() {
                    return match result {
                        Ok(()) => Ok(()),
                        Err(error) => Err(anyhow::anyhow!(error.clone())),
                    };
                }
                state.notify.notified().await;
            }
        })
    }

    fn begin_disposal(&self, activation: &Arc<Activation>, state: &Arc<DisposalState>) {
        activation.wake();
        let _ = activation
            .handle
            .agent
            .cancel(AgentCancelCause::Parent, CancelOptions::default());
        let idle = activation
            .handle
            .agent
            .when_idle()
            .unwrap_or_else(|error| Box::pin(async move { Err(error.into()) }));
        let children: Vec<Arc<Activation>> = {
            let owned = activation.owned_children.lock();
            let activations = self.activations.lock();
            owned
                .iter()
                .filter_map(|id| activations.get(id).cloned())
                .collect()
        };
        let child_disposals: Vec<BoxFuture<'static, anyhow::Result<()>>> =
            children.iter().map(|child| self.dispose(child)).collect();

        let manager = self.arc_self();
        let activation = Arc::clone(activation);
        let state = Arc::clone(state);
        tokio::spawn(async move {
            let result = manager
                .finish_disposal_async(&activation, idle, child_disposals)
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = state.result.set(result);
            state.notify.notify_waiters();
        });
    }

    async fn finish_disposal_async(
        &self,
        activation: &Arc<Activation>,
        idle: BoxFuture<'static, anyhow::Result<()>>,
        child_disposals: Vec<BoxFuture<'static, anyhow::Result<()>>>,
    ) -> anyhow::Result<()> {
        let child_id = activation.child_id.clone();
        let mut failures: Vec<SubagentError> = Vec::new();

        let child_results = futures::future::join_all(child_disposals).await;
        let reasons: Vec<String> = child_results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .map(|error| error_chain(error.as_ref()))
            .collect();
        if !reasons.is_empty() {
            failures.push(SubagentError::new(
                format!(
                    "subagent \"{child_id}\" child teardown failed: {}",
                    reasons.join("; ")
                ),
                "ACTIVATION_TEARDOWN_FAILED",
            ));
        }

        let phase_failure = async {
            idle.await?;
            self.flush_final_state(activation).await;
            activation
                .observer
                .capture(activation.handle.agent.as_ref());
            Ok::<(), anyhow::Error>(())
        }
        .await
        .err();
        if let Some(error) = phase_failure {
            failures.push(SubagentError::new(
                format!(
                    "subagent \"{child_id}\" activation teardown failed: {}",
                    error_chain(error.as_ref())
                ),
                "ACTIVATION_TEARDOWN_FAILED",
            ));
        }

        if let Err(error) = activation.handle.dispose().await {
            failures.push(SubagentError::new(
                format!(
                    "subagent \"{child_id}\" activation handle disposal failed: {}",
                    error_chain(error.as_ref())
                ),
                "ACTIVATION_TEARDOWN_FAILED",
            ));
        }

        let failure: Option<anyhow::Error> = match failures.len() {
            0 => None,
            1 => Some(failures.pop().expect("one failure").into()),
            _ => Some(
                SubagentError::new(
                    format!(
                        "subagent \"{child_id}\" activation teardown failed at {} boundaries: {}",
                        failures.len(),
                        failures
                            .iter()
                            .map(|error| error_chain(error))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                    "ACTIVATION_TEARDOWN_FAILED",
                )
                .into(),
            ),
        };

        self.activations.lock().remove(&child_id);
        let terminal = activation.observer.terminal(failure.as_ref());
        self.notify_settlement(activation, &terminal);
        self.release_ownership(&child_id);
        activation.observer.settle(failure.as_ref());
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn notify_settlement(&self, activation: &Arc<Activation>, terminal: &ActivationTerminal) {
        if !activation.announced.load(Ordering::Acquire) {
            return;
        }
        let result = (|| -> anyhow::Result<()> {
            let parent = self
                .agents()
                .and_then(|registry| registry.get(&activation.parent_session));
            let Some(parent) = parent else {
                return Ok(());
            };
            let summary = settlement_summary(&activation.child_id, terminal.stop_reason);
            let mut blocks = vec![ContentBlock::Text {
                text: summary.clone(),
            }];
            match &terminal.output {
                None => blocks.push(ContentBlock::Text {
                    text: "It left no closing message.".to_owned(),
                }),
                Some(output) => {
                    blocks.push(ContentBlock::Text {
                        text: "Its closing message:".to_owned(),
                    });
                    blocks.extend(output.clone());
                }
            }
            let message = UserMessage::new(blocks, settled_source(&summary, &activation.child_id));
            if self.closing_teardown_for(&parent).is_some() {
                parent.inject(message).map_err(anyhow::Error::from)?;
                return Ok(());
            }
            self.send_waking(&parent, &message, || {
                if parent.status() == AgentStatus::Idle {
                    parent
                        .followup(message.clone())
                        .map_err(anyhow::Error::from)
                } else {
                    parent.steer(message.clone()).map_err(anyhow::Error::from)
                }
            })?;
            Ok(())
        })();
        if let Err(error) = result {
            tracing::warn!(
                child = %activation.child_id,
                %error,
                "subagent settlement notice was not delivered to its parent"
            );
        }
    }

    async fn flush_final_state(&self, activation: &Arc<Activation>) {
        let child = activation.handle.agent.clone();
        let result = async {
            let sessions = child
                .context()
                .get(SESSIONS)
                .ok_or_else(|| anyhow::anyhow!("subagent child requires sessions"))?;
            sessions.flush(child.session()).await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(
                child = %activation.child_id,
                %error,
                "subagent best-effort final session flush failed; the persisted state may be unavailable or stale on resume"
            );
        }
    }

    fn require_persistence(&self) -> anyhow::Result<Arc<dyn SessionPersistence>> {
        self.context
            .get(SESSION_PERSISTENCE)
            .map(|service| service.persistence())
            .ok_or_else(|| {
                SubagentError::new(
                    "continuable subagents require session persistence (load a seekdeep-session-persistence backend)",
                    "PERSISTENCE_UNAVAILABLE",
                )
                .into()
            })
    }
}

fn unauthorized_reporter(id: &SessionId) -> anyhow::Error {
    SubagentError::new(
        format!("agent \"{id}\" is not a live continuable subagent and cannot report"),
        "UNAUTHORIZED",
    )
    .into()
}
