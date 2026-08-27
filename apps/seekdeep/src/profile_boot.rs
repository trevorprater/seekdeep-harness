//! Source-compatible profile layer composition and live configuration boot.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI32, Ordering},
    },
};

use indexmap::IndexMap;
use path_clean::PathClean as _;
use seekdeep_app_boot::{
    BootOptions, BootPrepare, BootUserPatchWatchOptions, BootedApplication, ConfigRefreshFailure,
    ConfigWatchRegistry, OwnedConfigWatcher, PROFILE_PATCH_FILENAME, PatchComposer, Profile, boot,
    compose_entries, load_optional_patches, load_overlay_patches, watch_boot_user_patches,
};
use seekdeep_cmdline::{CmdlineHost, provide_cmdline};
use seekdeep_cordis::FiberState;
use seekdeep_cordis_timer::TIMER;
use seekdeep_hmr::HMR;
use seekdeep_loader::{
    Entry, EntryId, EntryParent, ExpressionEnvironment, LOADER, PluginCatalog, PluginSpecifier,
    profile_patch::{ProfileEntry, ProfileNode, ProfilePatch, ProfilePatchWarning},
};
use seekdeep_util::launch_environment::{LaunchEnvironmentSnapshot, SEEKDEEP_LAUNCH_ENVIRONMENT};

use crate::{process_shutdown::ProcessShutdown, profile_support};

const NAME: &str = "seekdeep";
const TELEMETRY_ROW_ID: &str = "session-telemetry-otel";
const AGENT_PRESETS_ROW_ID: &str = "agent-presets";
const FRAMEWORK_TIMER_ID: &str = "profile-watch-timer";
const FRAMEWORK_HMR_ID: &str = "profile-watch-hmr";
const EXIT_CODE_UNSET: i32 = i32::MIN;

/// Privacy switch applied after every user-controlled profile layer.
pub const TELEMETRY_DISABLED_ENV: &str = "SEEKDEEP_TELEMETRY_DISABLED";

/// Shipped agent-preset asset root in the source-checkout application layout.
#[must_use]
pub fn shipped_preset_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../cli/config/agent-presets")
        .clean()
}

/// Builds the profile framework catalog over one frozen launch environment.
///
/// Product plugins join this catalog as their compiled Loader boundaries are ported.
/// Unknown installed packages remain eligible for the model-authored JavaScript
/// compatibility loader under the profile's module-resolution anchors.
///
/// # Errors
///
/// Returns executable-path, expression-environment, or registration failures.
pub fn framework_profile_catalog(
    cwd: &Path,
    home: &Path,
    environment: &LaunchEnvironmentSnapshot,
) -> anyhow::Result<PluginCatalog> {
    let platform = match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    };
    let expressions = ExpressionEnvironment::from_launch_environment(
        environment,
        cwd.to_owned(),
        std::env::current_exe()?,
        platform,
        env!("CARGO_PKG_VERSION"),
        home.to_owned(),
    );
    let catalog = PluginCatalog::new()
        .with_expression_environment(expressions)
        .with_bare_module_base(profile_support::install_anchor(home));
    register_profile_framework_plugins(&catalog)?;
    register_compiled_profile_plugins(&catalog)?;
    Ok(catalog)
}

/// Fully resolved profile layer stack and its pre-launch row index.
#[derive(Clone, Debug)]
pub struct ProfileBootPlan {
    profile: Profile,
    bundle_patches: Vec<ProfilePatch>,
    home_patch_path: PathBuf,
    home_patches: Vec<ProfilePatch>,
    overlays: Vec<ProfilePatch>,
    rows: IndexMap<String, ProfileEntry>,
    warnings: Vec<ProfilePatchWarning>,
}

/// Booted profile plus its explicitly ordered live-config owners.
pub struct ProfileBootApplication {
    application: BootedApplication,
    watchers: Vec<OwnedConfigWatcher>,
}

impl std::fmt::Debug for ProfileBootApplication {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProfileBootApplication")
            .field("application", &self.application)
            .field("watcher_count", &self.watchers.len())
            .finish()
    }
}

impl ProfileBootApplication {
    /// Root context carrying the mounted profile services.
    #[must_use]
    pub fn context(&self) -> &seekdeep_cordis::Context {
        self.application.context()
    }

    /// Loaded composition, absent when an app disposed itself during startup.
    #[must_use]
    pub fn composition(&self) -> Option<&seekdeep_loader::LoadedComposition> {
        self.application.composition()
    }

