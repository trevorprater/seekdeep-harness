//! Default executor-less, UI-less agent spine composition.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use seekdeep_agent::{AgentOptions, AgentRegistry, CreateAgentOptions, ResumeAgentOptions};
use seekdeep_agent_loop::{
    AGENT_LOOP, AgentLoop, AgentLoopServices, DEFAULT_MAX_PARALLEL_TOOL_CALLS,
};
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_core::{session::SessionId, session_store::SessionStore};
use seekdeep_llm::{ModelId, ProviderId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Loader plugin name.
pub const NAME: &str = "agent-spine-demo";
/// The bundle constructs its own dependency spine.
pub const INJECT: &[&str] = &[];
static NEXT_CONFIGURED_AGENT: AtomicU64 = AtomicU64::new(1);

/// Example-owned deterministic fallback title limits.
pub const EXAMPLE_SESSION_TITLE_CONFIG: seekdeep_session_title::SessionTitleConfig =
    seekdeep_session_title::SessionTitleConfig {
        fallback_max_words: 5,
        fallback_max_bytes: 40,
        max_title_bytes: 80,
    };

/// Boolean disable switch or owner-native config.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OptionalFeature<T> {
    /// Must be `false`; true is rejected.
    Disabled(bool),
    /// Enabled with owner configuration.
    Config(T),
}

impl<T> OptionalFeature<T> {
    fn resolved(self, label: &str) -> anyhow::Result<Option<T>> {
        match self {
            Self::Disabled(false) => Ok(None),
            Self::Disabled(true) => {
                anyhow::bail!("agent-spine-demo: {label} accepts false or config, not true")
            }
            Self::Config(config) => Ok(Some(config)),
        }
    }
}

/// Skill registry, local provider, and model-facing consumer config.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct SkillConfig {
    /// Whether the complete skill stack is mounted.
    pub enabled: Option<bool>,
    /// Registry cache config.
    pub registry: Option<seekdeep_skill::Config>,
    /// Filesystem provider config.
    pub filesystem: Option<seekdeep_skill_filesystem::Config>,
    /// Model-facing skill config.
    pub tool: Option<seekdeep_tool_skill::Config>,
}

/// Persisted goal-domain and model-tool config.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GoalConfig {
    /// Goal domain defaults.
    pub domain: Option<seekdeep_goal::Config>,
    /// Goal tool policy.
    pub tool: Option<seekdeep_tool_goal::Config>,
}

/// One declaratively pre-created or resumed agent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfiguredAgent {
    /// Stable config label.
    pub id: String,
    /// Exact fresh session identity.
    pub session_id: Option<String>,
    /// Provider route.
    pub provider: Option<String>,
    /// Model id.
    pub model: Option<String>,
    /// Output cap.
    pub max_tokens: Option<u64>,
    /// Fresh-session workspace.
    pub cwd: Option<String>,
    /// Persisted session identity to resume.
    pub resume_session_id: Option<String>,
}

/// Complete agent-spine bundle configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Agents created or resumed at bundle startup.
    #[serde(default)]
    pub agents: Vec<ConfiguredAgent>,
    /// Parallel-safe tool-call cap.
    #[serde(default)]
    pub max_parallel_tool_calls: Option<usize>,
    /// Fixed identity opener policy.
    #[serde(default)]
    pub include_harness_identity: Option<bool>,
    /// Dynamic runtime-context policy.
    #[serde(default)]
    pub include_runtime_context: Option<bool>,
    /// Deployment persona.
    #[serde(default)]
    pub persona: Option<String>,
    /// Explicit tool order.
    #[serde(default)]
    pub tool_order: Option<Vec<String>>,
    /// Tool runtime presentation config.
    #[serde(default)]
    pub tools: Option<seekdeep_tools::ToolRuntimeConfig>,
    /// `SeekDeep` home shared by shell and skills.
    #[serde(default, alias = "dshHome")]
    pub seekdeep_home: Option<String>,
    /// Session-title limits.
    #[serde(default)]
    pub session_title: Option<seekdeep_session_title::SessionTitleConfig>,
    /// Required workspace instruction policy.
    pub workspace_context: OptionalFeature<seekdeep_agent_instructions::Config>,
    /// Optional skill stack.
    #[serde(default)]
    pub skills: Option<SkillConfig>,
    /// Bash tool or false.
    #[serde(default)]
    pub tool_bash: Option<OptionalFeature<seekdeep_tool_bash::Config>>,
    /// Job admission config.
    #[serde(default)]
    pub jobs: Option<seekdeep_jobs_local::Config>,
    /// Job tools or false.
    #[serde(default)]
    pub tool_jobs: Option<OptionalFeature<seekdeep_tool_jobs::Config>>,
    /// Invariant registry filtering.
    #[serde(default)]
    pub invariants: Option<seekdeep_invariants::InvariantConfig>,
    /// Goal stack or false.
    #[serde(default)]
    pub goals: Option<OptionalFeature<GoalConfig>>,
}

