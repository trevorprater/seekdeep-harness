//! Programmatic agent-creation factory: the request/handle types and the
//! registry-facing contract.

use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use seekdeep_cordis::Context;
use seekdeep_core::session::{SessionEvent, SessionId, SessionOrigin};
use seekdeep_llm::AbortSignal;

use crate::{Agent, AgentOptions};

/// Synchronous validation/commit at the exact publication boundary.
pub trait AgentSetupCommit: Send + Sync + 'static {
    /// Validates and commits prepared setup.
    ///
    /// # Errors
    ///
    /// Rejects publication and triggers complete rollback.
    fn commit(&self) -> anyhow::Result<()>;
}

/// Trusted unpublished agent-scope composition callback.
pub type AgentSetup = Arc<
    dyn Fn(Context) -> BoxFuture<'static, anyhow::Result<Option<Arc<dyn AgentSetupCommit>>>>
        + Send
        + Sync
        + 'static,
>;

/// Durable session metadata accepted by programmatic creation.
#[derive(Clone, Debug, Default)]
pub struct CreateAgentMeta {
    /// Validated absolute working directory.
    pub cwd: Option<String>,
    /// Durable fork lineage.
    pub parent_session: Option<SessionId>,
    /// Inherited prefix boundary.
    pub seed_length: Option<u64>,
    /// Coarse subagent classification.
    pub origin: Option<SessionOrigin>,
    /// Persisted recursion depth.
    pub delegation_depth: Option<u64>,
    /// Agent preset that composed this session.
    pub agent_preset: Option<String>,
}

/// Programmatic create transaction input.
#[derive(Clone)]
pub struct CreateAgentOptions {
    /// Shared live agent/session identity.
    pub session_id: SessionId,
    /// Durable session metadata.
    pub meta: CreateAgentMeta,
    /// Initial replay/fork history.
    pub seed: Option<Vec<SessionEvent>>,
    /// Per-agent model options.
    pub agent_options: AgentOptions,
    /// Optional creation-only cancellation.
    pub signal: Option<AbortSignal>,
    /// Optional unpublished scoped composition.
    pub setup: Option<AgentSetup>,
    /// Runtime owner agent, independent of durable lineage.
    pub owner_agent: Option<Arc<Agent>>,
}

impl CreateAgentOptions {
    /// Builds the mandatory exact-identity portion with default composition.
    #[must_use]
    pub fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            meta: CreateAgentMeta::default(),
            seed: None,
            agent_options: AgentOptions::default(),
            signal: None,
            setup: None,
            owner_agent: None,
        }
    }
}

impl std::fmt::Debug for CreateAgentOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CreateAgentOptions")
            .field("session_id", &self.session_id)
            .field("meta", &self.meta)
            .field("seed_length", &self.seed.as_ref().map(Vec::len))
            .field("agent_options", &self.agent_options)
            .field("signal", &self.signal)
            .field("setup", &self.setup.is_some())
            .field(
                "owner_agent",
                &self.owner_agent.as_ref().map(|agent| agent.id()),
            )
            .finish()
    }
}

/// Programmatic resume transaction input.
#[derive(Clone)]
pub struct ResumeAgentOptions {
    /// Persisted identity to load and publish.
    pub resume_session_id: SessionId,
    /// Per-agent model options.
    pub agent_options: AgentOptions,
    /// Optional creation-only cancellation.
    pub signal: Option<AbortSignal>,
    /// Optional unpublished scoped composition.
    pub setup: Option<AgentSetup>,
    /// Runtime owner agent, independent of durable lineage.
    pub owner_agent: Option<Arc<Agent>>,
}

impl ResumeAgentOptions {
    /// Builds the mandatory persisted-identity portion.
    #[must_use]
    pub fn new(resume_session_id: SessionId) -> Self {
        Self {
            resume_session_id,
            agent_options: AgentOptions::default(),
            signal: None,
            setup: None,
            owner_agent: None,
        }
    }
}

impl std::fmt::Debug for ResumeAgentOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResumeAgentOptions")
            .field("resume_session_id", &self.resume_session_id)
            .field("agent_options", &self.agent_options)
            .field("signal", &self.signal)
            .field("setup", &self.setup.is_some())
            .field(
                "owner_agent",
                &self.owner_agent.as_ref().map(|agent| agent.id()),
            )
            .finish()
    }
}

/// An owned agent plus its disposer, returned by create/resume.
pub struct AgentHandle {
    /// Public exact live agent.
    pub agent: Arc<Agent>,
    dispose: Box<dyn Fn() -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>,
}

impl AgentHandle {
    /// Builds a handle from its agent and a disposal closure.
    #[must_use]
    pub fn new(
        agent: Arc<Agent>,
        dispose: Box<dyn Fn() -> BoxFuture<'static, anyhow::Result<()>> + Send + Sync>,
    ) -> Self {
        Self { agent, dispose }
    }

    /// Stops/drains, unregisters, detaches the session, and unwinds scope.
    ///
    /// # Errors
    ///
    /// Returns the shared aggregate teardown failure.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        (self.dispose)().await
    }
}

impl std::fmt::Debug for AgentHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentHandle")
            .field("agent", &self.agent.id())
            .finish_non_exhaustive()
    }
}

/// The agent-creation factory the loop implementation provides to the registry.
#[async_trait]
pub trait AgentFactory: Send + Sync {
    /// Creates a new agent on a caller-supplied session id.
    ///
    /// # Errors
    ///
    /// Returns preparation, setup, cancellation, collision, or publication
    /// failures.
    async fn create_agent(
        &self,
        owner_ctx: &Context,
        options: CreateAgentOptions,
    ) -> anyhow::Result<AgentHandle>;

    /// Loads, composes, and publishes one persisted session lifecycle.
    ///
    /// # Errors
    ///
    /// Returns missing-backend, load, cancellation, setup, collision, or
    /// publication failures.
    async fn resume(
        &self,
        owner_ctx: &Context,
        options: ResumeAgentOptions,
    ) -> anyhow::Result<AgentHandle>;
}