    /// Drains config watchers before disposing the Loader tree.
    ///
    /// # Errors
    ///
    /// Aggregates every watcher and application cleanup failure after attempting all of them.
    pub async fn dispose(self) -> anyhow::Result<()> {
        dispose_profile_parts(self.application, self.watchers).await
    }
}

#[derive(Debug, Default)]
struct ProfileApplicationSlot {
    application: tokio::sync::Mutex<Option<ProfileBootApplication>>,
    context: tokio::sync::Mutex<Option<seekdeep_cordis::Context>>,
    boot_finished: AtomicBool,
    shutdown_started: AtomicBool,
    changed: tokio::sync::Notify,
}

impl ProfileApplicationSlot {
    async fn publish_context(&self, context: seekdeep_cordis::Context) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.shutdown_started.load(Ordering::Acquire),
            "profile shutdown started before host preparation"
        );
        let mut slot = self.context.lock().await;
        anyhow::ensure!(
            slot.is_none(),
            "profile boot context was published more than once"
        );
        *slot = Some(context);
        drop(slot);
        self.changed.notify_waiters();
        Ok(())
    }

    async fn publish(
        &self,
        application: ProfileBootApplication,
    ) -> Result<(), ProfileBootApplication> {
        let mut slot = self.application.lock().await;
        if slot.is_some()
            || self.boot_finished.load(Ordering::Acquire)
            || self.shutdown_started.load(Ordering::Acquire)
        {
            return Err(application);
        }
        *slot = Some(application);
        self.boot_finished.store(true, Ordering::Release);
        drop(slot);
        self.changed.notify_waiters();
        Ok(())
    }

    fn finish_without_application(&self) {
        self.boot_finished.store(true, Ordering::Release);
        self.changed.notify_waiters();
    }

    fn is_shutdown_started(&self) -> bool {
        self.shutdown_started.load(Ordering::Acquire)
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        self.shutdown_started.store(true, Ordering::Release);
        loop {
            let changed = self.changed.notified();
            if let Some(application) = self.application.lock().await.take() {
                return application.dispose().await;
            }
            if let Some(context) = self.context.lock().await.take() {
                return context.fiber().dispose().await;
            }
            if self.boot_finished.load(Ordering::Acquire) {
                return Ok(());
            }
            changed.await;
        }
    }
}

#[derive(Debug, Default)]
struct ProfileProcessCompletion {
    code: AtomicI32,
    changed: tokio::sync::Notify,
}

impl ProfileProcessCompletion {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            code: AtomicI32::new(EXIT_CODE_UNSET),
            changed: tokio::sync::Notify::new(),
        })
    }

    fn complete(&self, code: i32) {
        if self
            .code
            .compare_exchange(EXIT_CODE_UNSET, code, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.changed.notify_waiters();
        }
    }

    async fn wait(&self) -> i32 {
        loop {
            let changed = self.changed.notified();
            let code = self.code.load(Ordering::Acquire);
            if code != EXIT_CODE_UNSET {
                return code;
            }
            changed.await;
        }
    }
}

/// Long-lived profile process waiting for app exit or a process signal.
pub struct RunningProfile {
    context: seekdeep_cordis::Context,
    shutdown: ProcessShutdown,
    completion: Arc<ProfileProcessCompletion>,
    signals: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl std::fmt::Debug for RunningProfile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunningProfile")
            .field("fiber_state", &self.context.fiber().state())
            .finish_non_exhaustive()
    }
}

impl RunningProfile {
    /// Root profile context, including launcher environment and cmdline facts.
    #[must_use]
    pub fn context(&self) -> &seekdeep_cordis::Context {
        &self.context
    }

    /// Waits for an app-requested normal exit and joins signal observation.
    ///
    /// # Errors
    ///
    /// Returns when the signal task fails or ends without terminating the process.
    pub async fn wait(self) -> anyhow::Result<i32> {
        let mut signals = self.signals;
        tokio::select! {
            code = self.completion.wait() => {
                signals.abort();
                match signals.await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => return Err(error),
                    Err(error) if error.is_cancelled() => {}
                    Err(error) => return Err(error.into()),
                }
                Ok(code)
            }
            result = &mut signals => {
                result??;
                anyhow::bail!("seekdeep: profile signal task ended without terminating the process")
            }
        }
    }

    /// Requests ordinary bounded shutdown, then returns the selected exit code.
    ///
    /// # Errors
    ///
    /// Returns application cleanup or signal-task failures.
    pub async fn shutdown(self, code: i32) -> anyhow::Result<i32> {
        self.shutdown.shutdown(code).await;
        self.wait().await
    }
}