/// Live service handles retained for programmatic users and tests.
pub struct SpineRuntime {
    /// LLM routing service.
    pub llm: Arc<seekdeep_llm::LlmRuntime>,
    /// Session store.
    pub sessions: Arc<SessionStore>,
    /// Agent registry.
    pub agents: Arc<AgentRegistry>,
    /// Tool registry.
    pub tools: Arc<seekdeep_tools::ToolRuntime>,
    /// Concrete loop.
    pub agent_loop: Arc<AgentLoop>,
}

impl std::fmt::Debug for SpineRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SpineRuntime")
            .finish_non_exhaustive()
    }
}

/// Returns the bundle-owned fields while removing pre-created agents.
#[must_use]
pub fn pick_spine_config(config: &Config) -> Config {
    let mut selected = config.clone();
    selected.agents.clear();
    selected
}

/// Constructs the complete bundle and configured agents.
///
/// # Errors
///
/// Returns config, duplicate-service, plugin, registration, or agent-start failures.
#[allow(clippy::too_many_lines)]
pub async fn apply(context: &Context, config: Config) -> anyhow::Result<Arc<SpineRuntime>> {
    validate_config(&config)?;
    let max_parallel = config
        .max_parallel_tool_calls
        .unwrap_or(DEFAULT_MAX_PARALLEL_TOOL_CALLS);
    let resolved_home = resolve_skill_home(&config)?;
    seekdeep_cordis_timer::TimerService::install(
        context,
        Arc::new(seekdeep_cordis_timer::TokioTimerDriver::default()),
    )?;
    let llm = seekdeep_llm::LlmRuntime::install(context)?;
    let sessions = SessionStore::install(context)?;
    seekdeep_session_title::SessionTitleService::install(
        context,
        config.session_title.unwrap_or(EXAMPLE_SESSION_TITLE_CONFIG),
    )?;
    let system_prompt = seekdeep_system_prompt::install(
        context,
        seekdeep_system_prompt::SystemPromptConfig {
            include_harness_identity: config.include_harness_identity.unwrap_or(true),
            include_runtime_context: config.include_runtime_context.unwrap_or(true),
            persona: config.persona.clone().unwrap_or_default(),
            tool_order: config.tool_order.clone(),
        },
    )?;
    let tools = seekdeep_tools::install(context, &system_prompt, config.tools.unwrap_or_default())?;

    let skills_enabled = config
        .skills
        .as_ref()
        .and_then(|skills| skills.enabled)
        .unwrap_or(true);
    if skills_enabled {
        let skills = config.skills.clone().unwrap_or_default();
        seekdeep_skill::SkillRegistry::install(context, &skills.registry.unwrap_or_default())?;
        let mut filesystem = skills.filesystem.unwrap_or_default();
        filesystem.seekdeep_home = Some(resolved_home.clone());
        seekdeep_skill_filesystem::install(context, filesystem)?;
    }

    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(context)?;
    let retry = seekdeep_llm_retry::install(context, seekdeep_llm_retry::RetryConfig::default())?;
    retry.await_settled().await?;

    if let Some(goals) = config.goals.clone()
        && let Some(goals) = goals.resolved("goals")?
    {
        seekdeep_goal::GoalService::install(context, goals.domain.unwrap_or_default())?;
        seekdeep_tool_goal::apply(context, &goals.tool.unwrap_or_default())?;
        seekdeep_goal_round_driver::apply(context)?;
    }
    seekdeep_jobs_local::LocalJobRegistry::new(context, config.jobs.unwrap_or_default())?;
    let invariants = seekdeep_invariants::InvariantRegistry::install(
        context,
        &config.invariants.unwrap_or_default(),
    )?;
    seekdeep_agent::register_invariant(&invariants)?
        .await_ready()
        .await?;
    seekdeep_scope::invariant::register_invariant(&invariants)?
        .await_ready()
        .await?;
    let invariant_sessions = Arc::clone(&sessions);
    invariants
        .register(
            "@seekdeep-ai/seekdeep-session",
            seekdeep_invariants::InvariantInstaller::new(
                std::iter::empty::<String>(),
                move |context, _| {
                    let sessions = Arc::clone(&invariant_sessions);
                    async move {
                        seekdeep_core::invariant::install_session_invariants(&context, &sessions)?;
                        Ok(())
                    }
                },
            ),
        )?
        .await_ready()
        .await?;

    let tool_bash = match config.tool_bash.clone() {
        Some(feature) => feature.resolved("toolBash")?,
        None => Some(seekdeep_tool_bash::Config::default()),
    };
    if let Some(tool_bash) = tool_bash {
        let shell_env = seekdeep_shell_env::ShellEnvConfig {
            seekdeep_home: Some(resolved_home.to_string_lossy().into_owned()),
        };
        seekdeep_shell_env::apply(context, &shell_env)?;
        context.plugin(
            seekdeep_tool_bash::plugin(),
            serde_json::to_value(tool_bash)?,
        )?;
    }
    if let Some(workspace) = config
        .workspace_context
        .clone()
        .resolved("workspaceContext")?
    {
        seekdeep_agent_instructions::apply(context, &workspace)?;
    }
    if skills_enabled {
        let tool = config
            .skills
            .as_ref()
            .and_then(|skills| skills.tool.clone())
            .unwrap_or_default();
        seekdeep_tool_skill::apply(context, &tool)?;
    }
    let tool_jobs = match config.tool_jobs.clone() {
        Some(feature) => feature.resolved("toolJobs")?,
        None => Some(seekdeep_tool_jobs::Config::default()),
    };
    if let Some(tool_jobs) = tool_jobs {
        seekdeep_tool_jobs::apply(context, &tool_jobs)?;
    }

    let agent_loop = Arc::new(AgentLoop::new(
        context.clone(),
        Arc::clone(&sessions),
        (*agents).clone(),
        AgentLoopServices {
            llm: Arc::clone(&llm),
            system_prompt,
            tools: Arc::clone(&tools),
            max_parallel_tool_calls: max_parallel,
        },
    )?);
    context.provide(AGENT_LOOP, Arc::clone(&agent_loop))?;
    agents.set_factory(agent_loop.clone())?;
    let invariant_llm = Arc::clone(&llm);
    let invariant_sessions = Arc::clone(&sessions);
    invariants
        .register(
            "@seekdeep-ai/seekdeep-agent-loop",
            seekdeep_invariants::InvariantInstaller::new(
                std::iter::empty::<String>(),
                move |context, _| {
                    let llm = Arc::clone(&invariant_llm);
                    let sessions = Arc::clone(&invariant_sessions);
                    async move {
                        seekdeep_agent_loop::install_request_invariant(&context, &llm, sessions)?;
                        Ok(())
                    }
                },
            ),
        )?
        .await_ready()
        .await?;
    let cleanup = Arc::clone(&agent_loop);
    context.own(EffectHandle::new(
        "agent-spine-demo agent loop",
        move || Box::pin(async move { cleanup.dispose().await }),
    ))?;
    create_configured_agents(&agents, &config.agents).await?;

    Ok(Arc::new(SpineRuntime {
        llm,
        sessions,
        agents,
        tools,
        agent_loop,
    }))
}

