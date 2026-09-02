//! ACP automation server app over the default agent spine and durable stores.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use seekdeep_acp::{ACP_BRIDGE, AcpBridge, AcpRuntime};
use seekdeep_agent_spine_demo::{GoalConfig, OptionalFeature, SkillConfig};
use seekdeep_app_boot::{BootOptions, boot, load_layered_env, resolve_config_path};
use seekdeep_cordis::{Context, Plugin};
use seekdeep_loader::{ExpressionEnvironment, PluginCatalog};
use serde::{Deserialize, Serialize};
use serde_json::Value;

mod snapshot_fixtures;

/// Loader and binary name.
pub const NAME: &str = "acp-demo";
/// App composition has no parent dependency.
pub const INJECT: &[&str] = &[];
const DEFAULT_PERSISTENCE_ROOT: &str = "./.sessions";

/// Complete ACP application configuration.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Provider for ACP-created agents.
    pub provider: String,
    /// Model for ACP-created agents.
    pub model: String,
    /// Agent-loop concurrency cap.
    #[serde(default)]
    pub max_parallel_tool_calls: Option<usize>,
    /// Deployment persona.
    #[serde(default)]
    pub persona: Option<String>,
    /// Explicit tool order.
    #[serde(default)]
    pub tool_order: Option<Vec<String>>,
    /// Tool presentation config.
    #[serde(default)]
    pub tools: Option<seekdeep_tools::ToolRuntimeConfig>,
    /// Shared `SeekDeep` home.
    #[serde(default, alias = "dshHome")]
    pub seekdeep_home: Option<String>,
    /// Fallback title limits.
    #[serde(default)]
    pub session_title: Option<seekdeep_session_title::SessionTitleConfig>,
    /// Persistence directory.
    #[serde(default = "default_persistence_root")]
    pub persistence_root: String,
    /// Packed chunk rows.
    #[serde(default)]
    pub pack_chunks: Option<bool>,
    /// Physical JSONL encoding.
    #[serde(default)]
    pub persistence_compression: seekdeep_session_persistence_jsonl::JsonlCompression,
    /// Required workspace instructions policy.
    pub workspace_context: OptionalFeature<seekdeep_agent_instructions::Config>,
    /// Optional skill stack config.
    #[serde(default)]
    pub skills: Option<SkillConfig>,
    /// Bash tool config or false.
    #[serde(default)]
    pub tool_bash: Option<OptionalFeature<seekdeep_tool_bash::Config>>,
    /// Job admission config.
    #[serde(default)]
    pub jobs: Option<seekdeep_jobs_local::Config>,
    /// Job tools config or false.
    #[serde(default)]
    pub tool_jobs: Option<OptionalFeature<seekdeep_tool_jobs::Config>>,
    /// Goal stack config or false; omission enables owner defaults.
    #[serde(default)]
    pub goals: Option<OptionalFeature<GoalConfig>>,
}

fn default_persistence_root() -> String {
    DEFAULT_PERSISTENCE_ROOT.to_owned()
}

/// Live assembled app handles.
pub struct AcpDemoRuntime {
    /// Default agent spine.
    pub spine: Arc<seekdeep_agent_spine_demo::SpineRuntime>,
    /// ACP bridge.
    pub bridge: Arc<AcpBridge>,
}

impl std::fmt::Debug for AcpDemoRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcpDemoRuntime")
            .finish_non_exhaustive()
    }
}

/// Assembles the app over an optional explicit ACP transport.
///
/// # Errors
///
/// Returns config, spine, persistence, checkpoint, query, transport, or lifecycle failures.
pub async fn apply_with_runtime(
    context: &Context,
    config: Config,
    runtime: Option<AcpRuntime>,
) -> anyhow::Result<Arc<AcpDemoRuntime>> {
    apply_with_runtime_mode(context, config, runtime, true).await
}