impl ProfileBootPlan {
    /// Loaded profile identity, paths, bundle layers, and initial user layer.
    #[must_use]
    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    /// Home-wide patch file applied after the profile-owned user layer.
    #[must_use]
    pub fn home_patch_path(&self) -> &Path {
        &self.home_patch_path
    }

    /// Effective pre-launch row by exact profile id.
    #[must_use]
    pub fn row(&self, id: &str) -> Option<&ProfileEntry> {
        self.rows.get(id)
    }

    /// Skipped-patch diagnostics produced while indexing the launcher's own rows.
    #[must_use]
    pub fn warnings(&self) -> &[ProfilePatchWarning] {
        &self.warnings
    }

    /// Complete initial patch list in source application order.
    #[must_use]
    pub fn all_patches(&self) -> Vec<ProfilePatch> {
        self.bundle_patches
            .iter()
            .chain(&self.profile.patches)
            .chain(&self.home_patches)
            .chain(&self.overlays)
            .cloned()
            .collect()
    }

    fn live_patches(&self) -> anyhow::Result<Vec<ProfilePatch>> {
        let profile = load_optional_patches(NAME, &self.profile.patch_path)?.unwrap_or_default();
        let home = load_optional_patches(NAME, &self.home_patch_path)?.unwrap_or_default();
        Ok(self
            .bundle_patches
            .iter()
            .chain(&profile)
            .chain(&home)
            .chain(&self.overlays)
            .cloned()
            .collect())
    }
}

/// Resolves the privacy opt-out into the fixed telemetry-row patch.
///
/// Every non-empty value, including `0` and `false`, disables telemetry. A
/// profile without the telemetry row needs no patch.
#[must_use]
pub fn resolve_telemetry_patch(
    disabled_environment: Option<&str>,
    has_row: bool,
) -> Option<ProfilePatch> {
    if disabled_environment.unwrap_or_default().is_empty() || !has_row {
        return None;
    }
    Some(ProfilePatch::from_fields(IndexMap::from([
        (
            "id".to_owned(),
            ProfileNode::String(TELEMETRY_ROW_ID.to_owned()),
        ),
        ("disabled".to_owned(), ProfileNode::Bool(true)),
    ])))
}

fn agent_presets_patch(row: &ProfileEntry, shipped_root: &Path) -> ProfilePatch {
    let mut config = row
        .config()
        .and_then(ProfileNode::as_mapping)
        .cloned()
        .unwrap_or_default();
    config.insert(
        "roots".to_owned(),
        ProfileNode::Sequence(vec![ProfileNode::Mapping(IndexMap::from([
            (
                "path".to_owned(),
                ProfileNode::String(shipped_root.to_string_lossy().into_owned()),
            ),
            ("trust".to_owned(), ProfileNode::String("system".to_owned())),
        ]))]),
    );
    ProfilePatch::from_fields(IndexMap::from([
        (
            "id".to_owned(),
            ProfileNode::String(AGENT_PRESETS_ROW_ID.to_owned()),
        ),
        ("config".to_owned(), ProfileNode::Mapping(config)),
    ]))
}

/// Resolves and composes one profile without mounting its plugin tree.
///
/// # Errors
///
/// Returns profile, bundle, overlay, home-layer, or patch-composition failures.
#[allow(clippy::too_many_arguments)]
pub fn compose_profile_at(
    profile_name: &str,
    overlay_files: &[PathBuf],
    cwd: &Path,
    home: &Path,
    install_anchor: &Path,
    shipped_preset_root: &Path,
    telemetry_disabled: Option<&str>,
) -> anyhow::Result<ProfileBootPlan> {
    let profile = profile_support::prepare_profile_at(profile_name, true, home, install_anchor)?;
    let home_patch_path = home.join(PROFILE_PATCH_FILENAME);
    let home_patches = load_optional_patches(NAME, &home_patch_path)?.unwrap_or_default();
    let mut overlays = Vec::new();
    for filename in overlay_files {
        let absolute = if filename.is_absolute() {
            filename.clone()
        } else {
            cwd.join(filename)
        }
        .clean();
        overlays.extend(load_overlay_patches(NAME, &absolute)?);
    }
    let bundle_patches = profile
        .layers
        .iter()
        .flat_map(|layer| layer.patches.iter().cloned())
        .collect::<Vec<_>>();
    let indexed = compose_entries(&[
        bundle_patches.clone(),
        profile.patches.clone(),
        home_patches.clone(),
        overlays.clone(),
    ])?;
    let (entries, warnings) = indexed.into_parts();
    let rows = entries
        .into_iter()
        .filter_map(|entry| entry.id().map(|id| (id.as_str().to_owned(), entry)))
        .collect::<IndexMap<_, _>>();
    if let Some(row) = rows.get(AGENT_PRESETS_ROW_ID) {
        overlays.push(agent_presets_patch(row, shipped_preset_root));
    }
    if let Some(patch) =
        resolve_telemetry_patch(telemetry_disabled, rows.contains_key(TELEMETRY_ROW_ID))
    {
        overlays.push(patch);
    }
    Ok(ProfileBootPlan {
        profile,
        bundle_patches,
        home_patch_path,
        home_patches,
        overlays,
        rows,
        warnings,
    })
}

