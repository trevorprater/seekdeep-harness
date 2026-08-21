//! Native application assembly for the `seekdeep` executable.
//!
//! This module is the first directly executable headless vertical slice. It
//! deliberately assembles typed Rust services while the source-compatible
//! profile/include loader is still being ported; it is not a substitute for
//! the shipped base-plus-headless profile composition.

pub mod args;
pub mod layered_env;
pub mod process_shutdown;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use seekdeep_agent::{AgentFactoryRegistration, AgentRegistry};
use seekdeep_agent_default_model::{
    AGENT_DEFAULT_MODEL, AgentDefaultModelConfig, install as install_default_model,
};
use seekdeep_agent_loop::{
    AgentLoop, AgentLoopServices, DEFAULT_MAX_PARALLEL_TOOL_CALLS, install_request_invariant,
};
use seekdeep_code_runtime_worker_thread::WorkerThreadCodeRuntimeConfig;
use seekdeep_cordis::Context;
use seekdeep_core::session_store::SessionStore;
use seekdeep_credentials_local::{LocalCredentialConfig, install as install_credentials};
use seekdeep_headless::{HeadlessRunResult, HeadlessRunner};
use seekdeep_llm::{LlmRuntime, ModelId, ProviderId};
use seekdeep_llm_deepseek::{DeepSeekConfig, install as install_deepseek};
use seekdeep_llm_retry::{RetryConfig, install as install_llm_retry};
use seekdeep_session_persistence::SESSION_PERSISTENCE;
use seekdeep_session_persistence_jsonl::{JsonlConfig, install as install_jsonl};
use seekdeep_session_projection::SessionProjectionRegistry;
use seekdeep_settings_file::{FileSettingsConfig, install as install_settings};
use seekdeep_system_prompt::{SystemPromptConfig, install as install_system_prompt};
use seekdeep_tool_todo::{Config as TodoConfig, apply as install_todo};
use seekdeep_tools::{ToolPresentationMode, ToolRuntimeConfig, install as install_tools};
use seekdeep_util::{
    abort::AbortSignal,
    home_paths::resolve_process_seekdeep_home,
    launch_environment::{
        LaunchEnvironmentLayerInput, LaunchEnvironmentSnapshot, LaunchEnvironmentSource,
        SEEKDEEP_LAUNCH_ENVIRONMENT, create_launch_environment_snapshot,
    },
};

/// Shipped headless persona after the product rename.
pub const HEADLESS_PERSONA: &str = concat!(
    "You are a coding agent powered by the {{model}} model. ",
    "Your working directory is {{cwd}}."
);

/// Default provider route in the shipped base composition.
pub const DEFAULT_PROVIDER: &str = "deepseek-official";
/// Default model in the shipped base composition.
pub const DEFAULT_MODEL: &str = "deepseek-v4-flash";
/// Temporary process-wide tool-presentation override retained by the source.
pub const TOOLS_MODE_ENV: &str = "SEEKDEEP_TOOLS_MODE";

/// Inputs resolved by the launcher before the typed runtime is mounted.
#[derive(Clone, Debug)]
pub struct HeadlessBootOptions {
    /// Absolute Harness home containing settings, credentials, and sessions.
    pub seekdeep_home: PathBuf,
    /// Absolute workspace recorded in the new Session and prompt.
    pub cwd: PathBuf,
    /// Native DeepSeek provider configuration.
    pub deepseek: DeepSeekConfig,
    /// Frozen launcher environment consumed by providers and credentials.
    pub launch_environment: LaunchEnvironmentSnapshot,
    /// Deployment-wide model-facing tool presentation.
    pub tools_mode: ToolPresentationMode,
    /// Initial provider route for new agents.
    pub provider: ProviderId,
    /// Initial model for new agents.
    pub model: ModelId,
}

impl HeadlessBootOptions {
    /// Resolves process defaults without reading profile files.
    ///
    /// # Errors
    ///
    /// Returns when the operating-system home or current directory cannot be
    /// resolved.
    pub fn from_process() -> anyhow::Result<Self> {
        let launch_environment = process_environment_snapshot();
        Ok(Self {
            seekdeep_home: resolve_process_seekdeep_home(None)?,
            cwd: std::env::current_dir()?,
            deepseek: DeepSeekConfig::default(),
            tools_mode: resolve_tools_mode(&launch_environment)?,
            launch_environment,
            provider: ProviderId::new(DEFAULT_PROVIDER),
            model: ModelId::new(DEFAULT_MODEL),
        })
    }

    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.seekdeep_home.is_absolute(),
            "seekdeep home must be absolute, got {:?}",
            self.seekdeep_home
        );
        anyhow::ensure!(
            self.cwd.is_absolute(),
            "headless working directory must be absolute, got {:?}",
            self.cwd
        );
        Ok(())
    }
}

/// Fully mounted native headless application.
///
/// The agent loop is not yet a Cordis plugin, so this owner must dispose it
/// before disposing the root tree. Dropping this value without calling
/// [`Self::shutdown`] is a lifecycle error.
pub struct HeadlessApplication {
    root: Context,
    agents: Arc<AgentRegistry>,
    factory_registration: AgentFactoryRegistration,
    agent_loop: AgentLoop,
    runner: HeadlessRunner,
    shutdown: tokio::sync::OnceCell<ApplicationShutdown>,
}

