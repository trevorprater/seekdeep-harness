//! Browser-surface startup flags and Host runtime glue.

pub mod startup;

use std::{path::PathBuf, sync::Arc};

use indexmap::IndexMap;
use path_clean::PathClean as _;
use seekdeep_cordis::{Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_host_webserver::WEB_SERVER;
use seekdeep_shell_env::{
    SHELL_ENV, ShellEnvContributor, ShellEnvResolvedValues, ShellEnvVariable,
};
use seekdeep_system_prompt::{PromptSection, PromptText, SYSTEM_PROMPT};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Loader plugin identity.
pub const NAME: &str = "web-app";
/// Runtime glue needs the bound server, prompt, and shell environment registries.
pub const INJECT: &[&str] = &["webServer", "systemPrompt", "shellEnv"];
/// Bind-dependent runtime service name.
pub const WEB_RUNTIME_SERVICE: &str = "webRuntime";
/// Typed JSON seat used by downstream config expressions.
pub const WEB_RUNTIME: ServiceKey<Value> = ServiceKey::new(WEB_RUNTIME_SERVICE);
/// Canonical local Web URL variable.
pub const SEEKDEEP_WEB_URL: &str = "SEEKDEEP_WEB_URL";

/// Web runtime glue configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Print the readiness URL after Loader settlement.
    pub print_url: bool,
    /// Add model-facing Web surface context.
    pub surface_context: bool,
    /// Explicit trusted Host authorities.
    pub trusted_hosts: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            print_url: true,
            surface_context: true,
            trusted_hosts: Vec::new(),
        }
    }
}

/// Bind-dependent LAN display and trust snapshot.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebRuntimeValues {
    /// Non-internal IPv4 literals sampled at bind time.
    pub lan_addresses: Vec<String>,
    /// LAN literals followed by explicit authorities.
    pub trusted_hosts: Vec<String>,
}

/// Resolves the trust snapshot from already-sampled interface addresses.
#[must_use]
pub fn resolve_lan_trust(
    bind_host: &str,
    extra: &[String],
    interface_addresses: impl IntoIterator<Item = String>,
) -> WebRuntimeValues {
    let lan_addresses = if bind_host == "0.0.0.0" {
        interface_addresses.into_iter().collect()
    } else {
        Vec::new()
    };
    let trusted_hosts = lan_addresses.iter().chain(extra).cloned().collect();
    WebRuntimeValues {
        lan_addresses,
        trusted_hosts,
    }
}

fn source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .clean()
}

fn dist_index() -> anyhow::Result<PathBuf> {
    let index = source_root().join("apps/web/dist/index.html");
    anyhow::ensure!(
        index.is_file(),
        "web-app: frontend dist not built; run pnpm run build from the repository root first"
    );
    Ok(index)
}

fn local_web_url(server: &seekdeep_host_webserver::WebServer) -> String {
    format!("http://127.0.0.1:{}", server.port())
}

fn surface_prompt(url: &str) -> String {
    format!(
        "You are interacting with the user through the SeekDeep Harness Web GUI at {url}. When the user refers to \"this page\", \"this GUI\", or \"this app\" without naming another target, they mean this GUI. The browser provides no implicit DOM, route, or screenshot context. The client-plugin HMR receiver is active, but client-plugin changes reload without a refresh only while `pnpm run dev:web` is also running from this same checkout to rebuild their bundles; verify that watcher before promising automatic updates. Every other change — the apps/web shell and plain packages — requires rebuilding the affected Web artifacts and verifying this existing URL after a page refresh. Starting another server does not update this GUI. The apps/web Vite entry builds the shell but is not a standalone application because only seekdeep web injects window.__SEEKDEEP_BOOT__. Do not start a replacement server unless the user asks; if one is needed, use a managed background job and verify its exact URL."
    )
}

/// Mounts static frontend serving and Web surface context.
///
/// # Errors
///
/// Returns missing-service, missing-build, duplicate-registration, or lifecycle failures.
pub fn install(context: &seekdeep_cordis::Context, config: &Config) -> anyhow::Result<()> {
    install_with_dist_index(context, config, dist_index()?)
}

/// Mounts Web runtime glue over an explicit built frontend index.
///
/// # Errors
///
/// Returns missing-service, invalid-index, duplicate-registration, or lifecycle failures.
pub fn install_with_dist_index(
    context: &seekdeep_cordis::Context,
    config: &Config,
    dist_index: PathBuf,
) -> anyhow::Result<()> {
    let server = context
        .get(WEB_SERVER)
        .ok_or_else(|| anyhow::anyhow!("web-app requires webServer"))?;
    let runtime = resolve_lan_trust(server.host().as_str(), &config.trusted_hosts, Vec::new());
    context.provide(WEB_RUNTIME, Arc::new(serde_json::to_value(&runtime)?))?;
    seekdeep_host_frontend_static::install(
        context,
        seekdeep_host_frontend_static::FrontendStaticConfig { dist_index },
    )?;
    let url = local_web_url(&server);
    if config.surface_context {
        seekdeep_app_boot::add_harness_source_section(context, &source_root())?;
        let prompt = context
            .get(SYSTEM_PROMPT)
            .ok_or_else(|| anyhow::anyhow!("web-app requires systemPrompt"))?;
        let prompt_url = url.clone();
        prompt.section(
            context,
            PromptSection::new(
                "app:web-surface",
                -98.0,
                PromptText::Dynamic(Arc::new(move |_| Ok(surface_prompt(&prompt_url)))),
            ),
        )?;
        let shell = context
            .get(SHELL_ENV)
            .ok_or_else(|| anyhow::anyhow!("web-app requires shellEnv"))?;
        let shell_url = url.clone();
        shell.register(
            context,
            ShellEnvContributor {
                name: "web-runtime".to_owned(),
                variables: IndexMap::from([(
                    SEEKDEEP_WEB_URL.to_owned(),
                    ShellEnvVariable {
                        description: "Canonical local URL of the SeekDeep Harness Web GUI serving this session.".to_owned(),
                    },
                )]),
                resolve: Arc::new(move |_| {
                    Ok(ShellEnvResolvedValues::from([(
                        SEEKDEEP_WEB_URL.to_owned(),
                        Value::String(shell_url.clone()),
                    )]))
                }),
            },
        )?;
    }
    if config.print_url {
        let context_for_task = context.clone();
        let server_for_task = server;
        let runtime_for_task = runtime;
        let task = tokio::spawn(async move {
            let settled = context_for_task
                .get(seekdeep_loader::LOADER)
                .map(|loader| async move { loader.wait().await });
            let passed = match settled {
                Some(settled) => settled.await.is_ok(),
                None => true,
            };
            if passed && context_for_task.get(WEB_SERVER).is_some() {
                let local = local_web_url(&server_for_task);
                if let Some(lan) = runtime_for_task.lan_addresses.first() {
                    println!(
                        "seekdeep web: {local} (LAN: http://{lan}:{})",
                        server_for_task.port()
                    );
                } else {
                    println!("seekdeep web: {local}");
                }
            }
        });
        context.own(EffectHandle::new("web-app readiness", move || {
            Box::pin(async move {
                task.abort();
                match task.await {
                    Ok(()) => Ok(()),
                    Err(error) if error.is_cancelled() => Ok(()),
                    Err(error) => Err(error.into()),
                }
            })
        }))?;
    }
    Ok(())
}

/// Builds the Loader-compatible Web runtime glue plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            install(&context, &serde_json::from_value(config)?)?;
            Ok(())
        })
    })
}