/// Registers the compiled framework plugins needed by profile boot and config HMR.
///
/// # Errors
///
/// Returns duplicate or invalid catalog-registration failures.
pub fn register_profile_framework_plugins(catalog: &PluginCatalog) -> anyhow::Result<()> {
    let timer = seekdeep_cordis_timer::plugin();
    catalog.register_named("cordis-plugin-timer", timer.clone())?;
    catalog.register_named("@seekdeep-ai/cordis-plugin-timer", timer)?;
    let restart: seekdeep_hmr::RestartHook = Arc::new(|| Box::pin(async { Ok(()) }));
    let hmr = seekdeep_hmr::plugin(restart);
    catalog.register_named("cordis-plugin-hmr", hmr.clone())?;
    catalog.register_named("@seekdeep-ai/cordis-plugin-hmr", hmr)?;
    Ok(())
}

fn register_product_plugin(
    catalog: &PluginCatalog,
    name: &str,
    plugin: seekdeep_cordis::Plugin,
) -> anyhow::Result<()> {
    catalog.register_named(name, plugin.clone())?;
    catalog.register_named(&format!("@seekdeep-ai/{name}"), plugin)?;
    Ok(())
}

/// Registers every product Host plugin that currently exposes a compiled Loader boundary.
///
/// # Errors
///
/// Returns duplicate or invalid catalog-registration failures.
#[allow(clippy::too_many_lines)]
pub fn register_compiled_profile_plugins(catalog: &PluginCatalog) -> anyhow::Result<()> {
    for (name, plugin) in [
        ("seekdeep-agent", seekdeep_agent::plugin()),
        ("seekdeep-agent-loop", seekdeep_agent_loop::plugin()),
        (
            "seekdeep-agent-default-model",
            seekdeep_agent_default_model::plugin(),
        ),
        (
            "seekdeep-agent-instructions",
            seekdeep_agent_instructions::plugin(),
        ),
        ("seekdeep-api-gateway", seekdeep_api_gateway::plugin()),
        ("seekdeep-api-remotes", seekdeep_api_remotes::host_plugin()),
        (
            "seekdeep-attachment-local",
            seekdeep_attachment_local::plugin(),
        ),
        ("seekdeep-bash-sandbox", seekdeep_bash_sandbox::plugin()),
        (
            "seekdeep-client-connection",
            seekdeep_client_connection::host_plugin(),
        ),
        (
            "seekdeep-client-hmr",
            seekdeep_client_hmr::client_hmr_host_plugin(),
        ),
        (
            "seekdeep-client-locale",
            seekdeep_client_locale::host_plugin(),
        ),
        (
            "seekdeep-client-modules",
            seekdeep_client_modules::client_modules_host_plugin(),
        ),
        (
            "seekdeep-client-runtime",
            seekdeep_client_runtime::host_plugin(),
        ),
        (
            "seekdeep-client-ui-cordis",
            seekdeep_client_ui_cordis::host_plugin(),
        ),
        (
            "seekdeep-client-ui-conversation",
            seekdeep_client_ui_conversation::host_plugin(),
        ),
        (
            "seekdeep-client-ui-deliverables",
            seekdeep_client_ui_deliverables::host_plugin(),
        ),
        (
            "seekdeep-client-ui-layout",
            seekdeep_client_ui_layout::host_plugin(),
        ),
        (
            "seekdeep-client-ui-message-feedback",
            seekdeep_client_ui_message_feedback::host_plugin(),
        ),
        (
            "seekdeep-client-ui-settings",
            seekdeep_client_ui_settings::host_plugin(),
        ),
        (
            "seekdeep-client-ui-settings-general",
            seekdeep_client_ui_settings_general::host_plugin(),
        ),
        (
            "seekdeep-client-ui-settings-models",
            seekdeep_client_ui_settings_models::host_plugin(),
        ),
        (
            "seekdeep-client-ui-settings-plugin-inventory",
            seekdeep_client_ui_settings_plugin_inventory::host_plugin(),
        ),
        (
            "seekdeep-client-ui-sidebar",
            seekdeep_client_ui_sidebar::host_plugin(),
        ),
        (
            "seekdeep-client-ui-theme",
            seekdeep_client_ui_theme::host_plugin(),
        ),
        (
            "seekdeep-client-ui-tool",
            seekdeep_client_ui_tool::host_plugin(),
        ),
        (
            "seekdeep-client-ui-workflow-run",
            seekdeep_client_ui_workflow_run::host_plugin(),
        ),
        (
            "seekdeep-code-runtime-worker-thread",
            seekdeep_code_runtime_worker_thread::plugin(),
        ),
        (
            "seekdeep-command-compact",
            seekdeep_command_compact::index::plugin(),
        ),
        (
            "seekdeep-command-feedback",
            seekdeep_command_feedback::plugin(),
        ),
        ("seekdeep-command-goal", seekdeep_command_goal::plugin()),
        ("seekdeep-commands", seekdeep_commands::plugin()),
        (
            "seekdeep-compaction-basic",
            seekdeep_compaction_basic::plugin(),
        ),
        (
            "seekdeep-compaction-tool-result-pruner",
            seekdeep_compaction_tool_result_pruner::plugin(),
        ),
        (
            "seekdeep-cordis-client-runner",
            seekdeep_cordis_client_runner::host_plugin(),
        ),
        (
            "seekdeep-cordis-host-runner",
            seekdeep_cordis_host_runner::plugin(),
        ),
        (
            "seekdeep-credentials-local",
            seekdeep_credentials_local::plugin(),
        ),
        (
            "seekdeep-fs-observation-policy",
            seekdeep_fs_observation_policy::plugin(),
        ),
        ("seekdeep-fs-sandbox", seekdeep_fs_sandbox::plugin()),
        ("seekdeep-goal", seekdeep_goal::plugin()),
        (
            "seekdeep-goal-round-driver",
            seekdeep_goal_round_driver::plugin(),
        ),
        (
            "seekdeep-host-directory-picker-auto",
            seekdeep_host_directory_picker_auto::plugin(),
        ),
        ("seekdeep-host-apiproxy", seekdeep_host_apiproxy::plugin()),
        ("seekdeep-host-webserver", seekdeep_host_webserver::plugin()),
        (
            "seekdeep-host-plugin-inventory",
            seekdeep_host_plugin_inventory::plugin(),
        ),
        ("seekdeep-llm", seekdeep_llm::plugin()),
        ("seekdeep-llm-deepseek", seekdeep_llm_deepseek::plugin()),
        ("seekdeep-llm-pi-ai", seekdeep_llm_pi_ai::plugin()),
        ("seekdeep-llm-retry", seekdeep_llm_retry::plugin()),
        (
            "seekdeep-message-feedback",
            seekdeep_message_feedback::plugin(),
        ),
        (
            "seekdeep-permission-presets",
            seekdeep_permission_presets::plugin(),
        ),
        ("seekdeep-plan-mode", seekdeep_plan_mode::plugin()),
        ("seekdeep-pwsh-sandbox", seekdeep_pwsh_sandbox::plugin()),
        (
            "seekdeep-repeat-tool-reminder",
            seekdeep_repeat_tool_reminder::plugin(),
        ),
        ("seekdeep-sandbox-local", seekdeep_sandbox_local::plugin()),
        ("seekdeep-sandbox-policy", seekdeep_sandbox_policy::plugin()),
        ("seekdeep-session", seekdeep_core::session_store::plugin()),
        (
            "seekdeep-session-checkpoint-policy",
            seekdeep_session_checkpoint_policy::plugin(),
        ),
        (
            "seekdeep-session-log-export",
            seekdeep_session_log_export::plugin(),
        ),
        (
            "seekdeep-session-persistence-jsonl",
            seekdeep_session_persistence_jsonl::plugin(),
        ),
        (
            "seekdeep-session-projection",
            seekdeep_session_projection::plugin(),
        ),
        (
            "seekdeep-session-projection-cache",
            seekdeep_session_projection_cache::plugin(),
        ),
        (
            "seekdeep-session-query-sqlite",
            seekdeep_session_query_sqlite::plugin(),
        ),
        (
            "seekdeep-session-telemetry-otel",
            seekdeep_session_telemetry_otel::plugin(),
        ),
        ("seekdeep-session-stats", seekdeep_session_stats::plugin()),
        ("seekdeep-session-title", seekdeep_session_title::plugin()),
        (
            "seekdeep-session-title-first-prompt-llm",
            seekdeep_session_title_first_prompt_llm::plugin(),
        ),
        ("seekdeep-settings-file", seekdeep_settings_file::plugin()),
        ("seekdeep-shell-env", seekdeep_shell_env::plugin()),
        ("seekdeep-skill", seekdeep_skill::plugin()),
        ("seekdeep-skill-badge", seekdeep_skill_badge::plugin()),
        (
            "seekdeep-skill-filesystem",
            seekdeep_skill_filesystem::plugin(),
        ),
        ("seekdeep-spill-local", seekdeep_spill_local::plugin()),
        ("seekdeep-spill-policy", seekdeep_spill_policy::plugin()),
        ("seekdeep-storage", seekdeep_storage::plugin()),
        ("seekdeep-storage-domain", seekdeep_storage_domain::plugin()),
        ("seekdeep-storage-json", seekdeep_storage_json::plugin()),
        ("seekdeep-subagent", seekdeep_subagent::plugin()),
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
        ("seekdeep-system-prompt", seekdeep_system_prompt::plugin()),
        ("seekdeep-token-meter", seekdeep_token_meter::plugin()),
        ("seekdeep-tool-bash", seekdeep_tool_bash::plugin()),
        ("seekdeep-tool-fs", seekdeep_tool_fs::plugin()),
        ("seekdeep-tool-fs-search", seekdeep_tool_fs_search::plugin()),
        ("seekdeep-tool-goal", seekdeep_tool_goal::index::plugin()),
        ("seekdeep-tool-jobs", seekdeep_tool_jobs::index::plugin()),
        ("seekdeep-tool-pwsh", seekdeep_tool_pwsh::plugin()),
        ("seekdeep-tool-skill", seekdeep_tool_skill::plugin()),
        (
            "seekdeep-tool-str-replace-editor",
            seekdeep_tool_str_replace_editor::plugin(),
        ),
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
        (
            "seekdeep-tool-call-timeout-policy",
            seekdeep_tool_timeout_policy::plugin(),
        ),
        ("seekdeep-tool-web", seekdeep_tool_web::plugin()),
        ("seekdeep-tools", seekdeep_tools::plugin()),
        ("seekdeep-typert-loader", seekdeep_typert_loader::plugin()),
        (
            "seekdeep-typert-registry",
            seekdeep_typert_registry::plugin(),
        ),
        ("seekdeep-user-approval", seekdeep_user_approval::plugin()),
        ("seekdeep-user-questions", seekdeep_user_questions::plugin()),
        ("seekdeep-web", seekdeep_web::plugin()),
        ("seekdeep-web-app", seekdeep_web_app::plugin()),
        (
            "seekdeep-web-search-deepseek",
            seekdeep_web_search_deepseek::plugin(),
        ),
        (
            "seekdeep-workflow-worker-thread",
            seekdeep_workflow_worker_thread::plugin(),
        ),
        ("seekdeep-workspace", seekdeep_workspace::plugin()),
    ] {
        register_product_plugin(catalog, name, plugin)?;
    }
    register_product_plugin(
        catalog,
        "seekdeep-jobs-local",
        seekdeep_jobs_local::LocalJobRegistry::plugin(),
    )?;
    register_product_plugin(
        catalog,
        "seekdeep-tool-subagent-control/list-agents",
        seekdeep_tool_subagent_control::list_plugin(),
    )?;
    register_product_plugin(
        catalog,
        "seekdeep-web-app/startup",
        seekdeep_web_app::startup::plugin(),
    )?;
    Ok(())
}