impl std::fmt::Debug for HeadlessApplication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HeadlessApplication")
            .field("root_state", &self.root.fiber().state())
            .field("runner", &self.runner)
            .field("shutdown_started", &self.shutdown.get().is_some())
            .finish_non_exhaustive()
    }
}

impl HeadlessApplication {
    /// Transactionally assembles the current native headless closure.
    ///
    /// Partial startup is completely rolled back. This closure intentionally
    /// contains only the services needed for a real model/tool/persistence
    /// turn; the source-compatible profile loader remains the authority for
    /// the eventual 81-row shipped composition.
    ///
    /// # Errors
    ///
    /// Returns the primary startup failure after attempting full rollback.
    pub async fn boot(options: HeadlessBootOptions) -> anyhow::Result<Self> {
        Self::boot_inner(options, None).await
    }

    /// Transactionally assembles the native headless closure while observing
    /// a process-startup cancellation signal.
    ///
    /// Cancellation drops the active mount future and fully rolls back every
    /// service that was already published before returning.
    ///
    /// # Errors
    ///
    /// Returns the primary startup or cancellation failure after attempting
    /// full rollback.
    pub async fn boot_with_abort(
        options: HeadlessBootOptions,
        signal: AbortSignal,
    ) -> anyhow::Result<Self> {
        Self::boot_inner(options, Some(signal)).await
    }

    async fn boot_inner(
        options: HeadlessBootOptions,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Self> {
        options.validate()?;
        let root = Context::new();
        let mut agent_loop = None;
        let assembled = {
            let assembly = assemble(&root, &options, &mut agent_loop);
            tokio::pin!(assembly);
            if let Some(signal) = &signal {
                tokio::select! {
                    biased;
                    () = signal.cancelled() => {
                        Err(anyhow::anyhow!("headless application boot interrupted"))
                    }
                    result = &mut assembly => result,
                }
            } else {
                assembly.await
            }
        };
        match assembled {
            Ok((agents, factory_registration, runner)) => Ok(Self {
                root,
                agents,
                factory_registration,
                agent_loop: agent_loop
                    .take()
                    .expect("successful assembly installs the agent loop"),
                runner,
                shutdown: tokio::sync::OnceCell::new(),
            }),
            Err(primary) => {
                let mut cleanup_errors = Vec::new();
                if let Some(loop_) = agent_loop
                    && let Err(error) = loop_.dispose().await
                {
                    cleanup_errors.push(format!("agent-loop rollback failed: {error:#}"));
                }
                if let Err(error) = root.fiber().dispose().await {
                    cleanup_errors.push(format!("root rollback failed: {error:#}"));
                }
                if cleanup_errors.is_empty() {
                    Err(primary)
                } else {
                    Err(anyhow::anyhow!(
                        "{primary:#}\n{}",
                        cleanup_errors.join("\n")
                    ))
                }
            }
        }
    }

    /// Drives one fresh Agent through a single task.
    #[must_use]
    pub async fn run(&self, task: &str) -> HeadlessRunResult {
        self.runner.run(task).await
    }

    /// Deterministically drains live Agents before plugin and persistence
    /// teardown.
    ///
    /// # Errors
    ///
    /// Aggregates every cleanup failure after attempting the complete order.
    pub async fn shutdown(&self) -> anyhow::Result<()> {
        self.shutdown
            .get_or_init(|| async {
                ApplicationShutdown::start(
                    self.root.clone(),
                    self.agents.clone(),
                    self.factory_registration.clone(),
                    self.agent_loop.clone(),
                )
            })
            .await
            .wait()
            .await
    }
}

#[derive(Debug)]
struct ApplicationShutdown {
    result: tokio::sync::watch::Receiver<Option<Result<(), Arc<str>>>>,
}

impl ApplicationShutdown {
    fn start(
        root: Context,
        agents: Arc<AgentRegistry>,
        factory_registration: AgentFactoryRegistration,
        agent_loop: AgentLoop,
    ) -> Self {
        let (sender, result) = tokio::sync::watch::channel(None);
        drop(tokio::spawn(async move {
            let result = shutdown_owned(root, agents, factory_registration, agent_loop)
                .await
                .map_err(|error| Arc::<str>::from(format!("{error:#}")));
            sender.send_replace(Some(result));
        }));
        Self { result }
    }

    async fn wait(&self) -> anyhow::Result<()> {
        let mut result = self.result.clone();
        loop {
            if let Some(outcome) = result.borrow().clone() {
                return outcome.map_err(|message| anyhow::anyhow!(message.to_string()));
            }
            anyhow::ensure!(
                result.changed().await.is_ok(),
                "headless application shutdown task ended without an outcome"
            );
        }
    }
}

async fn shutdown_owned(
    root: Context,
    agents: Arc<AgentRegistry>,
    factory_registration: AgentFactoryRegistration,
    agent_loop: AgentLoop,
) -> anyhow::Result<()> {
    let mut errors = Vec::new();
    if let Err(error) = factory_registration.dispose().await {
        errors.push(format!("agent-factory withdrawal failed: {error:#}"));
    }
    agents.close_initiators();
    if let Err(error) = agent_loop.dispose().await {
        errors.push(format!("agent-loop shutdown failed: {error:#}"));
    }
    agents.dispose_initiators().await;
    if let Err(error) = root.fiber().dispose().await {
        errors.push(format!("plugin-tree shutdown failed: {error:#}"));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow::anyhow!(errors.join("\n")))
    }
}

