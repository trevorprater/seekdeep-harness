//! Shared prerequisite service mounting for `AgentLoop` tests.

use std::sync::Arc;

use seekdeep_agent::AgentRegistry;
use seekdeep_cordis::Context;
use seekdeep_core::session_store::SessionStore;
use seekdeep_llm::LlmRuntime;
use seekdeep_system_prompt::{SystemPrompt, SystemPromptConfig};
use seekdeep_tools::{ToolRuntime, ToolRuntimeConfig};

/// Configuration forwarded without mutation to prompt and tool services.
#[derive(Clone, Debug, Default)]
pub struct AgentLoopTestDependenciesOptions {
    /// System-prompt registry configuration.
    pub system_prompt: SystemPromptConfig,
    /// Tool registry configuration.
    pub tools: ToolRuntimeConfig,
}

/// Mounted prerequisite services retained for direct test composition.
#[derive(Clone)]
pub struct AgentLoopTestDependencies {
    /// Provider-neutral LLM adapter registry.
    pub llm: Arc<LlmRuntime>,
    /// Live session registry.
    pub sessions: Arc<SessionStore>,
    /// System-prompt registry.
    pub system_prompt: Arc<SystemPrompt>,
    /// Tool runtime.
    pub tools: Arc<ToolRuntime>,
    /// Live agent registry, without an installed factory.
    pub agents: Arc<AgentRegistry>,
}

impl std::fmt::Debug for AgentLoopTestDependencies {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentLoopTestDependencies")
            .finish_non_exhaustive()
    }
}

/// Mounts the standard prerequisite services without `AgentLoop` or an adapter.
///
/// Earlier services remain context-owned if a later mount fails and unwind
/// with the caller-owned context.
///
/// # Errors
///
/// Returns the first service construction, configuration, publication, or
/// ownership failure.
pub fn mount_agent_loop_test_dependencies(
    context: &Context,
    options: AgentLoopTestDependenciesOptions,
) -> anyhow::Result<AgentLoopTestDependencies> {
    let llm = LlmRuntime::install(context)?;
    let sessions = SessionStore::install(context)?;
    let system_prompt = seekdeep_system_prompt::install(context, options.system_prompt)?;
    let tools = seekdeep_tools::install(context, &system_prompt, options.tools)?;
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(context)?;
    Ok(AgentLoopTestDependencies {
        llm,
        sessions,
        system_prompt,
        tools,
        agents,
    })
}