fn unused_entry_id(
    base: &str,
    loader: &seekdeep_loader::LoaderSettlement,
) -> anyhow::Result<EntryId> {
    let entries = loader.entries()?;
    for suffix in 1_u64.. {
        let candidate = if suffix == 1 {
            base.to_owned()
        } else {
            format!("{base}-{suffix}")
        };
        let id = EntryId::new(candidate)?;
        if entries.iter().all(|entry| entry.id != id) {
            return Ok(id);
        }
    }
    unreachable!("the integer suffix space is unbounded for practical profiles")
}

async fn ensure_config_hmr(context: &seekdeep_cordis::Context) -> anyhow::Result<()> {
    if context.get(HMR).is_some() {
        return Ok(());
    }
    let loader = context
        .get(LOADER)
        .ok_or_else(|| anyhow::anyhow!("seekdeep: profile watching requires the Cordis Loader"))?;
    if context.get(TIMER).is_none() {
        loader
            .create_entry(
                Entry::new(
                    unused_entry_id(FRAMEWORK_TIMER_ID, &loader)?,
                    PluginSpecifier::new("@seekdeep-ai/cordis-plugin-timer")?,
                ),
                EntryParent::Root,
                None,
            )
            .await?;
    }
    let mut hmr = Entry::new(
        unused_entry_id(FRAMEWORK_HMR_ID, &loader)?,
        PluginSpecifier::new("@seekdeep-ai/cordis-plugin-hmr")?,
    );
    hmr.config = serde_json::json!({"root": []});
    loader.create_entry(hmr, EntryParent::Root, None).await?;
    loader.wait().await?;
    anyhow::ensure!(
        context.get(HMR).is_some(),
        "seekdeep: watch-only HMR did not activate"
    );
    Ok(())
}

