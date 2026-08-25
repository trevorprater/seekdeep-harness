//! MCP client bridge: external server tools become native `SeekDeep` tools.

mod config;
mod connection;
mod protocol;
mod tools;
mod transport;

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, LazyLock},
};

use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use serde_json::Value;

pub use config::{Config, ReconnectConfig, ResolvedReconnectPolicy, resolve_reconnect_policy};
pub use connection::{
    ConnectionHandle, ConnectionRuntime, McpTiming, TokioMcpTiming, start_connection,
};
pub use protocol::{
    McpClient, McpClientFactory, McpClientSignals, McpTool, McpToolExecution, McpToolPage,
    normalize_tool_schemas,
};
pub use tools::{
    RegistrationFailure, ToolBridgeOptions, ToolDisposers, dispose_generation, extract_text,
    public_tool_name, sync_tools,
};
pub use transport::NativeMcpClientFactory;

/// Loader plugin name.
pub const NAME: &str = "mcp-client";
/// Source-compatible declared dependency surface.
pub const INJECT: &[&str] = &["tools"];

static ACTIVE_SERVER_NAMES: LazyLock<Mutex<HashMap<usize, HashSet<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Applies one MCP server using explicit client and timing dependencies.
///
/// # Errors
///
/// Returns missing tools, invalid config, duplicate namespace, strict startup,
/// or effect-ownership failures.
pub async fn apply_with_runtime(
    context: &Context,
    config: Config,
    runtime: ConnectionRuntime,
) -> anyhow::Result<Arc<ConnectionHandle>> {
    anyhow::ensure!(
        context.get(seekdeep_tools::TOOLS).is_some(),
        "mcp-client requires tools"
    );
    let policy = config.validate()?;
    let root_key = Arc::as_ptr(context.root_fiber()) as usize;
    let server_name = config.server_name().to_owned();
    {
        let mut roots = ACTIVE_SERVER_NAMES.lock();
        let names = roots.entry(root_key).or_default();
        anyhow::ensure!(
            names.insert(server_name.clone()),
            "mcp-client: serverName {server_name:?} is already in use by another mcp-client instance — pick a unique serverName in cordis.yml"
        );
    }
    let reserved_name = server_name.clone();
    let reservation = EffectHandle::synchronous("mcp-client.serverName", move || {
        let mut roots = ACTIVE_SERVER_NAMES.lock();
        if let Some(names) = roots.get_mut(&root_key) {
            names.remove(&reserved_name);
            if names.is_empty() {
                roots.remove(&root_key);
            }
        }
        Ok(())
    });
    if let Err(error) = context.own(reservation.clone()) {
        let _ = reservation.dispose().await;
        return Err(error.into());
    }

    let connection = start_connection(context, config.clone(), policy, runtime);
    let cleanup = Arc::clone(&connection);
    let connection_effect = EffectHandle::new("mcp-client.connection", move || {
        Box::pin(async move { cleanup.dispose().await })
    });
    if let Err(error) = context.own(connection_effect.clone()) {
        let _ = connection.dispose().await;
        let _ = reservation.dispose().await;
        return Err(error.into());
    }
    if let Some(error) = connection.initial_error().await
        && config.fail_on_startup_error()
    {
        return Err(anyhow::Error::msg(error).context(format!(
            "mcp-client({server_name}): initial connection or tool synchronization failed"
        )));
    }
    Ok(connection)
}

/// Applies one MCP server with native Rust transports and Tokio policy time.
///
/// # Errors
///
/// Returns the same failures as [`apply_with_runtime`].
pub async fn apply(context: &Context, config: Config) -> anyhow::Result<Arc<ConnectionHandle>> {
    apply_with_runtime(
        context,
        config,
        ConnectionRuntime {
            factory: Arc::new(NativeMcpClientFactory),
            timing: Arc::new(TokioMcpTiming::default()),
        },
    )
    .await
}

fn normalize_config(value: &Value) -> anyhow::Result<Value> {
    let config = serde_json::from_value::<Config>(value.clone())?.normalized()?;
    Ok(serde_json::to_value(config)?)
}

/// Builds the Loader-compatible namespace plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config = serde_json::from_value::<Config>(config)?;
            apply(&context, config).await?;
            Ok(())
        })
    })
    .with_config_validator(normalize_config)
}
