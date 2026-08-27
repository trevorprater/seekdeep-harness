//! Deterministic agent driver, runtime-context projection, and tool scheduling.

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use seekdeep_agent::{AgentOptions, AgentRegistry};
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_core::{session::SessionId, session_store::SessionStore};
use seekdeep_llm::{LLM, ModelId, ProviderId};
use seekdeep_session_persistence::SESSION_PERSISTENCE;
use seekdeep_system_prompt::SYSTEM_PROMPT;
use seekdeep_tools::TOOLS;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Default maximum number of parallel-safe calls dispatched per agent step.
pub const DEFAULT_MAX_PARALLEL_TOOL_CALLS: usize = 10;
/// Loader plugin identity.
pub const PLUGIN_NAME: &str = "agent-loop";
/// Services required by the concrete factory.
pub const PLUGIN_INJECT: &[&str] = &["agents", "sessions", "llm", "tools", "systemPrompt"];
static NEXT_CONFIGURED_AGENT: AtomicU64 = AtomicU64::new(1);

/// One declaratively created or resumed agent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfiguredAgent {
    /// Stable configuration label used for diagnostics and fallback identity.
    pub id: String,
    /// Exact fresh session identity.
    pub session_id: Option<String>,
    /// Provider route.
    pub provider: Option<String>,
    /// Model identity.
    pub model: Option<String>,
    /// Optional output-token cap.
    pub max_tokens: Option<u64>,
    /// Fresh-session workspace.
    pub cwd: Option<String>,
    /// Existing persisted session to resume.
    pub resume_session_id: Option<String>,
}

/// Agent-loop Loader configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Maximum parallel-safe tool calls in flight per step.
    pub max_parallel_tool_calls: Option<usize>,
    /// Agents created or resumed during plugin startup.
    pub agents: Vec<ConfiguredAgent>,
}

/// Durable projection of dynamic runtime context.
pub mod controller;
/// Durable turn/step machine and request reconstruction.
pub mod driver;
/// Rollback-covered agent/session factory publication.
pub mod factory;
/// Dispatch-time reconstruction guard for loop-built model calls.
pub mod invariant;
/// Durable projection of dynamic runtime context.
pub mod runtime_context;
/// Ordered, bounded scheduling for one assistant step's tool calls.
pub mod tool_calls;

pub use controller::{AgentInboxClaimed, AgentInboxMessage, DriverTask, LoopAgent, LoopController};
pub use driver::{
    AgentErrorEvent, AgentLoopServices, AgentPreStepEvent, AgentRequestErrorEvent,
    AgentRequestEvent, AgentTurnStoppingEvent, DefaultAgentDriver,
};
pub use factory::{AgentLoop, SessionStartEvent};
pub use invariant::{install_request_invariant, validate_agent_loop_request};
pub use runtime_context::RuntimeContextProjection;
pub use seekdeep_agent::AgentStatusChanged;
pub use seekdeep_agent::factory::{
    AgentFactory, AgentHandle, AgentSetup, AgentSetupCommit, CreateAgentMeta, CreateAgentOptions,
    ResumeAgentOptions,
};
pub use tool_calls::{ToolCall, ToolCallBatch, ToolCallBatchOutcome, execute_tool_calls};

/// Typed Cordis slot for the concrete agent-loop factory.
pub const AGENT_LOOP: seekdeep_cordis::ServiceKey<AgentLoop> =
    seekdeep_cordis::ServiceKey::new("agentLoop");

fn validate_config(config: &Config) -> anyhow::Result<()> {
    anyhow::ensure!(
        config.max_parallel_tool_calls.is_none_or(|value| value > 0),
        "maxParallelToolCalls must be a positive integer"
    );
    let mut identities = HashSet::new();
    for configured in &config.agents {
        anyhow::ensure!(!configured.id.is_empty(), "configured agent id is required");
        let resume = configured
            .resume_session_id
            .as_deref()
            .filter(|identity| !identity.is_empty());
        anyhow::ensure!(
            configured.session_id.is_none() || resume.is_none(),
            "agent {:?}: sessionId and resumeSessionId are mutually exclusive",
            configured.id
        );
        if let Some(identity) = resume.or(configured.session_id.as_deref()) {
            anyhow::ensure!(
                identities.insert(identity.to_owned()),
                "configured agents use duplicate exact session identity {identity:?}"
            );
        }
        anyhow::ensure!(
            configured
                .max_tokens
                .is_none_or(|value| value > 0 && value <= 9_007_199_254_740_991),
            "agent maxTokens must be a positive safe integer"
        );
    }
    Ok(())
}