async fn dispose_after_setup_failure(
    application: BootedApplication,
    watchers: Vec<OwnedConfigWatcher>,
    primary: anyhow::Error,
) -> anyhow::Error {
    match dispose_profile_parts(application, watchers).await {
        Ok(()) => primary,
        Err(cleanup) => anyhow::anyhow!("{primary:#}\nprofile setup cleanup failed: {cleanup:#}"),
    }
}

async fn dispose_profile_parts(
    application: BootedApplication,
    mut watchers: Vec<OwnedConfigWatcher>,
) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    while let Some(watcher) = watchers.pop() {
        if let Err(error) = watcher.dispose().await {
            failures.push(format!("profile watcher cleanup failed: {error:#}"));
        }
    }
    if let Err(error) = application.dispose().await {
        failures.push(format!("profile application cleanup failed: {error:#}"));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(failures.join("\n"))
    }
}

/// Boots a composed profile and installs last-good live watchers for both user layers.
///
/// # Errors
///
/// Returns profile-tree, fallback-HMR, watcher, activation, or rollback failures.
pub async fn boot_profile(
    plan: ProfileBootPlan,
    catalog: &PluginCatalog,
    prepare: Option<BootPrepare>,
) -> anyhow::Result<ProfileBootApplication> {
    let failure: ConfigRefreshFailure = Arc::new(|path, error| {
        eprintln!(
            "seekdeep: failed to refresh profile patches {}: {error:#}",
            path.display()
        );
    });
    boot_profile_with_failure_handler(plan, catalog, prepare, failure).await
}

