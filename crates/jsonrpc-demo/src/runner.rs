//! Shared lifecycle for generic and packaged JSON-RPC launchers.

use std::{
    collections::BTreeMap,
    ffi::OsStr,
    io::Write as _,
    path::{Path, PathBuf},
    process::ExitCode,
};

use seekdeep_app_boot::{
    BootOptions, BootedApplication, boot, load_layered_env, resolve_config_path,
};
use seekdeep_cordis::Plugin;
use seekdeep_loader::{ExpressionEnvironment, PluginCatalog};
use seekdeep_util::home_paths::{SEEKDEEP_HOME_ENV, resolve_process_seekdeep_home};
use tokio::io::AsyncReadExt as _;

use crate::NAME;

/// Environment variable that overrides the positional configuration path.
pub const CONFIG_ENV: &str = "SEEKDEEP_CORDIS_CONFIG";

/// Resolves environment-over-argument configuration selection.
///
/// Empty values are absent. The selected file must exist.
///
/// # Errors
///
/// Returns the exact usage diagnostic when no existing config is selected.
pub fn selected_config_path(
    environment: &BTreeMap<String, String>,
    arguments: &[String],
    cwd: &Path,
) -> anyhow::Result<PathBuf> {
    let requested = environment
        .get(CONFIG_ENV)
        .filter(|value| !value.is_empty())
        .or_else(|| arguments.first().filter(|value| !value.is_empty()));
    let Some(requested) = requested else {
        anyhow::bail!(usage());
    };
    let path = resolve_config_path(Path::new(requested), None, cwd)?;
    anyhow::ensure!(path.exists(), usage());
    Ok(path)
}

/// Exact missing-config usage line.
#[must_use]
pub fn usage() -> String {
    format!(
        "usage: {NAME} <path/to/cordis.yml> (or set {CONFIG_ENV}=<path>, which wins); the config is required — there is no built-in fallback"
    )
}

/// Builds the frozen expression environment and compiled plugin catalog.
///
/// # Errors
///
/// Returns dotenv, home-path, executable, or catalog-registration failures.
pub fn catalog(
    cwd: &Path,
    inherited: &BTreeMap<String, String>,
    bare_module_base: Option<&Path>,
) -> anyhow::Result<PluginCatalog> {
    let warning: seekdeep_app_boot::EnvironmentWarning = std::sync::Arc::new(|message: String| {
        let _ = writeln!(std::io::stderr(), "{message}");
    });
    let environment = load_layered_env(NAME, cwd, inherited, Some(&warning))?;
    let configured_home = environment.get(SEEKDEEP_HOME_ENV).map(|entry| entry.value);
    let seekdeep_home = resolve_process_seekdeep_home(configured_home.as_deref().map(OsStr::new))?;
    let platform = match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    };
    let expressions = ExpressionEnvironment::from_launch_environment(
        &environment,
        cwd.to_owned(),
        std::env::current_exe()?,
        platform,
        env!("CARGO_PKG_VERSION"),
        seekdeep_home,
    );
    let mut catalog = PluginCatalog::new().with_expression_environment(expressions);
    register_plugins(&catalog)?;
    if let Some(base) = bare_module_base {
        catalog = catalog.with_bare_module_base(base);
    }
    Ok(catalog)
}

fn register_plugins(catalog: &PluginCatalog) -> anyhow::Result<()> {
    register(
        catalog,
        "seekdeep-sdk-jsonrpc-server",
        seekdeep_sdk_server::deferred_plugin(),
    )?;
    register(
        catalog,
        "seekdeep-llm-deepseek",
        seekdeep_llm_deepseek::plugin(),
    )?;
    register(
        catalog,
        "seekdeep-llm-replay",
        seekdeep_llm_replay::plugin(),
    )?;
    register(
        catalog,
        "seekdeep-subprocess-local",
        seekdeep_subprocess_local::plugin(),
    )?;
    register(
        catalog,
        "seekdeep-bash-local",
        seekdeep_bash_local::plugin(),
    )?;
    register(
        catalog,
        "seekdeep-agent-spine-demo",
        seekdeep_agent_spine_demo::plugin(),
    )?;
    register(
        catalog,
        "seekdeep-session-persistence-jsonl",
        seekdeep_session_persistence_jsonl::plugin(),
    )?;
    register(
        catalog,
        "seekdeep-session-checkpoint-policy",
        seekdeep_session_checkpoint_policy::plugin(),
    )?;
    register(catalog, "seekdeep-subagent", seekdeep_subagent::plugin())?;
    register(
        catalog,
        "seekdeep-subagent-spawn-in-process",
        seekdeep_subagent_spawn_in_process::plugin(),
    )?;
    register(
        catalog,
        "seekdeep-tool-subagent",
        seekdeep_tool_subagent::plugin(),
    )?;
    register(catalog, "seekdeep-tool-todo", seekdeep_tool_todo::plugin())?;
    register(catalog, "seekdeep-fs-local", seekdeep_fs_local::plugin())?;
    register(
        catalog,
        "seekdeep-fs-observation-policy",
        seekdeep_fs_observation_policy::plugin(),
    )?;
    register(catalog, "seekdeep-tool-fs", seekdeep_tool_fs::plugin())?;
    register(
        catalog,
        "seekdeep-token-meter",
        seekdeep_token_meter::plugin(),
    )?;
    register(
        catalog,
        "seekdeep-compaction-basic",
        seekdeep_compaction_basic::plugin(),
    )?;
    register(
        catalog,
        "seekdeep-sandbox-local",
        seekdeep_sandbox_local::plugin(),
    )?;
    register(
        catalog,
        "seekdeep-sandbox-policy",
        seekdeep_sandbox_policy::plugin(),
    )?;
    register(catalog, "seekdeep-terminal", seekdeep_terminal::plugin())?;
    register(
        catalog,
        "seekdeep-terminal-bash",
        seekdeep_terminal_bash::plugin(),
    )?;
    register(
        catalog,
        "seekdeep-tool-bash-persistent",
        seekdeep_tool_bash_persistent::plugin(),
    )?;
    register(
        catalog,
        "seekdeep-tool-str-replace-editor",
        seekdeep_tool_str_replace_editor::plugin(),
    )?;
    Ok(())
}