fn validate_config(config: &Config) -> anyhow::Result<()> {
    anyhow::ensure!(
        config.max_parallel_tool_calls.is_none_or(|value| value > 0),
        "maxParallelToolCalls must be a positive integer"
    );
    validate_optional_feature(&config.workspace_context, "workspaceContext")?;
    if let Some(feature) = &config.tool_bash {
        validate_optional_feature(feature, "toolBash")?;
    }
    if let Some(feature) = &config.tool_jobs {
        validate_optional_feature(feature, "toolJobs")?;
    }
    if let Some(feature) = &config.goals {
        validate_optional_feature(feature, "goals")?;
    }
    let mut identities = HashSet::new();
    for agent in &config.agents {
        anyhow::ensure!(!agent.id.is_empty(), "configured agent id is required");
        anyhow::ensure!(
            agent.session_id.is_none() || agent.resume_session_id.is_none(),
            "agent {:?}: sessionId and resumeSessionId are mutually exclusive",
            agent.id
        );
        if let Some(identity) = agent
            .resume_session_id
            .as_ref()
            .or(agent.session_id.as_ref())
        {
            anyhow::ensure!(
                identities.insert(identity.clone()),
                "duplicate exact session identity {identity:?}"
            );
        }
    }
    Ok(())
}

fn validate_optional_feature<T>(feature: &OptionalFeature<T>, label: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !matches!(feature, OptionalFeature::Disabled(true)),
        "agent-spine-demo: {label} accepts false or config, not true"
    );
    Ok(())
}