/// Boots a profile with an explicit last-good refresh failure observer.
///
/// # Errors
///
/// Returns the same failures as [`boot_profile`].
pub async fn boot_profile_with_failure_handler(
    plan: ProfileBootPlan,
    catalog: &PluginCatalog,
    prepare: Option<BootPrepare>,
    failure: ConfigRefreshFailure,
) -> anyhow::Result<ProfileBootApplication> {
    let warning = Arc::new(|message: String| eprintln!("{message}"));
    let application = boot(
        NAME,
        &plan
            .profile
            .dir
            .join(profile_support::PROFILE_ROOT_FILENAME),
        catalog,
        BootOptions {
            patches: plan.all_patches(),
            prepare,
            warn: Some(warning),
        },
    )
    .await?;
    let context = application.context().clone();
    if application.composition().is_none()
        || context.fiber().state() != FiberState::Active
        || context.get(LOADER).is_none()
    {
        return Ok(ProfileBootApplication {
            application,
            watchers: Vec::new(),
        });
    }
    if let Err(error) = ensure_config_hmr(&context).await {
        return Err(dispose_after_setup_failure(application, Vec::new(), error).await);
    }
    let plan = Arc::new(plan);
    let compose: PatchComposer = Arc::new({
        let plan = plan.clone();
        move |_| plan.live_patches()
    });
    let registry = ConfigWatchRegistry::new();
    let mut watchers = Vec::new();
    for filename in [&plan.profile.patch_path, &plan.home_patch_path] {
        let result = watch_boot_user_patches(BootUserPatchWatchOptions {
            bin_name: NAME.to_owned(),
            filename: filename.clone(),
            compose: compose.clone(),
            context: context.clone(),
            registry: registry.clone(),
            failure: failure.clone(),
        })
        .await;
        match result {
            Ok(watcher) => watchers.push(watcher),
            Err(error) => {
                return Err(dispose_after_setup_failure(application, watchers, error).await);
            }
        }
    }
    Ok(ProfileBootApplication {
        application,
        watchers,
    })
}