fn register(catalog: &PluginCatalog, name: &str, plugin: Plugin) -> anyhow::Result<()> {
    catalog.register_named(name, plugin.clone())?;
    catalog.register_named(&format!("@seekdeep-ai/{name}"), plugin)?;
    Ok(())
}

/// Boots the selected external configuration transactionally.
///
/// # Errors
///
/// Returns selection, environment, import, activation, or rollback failures.
pub async fn boot_selected(
    environment: &BTreeMap<String, String>,
    arguments: &[String],
    cwd: &Path,
    bare_module_base: Option<&Path>,
) -> anyhow::Result<BootedApplication> {
    let path = selected_config_path(environment, arguments, cwd)?;
    let catalog = catalog(cwd, environment, bare_module_base)?;
    boot(NAME, &path, &catalog, BootOptions::default()).await
}

/// Runs the process launcher and returns its selected exit status.
pub fn process_main(packaged: bool) -> ExitCode {
    install_panic_reporter();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "{NAME}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(process_main_async(packaged));
    match result {
        Ok(code) => ExitCode::from(u8::try_from(code.clamp(0, 255)).unwrap_or(1)),
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "{error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn process_main_async(packaged: bool) -> anyhow::Result<i32> {
    let cwd = std::env::current_dir()?;
    let environment = std::env::vars().collect::<BTreeMap<_, _>>();
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let bare_module_base = packaged
        .then(std::env::current_exe)
        .transpose()?
        .and_then(|path| path.parent().map(Path::to_owned));
    let application =
        boot_selected(&environment, &arguments, &cwd, bare_module_base.as_deref()).await?;
    if let Some(server) = application
        .context()
        .get(seekdeep_sdk_server::SDK_JSONRPC_SERVER)
    {
        server.mark_ready();
    }
    let server_owns_stdin = application
        .context()
        .get(seekdeep_sdk_server::SDK_JSONRPC_SERVER)
        .is_some();
    let exit_code = wait_for_shutdown(server_owns_stdin).await?;
    application.dispose().await?;
    Ok(exit_code)
}

#[cfg(unix)]
async fn wait_for_shutdown(server_owns_stdin: bool) -> anyhow::Result<i32> {
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    if server_owns_stdin {
        return tokio::select! {
            _ = terminate.recv() => Ok(0),
            _ = interrupt.recv() => Ok(130),
        };
    }
    let mut input = tokio::io::stdin();
    let mut discarded = Vec::new();
    tokio::select! {
        result = input.read_to_end(&mut discarded) => result.map(|_| 0).map_err(anyhow::Error::from),
        _ = terminate.recv() => Ok(0),
        _ = interrupt.recv() => Ok(130),
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown(server_owns_stdin: bool) -> anyhow::Result<i32> {
    if server_owns_stdin {
        tokio::signal::ctrl_c().await?;
        return Ok(130);
    }
    let mut input = tokio::io::stdin();
    let mut discarded = Vec::new();
    tokio::select! {
        result = input.read_to_end(&mut discarded) => result.map(|_| 0).map_err(anyhow::Error::from),
        result = tokio::signal::ctrl_c() => result.map(|()| 130).map_err(anyhow::Error::from),
    }
}

fn install_panic_reporter() {
    std::panic::set_hook(Box::new(|panic| {
        let _ = writeln!(std::io::stderr(), "{NAME}: unhandled failure: {panic}");
    }));
}