async fn assemble(
    root: &Context,
    options: &HeadlessBootOptions,
    agent_loop_slot: &mut Option<AgentLoop>,
) -> anyhow::Result<(Arc<AgentRegistry>, AgentFactoryRegistration, HeadlessRunner)> {
    root.provide(
        SEEKDEEP_LAUNCH_ENVIRONMENT,
        Arc::new(options.launch_environment.clone()),
    )?;

    let settings = install_settings(
        root,
        FileSettingsConfig {
            seekdeep_home: Some(options.seekdeep_home.clone()),
            ..FileSettingsConfig::default()
        },
    )?;
    settings.await_settled().await?;

    let credentials = install_credentials(
        root,
        LocalCredentialConfig {
            seekdeep_home: Some(options.seekdeep_home.clone()),
            ..LocalCredentialConfig::default()
        },
    )?;
    credentials.await_settled().await?;

    let sessions = SessionStore::install(root)?;
    SessionProjectionRegistry::install(root)?;
    let persistence = install_jsonl(
        root,
        JsonlConfig::new(options.seekdeep_home.join("sessions")),
    )?;
    persistence.await_settled().await?;
    let persistence = root.get(SESSION_PERSISTENCE).ok_or_else(|| {
        anyhow::anyhow!("session-persistence-jsonl did not publish sessionPersistence")
    })?;

    let agents = Arc::new(AgentRegistry::new(root.clone()));
    agents.provide(root)?;
    let llm = LlmRuntime::install(root)?;

    let deepseek = install_deepseek(root, options.deepseek.clone())?;
    deepseek.await_settled().await?;

    let default_model = install_default_model(
        root,
        AgentDefaultModelConfig {
            provider: options.provider.clone(),
            model: options.model.clone(),
        },
    )?;
    default_model.await_settled().await?;
    let selection = root
        .get(AGENT_DEFAULT_MODEL)
        .ok_or_else(|| anyhow::anyhow!("agent-default-model did not publish agentDefaultModel"))?
        .current_selection();

    let prompt = install_system_prompt(
        root,
        SystemPromptConfig {
            persona: HEADLESS_PERSONA.to_owned(),
            ..SystemPromptConfig::default()
        },
    )?;
    seekdeep_code_runtime_worker_thread::install(root, &WorkerThreadCodeRuntimeConfig::default())?;
    let tools = install_tools(
        root,
        &prompt,
        ToolRuntimeConfig {
            mode: options.tools_mode,
            ..ToolRuntimeConfig::default()
        },
    )?;
    install_todo(
        root,
        TodoConfig {
            allow_parallel_in_progress: true,
        },
    )?;
    install_request_invariant(root, &llm, sessions.clone())?;
    seekdeep_session_checkpoint_policy::install(root, &llm, &sessions, &tools).await?;
    let retry = install_llm_retry(root, RetryConfig::default())?;
    retry.await_settled().await?;

    let agent_loop = AgentLoop::new(
        root.clone(),
        sessions.clone(),
        (*agents).clone(),
        AgentLoopServices {
            llm,
            system_prompt: prompt.clone(),
            tools,
            max_parallel_tool_calls: DEFAULT_MAX_PARALLEL_TOOL_CALLS,
        },
    )?;
    *agent_loop_slot = Some(agent_loop.clone());
    agent_loop.set_persistence(persistence.persistence())?;
    let factory_registration = agents.register_factory(root, Arc::new(agent_loop))?;

    let runner = HeadlessRunner::new(
        agents.clone(),
        sessions,
        prompt,
        selection,
        display_path(&options.cwd),
    )?;
    Ok((agents, factory_registration, runner))
}

fn process_environment_snapshot() -> seekdeep_util::launch_environment::LaunchEnvironmentSnapshot {
    let values = std::env::vars_os()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    create_launch_environment_snapshot(&[LaunchEnvironmentLayerInput {
        source: LaunchEnvironmentSource::Process,
        path: None,
        values,
    }])
}

fn resolve_tools_mode(
    environment: &LaunchEnvironmentSnapshot,
) -> anyhow::Result<ToolPresentationMode> {
    let Some(raw) = environment.get(TOOLS_MODE_ENV) else {
        return Ok(ToolPresentationMode::Native);
    };
    match raw.value.as_str() {
        "native" => Ok(ToolPresentationMode::Native),
        "code" => Ok(ToolPresentationMode::Code),
        "both" => Ok(ToolPresentationMode::Both),
        value => anyhow::bail!(
            "tools: mode must be one of \"native\", \"code\", or \"both\", got {value:?}"
        ),
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