fn resolve_skill_home(config: &Config) -> anyhow::Result<PathBuf> {
    let nested = config
        .skills
        .as_ref()
        .and_then(|skills| skills.filesystem.as_ref())
        .and_then(|filesystem| filesystem.seekdeep_home.as_deref());
    let explicit = config.seekdeep_home.as_deref().map(std::ffi::OsStr::new);
    let nested = nested.map(PathBuf::from);
    let resolved = seekdeep_util::home_paths::resolve_process_seekdeep_home(
        explicit.or_else(|| nested.as_deref().map(Path::as_os_str)),
    )?;
    if let (Some(explicit), Some(nested)) = (explicit, nested.as_deref()) {
        let explicit = seekdeep_util::home_paths::resolve_process_seekdeep_home(Some(explicit))?;
        let nested =
            seekdeep_util::home_paths::resolve_process_seekdeep_home(Some(nested.as_os_str()))?;
        anyhow::ensure!(
            explicit == nested,
            "agent-spine-demo: seekdeepHome and skills.filesystem.seekdeepHome must resolve to the same directory"
        );
    }
    Ok(resolved)
}

async fn create_configured_agents(
    agents: &Arc<AgentRegistry>,
    configured: &[ConfiguredAgent],
) -> anyhow::Result<()> {
    for configured in configured {
        let options = AgentOptions {
            provider: configured.provider.clone().map(ProviderId::new),
            model: configured.model.clone().map(ModelId::new),
            max_tokens: configured.max_tokens,
            subagent_depth: None,
        };
        if let Some(resume) = &configured.resume_session_id {
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
            request.meta.cwd = configured.cwd.clone();
            agents.create(request).await?;
        }
    }
    Ok(())
}

fn normalize_config(value: &Value) -> anyhow::Result<Value> {
    let config = serde_json::from_value::<Config>(value.clone())?;
    validate_config(&config)?;
    Ok(serde_json::to_value(config)?)
}

/// Builds the Loader-compatible bundle plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            apply(&context, serde_json::from_value(config)?).await?;
            Ok(())
        })
    })
    .with_config_validator(normalize_config)
}