async fn create_configured_agents(
    agents: &Arc<AgentRegistry>,
    configured_agents: &[ConfiguredAgent],
) -> anyhow::Result<()> {
    for configured in configured_agents {
        let options = AgentOptions {
            provider: configured.provider.clone().map(ProviderId::new),
            model: configured.model.clone().map(ModelId::new),
            max_tokens: configured.max_tokens,
            subagent_depth: None,
        };
        if let Some(resume) = configured
            .resume_session_id
            .as_deref()
            .filter(|identity| !identity.is_empty())
        {
            let mut request = ResumeAgentOptions::new(SessionId::new(resume));
            request.agent_options = options;
            agents.resume(request).await?;
        } else {
            let id = configured.session_id.clone().unwrap_or_else(|| {
                format!(
                    "{}-session-{:016x}",
                    configured.id,
                    NEXT_CONFIGURED_AGENT.fetch_add(1, Ordering::AcqRel)
                )
            });
            let mut request = CreateAgentOptions::new(SessionId::new(id));
            request.agent_options = options;
            request.meta.cwd.clone_from(&configured.cwd);
            agents.create(request).await?;
        }
    }
    Ok(())
}

/// Mounts the concrete Agent factory over the already-published service spine.
///
/// # Errors
///
/// Returns configuration, missing-service, factory, startup-agent, or teardown-ownership failures.
pub async fn apply(context: &Context, config: Config) -> anyhow::Result<Arc<AgentLoop>> {
    validate_config(&config)?;
    let agents = context
        .get(seekdeep_agent::AGENTS)
        .ok_or_else(|| anyhow::anyhow!("agent-loop requires agents"))?;
    let sessions: Arc<SessionStore> = context
        .get(seekdeep_core::session_store::SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("agent-loop requires sessions"))?;
    let llm = context
        .get(LLM)
        .ok_or_else(|| anyhow::anyhow!("agent-loop requires llm"))?;
    let tools = context
        .get(TOOLS)
        .ok_or_else(|| anyhow::anyhow!("agent-loop requires tools"))?;
    let system_prompt = context
        .get(SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("agent-loop requires systemPrompt"))?;
    let agent_loop = Arc::new(AgentLoop::new(
        context.clone(),
        sessions,
        (*agents).clone(),
        AgentLoopServices {
            llm,
            system_prompt,
            tools,
            max_parallel_tool_calls: config
                .max_parallel_tool_calls
                .unwrap_or(DEFAULT_MAX_PARALLEL_TOOL_CALLS),
        },
    )?);
    if let Some(persistence) = context.get(SESSION_PERSISTENCE) {
        agent_loop.set_persistence(persistence.persistence())?;
    }
    context.provide(AGENT_LOOP, agent_loop.clone())?;
    agents.register_factory(context, agent_loop.clone())?;
    let cleanup = agent_loop.clone();
    context.own(EffectHandle::new("agent loop", move || {
        let cleanup = cleanup.clone();
        Box::pin(async move { cleanup.dispose().await })
    }))?;
    create_configured_agents(&agents, &config.agents).await?;
    Ok(agent_loop)
}

fn normalize_config(value: &Value) -> anyhow::Result<Value> {
    let config = serde_json::from_value::<Config>(value.clone())?;
    validate_config(&config)?;
    Ok(serde_json::to_value(config)?)
}

/// Builds the Loader-compatible concrete Agent Loop plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(
        PLUGIN_NAME,
        PLUGIN_INJECT.iter().copied(),
        |context, config| {
            Box::pin(async move {
                apply(&context, serde_json::from_value(config)?).await?;
                Ok(())
            })
        },
    )
    .with_config_validator(normalize_config)
}