async fn apply_with_runtime_mode(
    context: &Context,
    config: Config,
    runtime: Option<AcpRuntime>,
    start_bridge: bool,
) -> anyhow::Result<Arc<AcpDemoRuntime>> {
    validate_config(&config)?;
    let goals = config
        .goals
        .clone()
        .or_else(|| Some(OptionalFeature::Config(GoalConfig::default())));
    let spine = seekdeep_agent_spine_demo::apply(
        context,
        seekdeep_agent_spine_demo::Config {
            agents: Vec::new(),
            max_parallel_tool_calls: config.max_parallel_tool_calls,
            include_harness_identity: None,
            include_runtime_context: None,
            persona: config.persona,
            tool_order: config.tool_order,
            tools: config.tools,
            seekdeep_home: config.seekdeep_home,
            session_title: config.session_title,
            workspace_context: config.workspace_context,
            skills: config.skills,
            tool_bash: config.tool_bash,
            jobs: config.jobs,
            tool_jobs: config.tool_jobs,
            invariants: None,
            goals,
        },
    )
    .await?;

    let mut persistence = seekdeep_session_persistence_jsonl::JsonlConfig::new(PathBuf::from(
        &config.persistence_root,
    ));
    if let Some(pack_chunks) = config.pack_chunks {
        persistence.pack_chunks = pack_chunks;
    }
    persistence.compression = config.persistence_compression;
    let persistence_backend =
        seekdeep_session_persistence_jsonl::JsonlSessionPersistence::new_in_context(
            context,
            Arc::clone(&spine.sessions),
            persistence,
        )?;
    let erased_persistence: Arc<dyn seekdeep_session_persistence::SessionPersistence> =
        persistence_backend.clone();
    seekdeep_session_persistence::SessionPersistenceService::new(erased_persistence.clone())
        .provide(context)?;
    spine.agent_loop.set_persistence(erased_persistence)?;
    seekdeep_session_checkpoint_policy::install(context, &spine.llm, &spine.sessions, &spine.tools)
        .await?;
    let query = seekdeep_session_query_sqlite::SqliteSessionQueryConfig {
        path: Path::new(&config.persistence_root)
            .join("session-query.db")
            .to_string_lossy()
            .into_owned(),
        ..seekdeep_session_query_sqlite::SqliteSessionQueryConfig::default()
    };
    let query_engine =
        seekdeep_session_query_sqlite::SqliteSessionQueryEngine::new(context, query)?;
    let erased_query: Arc<dyn seekdeep_session_query::SessionQueryEngine> = query_engine.clone();
    seekdeep_session_query::SessionQueryService::new(erased_query).provide(context)?;
    let closing_query = query_engine;
    context.own(seekdeep_cordis::fiber::EffectHandle::new(
        "acp-demo session query",
        move || {
            Box::pin(async move {
                closing_query.close().await;
                Ok(())
            })
        },
    ))?;
    let bridge_config = seekdeep_acp::Config {
        provider: Some(config.provider),
        model: Some(config.model),
    };
    let runtime = runtime.unwrap_or_else(|| AcpRuntime {
        input: Box::pin(tokio::io::stdin()),
        output: Box::pin(tokio::io::stdout()),
    });
    let bridge = if start_bridge {
        seekdeep_acp::apply_with_runtime_and_agents(
            context,
            bridge_config,
            runtime,
            Arc::clone(&spine.agents),
        )?
    } else {
        seekdeep_acp::apply_with_runtime_and_agents_deferred(
            context,
            bridge_config,
            runtime,
            Arc::clone(&spine.agents),
        )?
    };
    Ok(Arc::new(AcpDemoRuntime { spine, bridge }))
}

/// Assembles the production stdio app.
///
/// # Errors
///
/// Returns the same failures as [`apply_with_runtime`].
pub async fn apply(context: &Context, config: Config) -> anyhow::Result<Arc<AcpDemoRuntime>> {
    apply_with_runtime(context, config, None).await
}

fn validate_config(config: &Config) -> anyhow::Result<()> {
    anyhow::ensure!(
        !config.provider.is_empty(),
        "acp-demo: provider is required"
    );
    anyhow::ensure!(!config.model.is_empty(), "acp-demo: model is required");
    anyhow::ensure!(
        config.max_parallel_tool_calls.is_none_or(|value| value > 0),
        "acp-demo: maxParallelToolCalls must be positive"
    );
    Ok(())
}

fn normalize_config(value: &Value) -> anyhow::Result<Value> {
    let config = serde_json::from_value::<Config>(value.clone())?;
    validate_config(&config)?;
    Ok(serde_json::to_value(config)?)
}

/// Builds the Loader-compatible app plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            apply_with_runtime_mode(
                &context,
                serde_json::from_value(config)?,
                Some(AcpRuntime {
                    input: Box::pin(tokio::io::stdin()),
                    output: Box::pin(tokio::io::stdout()),
                }),
                false,
            )
            .await?;
            Ok(())
        })
    })
    .with_config_validator(normalize_config)
}

/// Runs the ACP demo binary.
pub fn process_main() -> std::process::ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("seekdeep-acp-demo: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match runtime.block_on(process_main_async()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn process_main_async() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let requested = parse_config_argument(&arguments)?;
    let snapshot = std::env::var("SEEKDEEP_SNAPSHOT").ok();
    let path = resolve_config_path(
        Path::new(requested.as_deref().unwrap_or("./cordis.yml")),
        snapshot.as_deref(),
        &cwd,
    )?;
    anyhow::ensure!(
        path.exists(),
        "seekdeep-acp-demo: config file not found: {}",
        path.display()
    );
    let inherited = std::env::vars().collect::<std::collections::BTreeMap<_, _>>();
    let mut catalog = PluginCatalog::new();
    if snapshot.as_deref() != Some("replay") {
        let environment = load_layered_env(NAME, &cwd, &inherited, None)?;
        let home = seekdeep_util::home_paths::resolve_process_seekdeep_home(
            environment
                .get(seekdeep_util::home_paths::SEEKDEEP_HOME_ENV)
                .map(|entry| entry.value)
                .as_deref()
                .map(std::ffi::OsStr::new),
        )?;
        catalog =
            catalog.with_expression_environment(ExpressionEnvironment::from_launch_environment(
                &environment,
                cwd.clone(),
                std::env::current_exe()?,
                std::env::consts::OS,
                env!("CARGO_PKG_VERSION"),
                home,
            ));
    }
    register_compiled_plugins(&catalog)?;
    let application = boot(NAME, &path, &catalog, BootOptions::default()).await?;
    let bridge = application
        .context()
        .get(ACP_BRIDGE)
        .ok_or_else(|| anyhow::anyhow!("seekdeep-acp-demo: config did not mount the ACP app"))?;
    bridge.start();
    bridge.connection_closed_signal().cancelled().await;
    application.dispose().await
}

