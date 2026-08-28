//! Browser-surface startup flags and Host runtime glue.

pub mod startup;

use std::{net::IpAddr, path::PathBuf, sync::Arc};

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
/// The bound server is mandatory; surface-context registries resolve only when enabled.
pub const INJECT: &[&str] = &["webServer"];
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

fn current_web_url(context: &seekdeep_cordis::Context) -> anyhow::Result<String> {
    let server = context.get(WEB_SERVER).ok_or_else(|| {
        anyhow::anyhow!("web-app: webServer service missing while resolving Web runtime")
    })?;
    Ok(local_web_url(&server))
}

fn interface_ipv4_addresses() -> anyhow::Result<Vec<String>> {
    Ok(if_addrs::get_if_addrs()?
        .into_iter()
        .filter(|interface| !interface.is_loopback())
        .filter_map(|interface| match interface.ip() {
            IpAddr::V4(address) => Some(address.to_string()),
            IpAddr::V6(_) => None,
        })
        .collect())
}

fn interface_addresses_for_context(
    context: &seekdeep_cordis::Context,
) -> anyhow::Result<Vec<String>> {
    let server = context
        .get(WEB_SERVER)
        .ok_or_else(|| anyhow::anyhow!("web-app requires webServer"))?;
    if server.host() == seekdeep_host_webserver::ListenHost::AllInterfaces {
        interface_ipv4_addresses()
    } else {
        Ok(Vec::new())
    }
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
    install_with_runtime_seams(
        context,
        config,
        dist_index()?,
        interface_addresses_for_context(context)?,
        Arc::new(|line| println!("{line}")),
    )
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
    install_with_runtime_seams(
        context,
        config,
        dist_index,
        interface_addresses_for_context(context)?,
        Arc::new(|line| println!("{line}")),
    )
}

/// Mounts Web runtime glue with deterministic native-boundary inputs.
///
/// # Errors
///
/// Returns missing-service, invalid-index, duplicate-registration, or lifecycle failures.
#[doc(hidden)]
pub fn install_with_runtime_seams(
    context: &seekdeep_cordis::Context,
    config: &Config,
    dist_index: PathBuf,
    interface_addresses: Vec<String>,
    print_readiness: Arc<dyn Fn(String) + Send + Sync>,
) -> anyhow::Result<()> {
    let server = context
        .get(WEB_SERVER)
        .ok_or_else(|| anyhow::anyhow!("web-app requires webServer"))?;
    let runtime = resolve_lan_trust(
        server.host().as_str(),
        &config.trusted_hosts,
        interface_addresses,
    );
    context.provide(WEB_RUNTIME, Arc::new(serde_json::to_value(&runtime)?))?;
    seekdeep_host_frontend_static::install(
        context,
        seekdeep_host_frontend_static::FrontendStaticConfig { dist_index },
    )?;
    if config.surface_context {
        seekdeep_app_boot::add_harness_source_section(context, &source_root())?;
        let prompt = context
            .get(SYSTEM_PROMPT)
            .ok_or_else(|| anyhow::anyhow!("web-app requires systemPrompt"))?;
        let prompt_context = context.clone();
        prompt.section(
            context,
            PromptSection::new(
                "app:web-surface",
                -98.0,
                PromptText::Dynamic(Arc::new(move |_| {
                    Ok(surface_prompt(&current_web_url(&prompt_context)?))
                })),
            ),
        )?;
        let shell = context
            .get(SHELL_ENV)
            .ok_or_else(|| anyhow::anyhow!("web-app requires shellEnv"))?;
        let shell_context = context.clone();
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
                        Value::String(current_web_url(&shell_context)?),
                    )]))
                }),
            },
        )?;
    }
    if config.print_url {
        if let Some(loader) = context.get(seekdeep_loader::LOADER) {
            let context_for_task = context.clone();
            let server_for_task = server;
            let runtime_for_task = runtime;
            let readiness_for_task = print_readiness;
            let task = tokio::spawn(publish_readiness_after(
                async move { loader.wait().await },
                context_for_task,
                server_for_task,
                runtime_for_task,
                readiness_for_task,
            ));
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
        } else if context.get(WEB_SERVER).is_some() {
            print_readiness(readiness_line(&server, &runtime));
        }
    }
    Ok(())
}

async fn publish_readiness_after<F>(
    settlement: F,
    context: seekdeep_cordis::Context,
    server: Arc<seekdeep_host_webserver::WebServer>,
    runtime: WebRuntimeValues,
    print_readiness: Arc<dyn Fn(String) + Send + Sync>,
) where
    F: std::future::Future<Output = anyhow::Result<()>> + Send,
{
    if settlement.await.is_ok() && context.get(WEB_SERVER).is_some() {
        print_readiness(readiness_line(&server, &runtime));
    }
}

fn readiness_line(
    server: &seekdeep_host_webserver::WebServer,
    runtime: &WebRuntimeValues,
) -> String {
    let local = local_web_url(server);
    runtime.lan_addresses.first().map_or_else(
        || format!("seekdeep web: {local}"),
        |lan| {
            format!(
                "seekdeep web: {local} (LAN: http://{lan}:{})",
                server.port()
            )
        },
    )
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

#[cfg(test)]
mod readiness_tests {
    use std::sync::Mutex;

    use super::*;
    use seekdeep_host_webserver::{ListenHost, WebServer, WebServerConfig};

    #[tokio::test]
    async fn readiness_waits_for_success_and_stays_quiet_on_failure() -> anyhow::Result<()> {
        let missing = seekdeep_cordis::Context::new();
        assert!(
            current_web_url(&missing)
                .unwrap_err()
                .to_string()
                .contains("webServer service missing")
        );
        for address in interface_ipv4_addresses()? {
            let address = address.parse::<std::net::Ipv4Addr>()?;
            assert!(!address.is_loopback());
        }
        let context = seekdeep_cordis::Context::new();
        let server = WebServer::install(
            &context,
            WebServerConfig {
                host: ListenHost::Loopback,
                port: 0,
            },
        )
        .await?;
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink_lines = lines.clone();
        let sink: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |line| {
            sink_lines.lock().unwrap().push(line);
        });
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(publish_readiness_after(
            async move {
                wait.await?;
                Ok(())
            },
            context.clone(),
            server.clone(),
            WebRuntimeValues::default(),
            sink.clone(),
        ));
        tokio::task::yield_now().await;
        assert!(lines.lock().unwrap().is_empty());
        release.send(()).unwrap();
        task.await?;
        assert_eq!(
            lines.lock().unwrap().as_slice(),
            [format!("seekdeep web: http://127.0.0.1:{}", server.port())]
        );

        lines.lock().unwrap().clear();
        publish_readiness_after(
            async { anyhow::bail!("boot failed") },
            context.clone(),
            server,
            WebRuntimeValues::default(),
            sink,
        )
        .await;
        assert!(lines.lock().unwrap().is_empty());
        context.fiber().dispose().await?;
        Ok(())
    }
}