#[cfg(unix)]
fn spawn_profile_signals(
    shutdown: ProcessShutdown,
) -> anyhow::Result<tokio::task::JoinHandle<anyhow::Result<()>>> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    Ok(tokio::spawn(async move {
        loop {
            tokio::select! {
                signal = terminate.recv() => {
                    anyhow::ensure!(signal.is_some(), "SIGTERM stream ended");
                    shutdown.interrupt_sigterm();
                }
                signal = interrupt.recv() => {
                    anyhow::ensure!(signal.is_some(), "SIGINT stream ended");
                    shutdown.interrupt_sigint();
                }
            }
        }
    }))
}

#[cfg(not(unix))]
fn spawn_profile_signals(
    shutdown: ProcessShutdown,
) -> anyhow::Result<tokio::task::JoinHandle<anyhow::Result<()>>> {
    Ok(tokio::spawn(async move {
        loop {
            tokio::signal::ctrl_c().await?;
            shutdown.interrupt_sigint();
        }
    }))
}

async fn stop_profile_signals(
    signals: tokio::task::JoinHandle<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    signals.abort();
    match signals.await {
        Ok(result) => result,
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Boots one profile under launcher-owned environment, cmdline, signal, and shutdown facts.
///
/// An app may request exit while its siblings are still mounting. The shutdown
/// owner waits for publication, then drains profile watchers before the Loader tree.
///
/// # Errors
///
/// Returns signal setup, host preparation, profile boot, publication, or rollback failures.
pub async fn run_profile_process(
    plan: ProfileBootPlan,
    catalog: &PluginCatalog,
    environment: LaunchEnvironmentSnapshot,
    arguments: Vec<String>,
) -> anyhow::Result<RunningProfile> {
    let application_slot = Arc::new(ProfileApplicationSlot::default());
    let completion = ProfileProcessCompletion::new();
    let application_for_shutdown = application_slot.clone();
    let completion_for_shutdown = completion.clone();
    let shutdown = ProcessShutdown::new(
        move || async move { application_for_shutdown.shutdown().await },
        std::process::exit,
        move |code| completion_for_shutdown.complete(code),
    );
    let signals = spawn_profile_signals(shutdown.clone())?;
    let shutdown_for_cmdline = shutdown.clone();
    let application_for_prepare = application_slot.clone();
    let prepare: BootPrepare = Arc::new(move |context| {
        let environment = environment.clone();
        let arguments = arguments.clone();
        let shutdown = shutdown_for_cmdline.clone();
        let application = application_for_prepare.clone();
        Box::pin(async move {
            application.publish_context(context.clone()).await?;
            context.provide(SEEKDEEP_LAUNCH_ENVIRONMENT, Arc::new(environment))?;
            provide_cmdline(
                &context,
                CmdlineHost::new(arguments, move |code| {
                    drop(shutdown.shutdown(code));
                    Ok(())
                }),
            )?;
            Ok(())
        })
    });
    let application = match boot_profile(plan, catalog, Some(prepare)).await {
        Ok(application) => application,
        Err(error) => {
            application_slot.finish_without_application();
            let signal_result = stop_profile_signals(signals).await;
            return match signal_result {
                Ok(()) => Err(error),
                Err(signals) => Err(anyhow::anyhow!(
                    "{error:#}\nprofile signal cleanup failed: {signals:#}"
                )),
            };
        }
    };
    let context = application.context().clone();
    if let Err(application) = application_slot.publish(application).await {
        let cleanup = application.dispose().await;
        if application_slot.is_shutdown_started() {
            cleanup?;
            return Ok(RunningProfile {
                context,
                shutdown,
                completion,
                signals,
            });
        }
        application_slot.finish_without_application();
        let signal_cleanup = stop_profile_signals(signals).await;
        return match (cleanup, signal_cleanup) {
            (Ok(()), Ok(())) => Err(anyhow::anyhow!(
                "seekdeep: profile application was published more than once"
            )),
            (cleanup, signals) => Err(anyhow::anyhow!(
                "seekdeep: profile application was published more than once\nprofile cleanup: {cleanup:?}\nprofile signal cleanup: {signals:?}"
            )),
        };
    }
    Ok(RunningProfile {
        context,
        shutdown,
        completion,
        signals,
    })
}