/// Registers the complete compiled plugin set used by the shipped ACP compositions.
///
/// # Errors
///
/// Returns invalid or duplicate catalog-name failures.
pub fn register_compiled_plugins(catalog: &PluginCatalog) -> anyhow::Result<()> {
    for (name, plugin) in [
        ("seekdeep-acp-demo", plugin()),
        ("seekdeep-bash-sandbox", seekdeep_bash_sandbox::plugin()),
        (
            "seekdeep-compaction-basic",
            seekdeep_compaction_basic::index::plugin(),
        ),
        (
            "seekdeep-fs-observation-policy",
            seekdeep_fs_observation_policy::plugin(),
        ),
        ("seekdeep-fs-sandbox", seekdeep_fs_sandbox::plugin()),
        (
            "seekdeep-hooks-claude-code",
            seekdeep_hooks_claude_code::plugin(),
        ),
        ("seekdeep-hooks-codex", seekdeep_hooks_codex::plugin()),
        ("seekdeep-llm-deepseek", seekdeep_llm_deepseek::plugin()),
        ("seekdeep-llm-replay", seekdeep_llm_replay::plugin()),
        (
            "seekdeep-repeat-tool-reminder",
            seekdeep_repeat_tool_reminder::plugin(),
        ),
        ("seekdeep-sandbox-local", seekdeep_sandbox_local::plugin()),
        ("seekdeep-sandbox-policy", seekdeep_sandbox_policy::plugin()),
        (
            "seekdeep-session-projection",
            seekdeep_session_projection::plugin(),
        ),
        ("seekdeep-subagent", seekdeep_subagent::index::plugin()),
        (
            "seekdeep-subagent-fork-in-process",
            seekdeep_subagent_fork_in_process::plugin(),
        ),
        (
            "seekdeep-subagent-spawn-in-process",
            seekdeep_subagent_spawn_in_process::plugin(),
        ),
        (
            "seekdeep-subprocess-local",
            seekdeep_subprocess_local::plugin(),
        ),
        (
            "seekdeep-token-meter",
            seekdeep_token_meter::runtime::plugin(),
        ),
        ("seekdeep-tool-fs", seekdeep_tool_fs::index::plugin()),
        ("seekdeep-tool-ralph", seekdeep_tool_ralph::plugin()),
        ("seekdeep-tool-subagent", seekdeep_tool_subagent::plugin()),
        (
            "seekdeep-tool-subagent-control",
            seekdeep_tool_subagent_control::plugin(),
        ),
        (
            "seekdeep-tool-subagent-report",
            seekdeep_tool_subagent_report::plugin(),
        ),
        ("seekdeep-tool-todo", seekdeep_tool_todo::plugin()),
        ("seekdeep-tool-workflow", seekdeep_tool_workflow::plugin()),
        ("seekdeep-user-approval", seekdeep_user_approval::plugin()),
        (
            "seekdeep-workflow-worker-thread",
            seekdeep_workflow_worker_thread::index::plugin(),
        ),
    ] {
        register_plugin_pair(catalog, name, plugin)?;
    }
    register_plugin_pair(
        catalog,
        "seekdeep-tool-subagent-control/list-agents",
        seekdeep_tool_subagent_control::list_plugin(),
    )?;
    catalog.register_named(
        "./tests/fixtures/subagent-settlement-marker.ts",
        snapshot_fixtures::subagent_settlement_marker_plugin(),
    )?;
    Ok(())
}

fn register_plugin_pair(catalog: &PluginCatalog, name: &str, plugin: Plugin) -> anyhow::Result<()> {
    catalog.register_named(name, plugin.clone())?;
    catalog.register_named(&format!("@seekdeep-ai/{name}"), plugin)?;
    Ok(())
}

fn parse_config_argument(arguments: &[String]) -> anyhow::Result<Option<String>> {
    let mut selected = None;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--config" | "-c" => {
                index += 1;
                let value = arguments.get(index).ok_or_else(|| {
                    anyhow::anyhow!("seekdeep-acp-demo: --config requires a path")
                })?;
                selected = Some(value.clone());
            }
            argument => anyhow::bail!("seekdeep-acp-demo: unknown argument {argument:?}"),
        }
        index += 1;
    }
    Ok(selected)
}
