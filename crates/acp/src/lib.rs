//! Automation-only Agent Client Protocol bridge and subprocess client.

mod bridge;
mod client;
mod codec;
pub mod types;

use std::sync::Arc;

use seekdeep_cordis::{Context, Plugin, ServiceKey, fiber::EffectHandle};
use seekdeep_sdk_protocol::{BoxedJsonRpcInput, BoxedJsonRpcOutput, JsonRpcLineTransport};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use bridge::{AcpBridge, AcpBridgeConfig, AcpContinuableDrainHook};
pub use client::{AcpClient, AcpPermissionHandler, AcpUpdateObserver};
pub use codec::{
    acp_content_text, acp_prompt_to_text, acp_stop_reason, prompt_has_unsupported_content,
    to_acp_prompt, turn_end_to_stop_reason,
};
pub use types::{
    AcpSessionId, AcpSessionUpdate, AcpStopReason, PROTOCOL_VERSION, PermissionPolicy,
};

/// Loader plugin name.
pub const NAME: &str = "acp";
/// The bridge requires the shared agent factory.
pub const INJECT: &[&str] = &["agents"];
/// Exact live bridge marker for process runners and assembled apps.
pub const ACP_BRIDGE: ServiceKey<AcpBridge> = ServiceKey::new("acpBridge");

/// Provider/model selection for each ACP-created agent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Provider route.
    pub provider: Option<String>,
    /// Model id.
    pub model: Option<String>,
}

/// Runtime-only stream hooks.
pub struct AcpRuntime {
    /// Incoming frames.
    pub input: BoxedJsonRpcInput,
    /// Outgoing frames.
    pub output: BoxedJsonRpcOutput,
}

/// Applies the ACP bridge over explicit streams.
///
/// # Errors
///
/// Returns server construction, listener, or effect ownership failures.
pub fn apply_with_runtime(
    context: &Context,
    config: Config,
    runtime: AcpRuntime,
) -> anyhow::Result<Arc<AcpBridge>> {
    apply_with_runtime_inner(context, config, runtime, None, true)
}

/// Applies the ACP bridge with an exact assembled Agent registry.
#[doc(hidden)]
pub fn apply_with_runtime_and_agents(
    context: &Context,
    config: Config,
    runtime: AcpRuntime,
    agents: Arc<seekdeep_agent::AgentRegistry>,
) -> anyhow::Result<Arc<AcpBridge>> {
    apply_with_runtime_inner(context, config, runtime, Some(agents), true)
}

/// Prepares an ACP bridge whose input reader starts only when [`AcpBridge::start`] is called.
///
/// Application loaders use this form so plugins depending on services published
/// by the bridge's owning composition can finish activation before client frames
/// are accepted.
#[doc(hidden)]
pub fn apply_with_runtime_and_agents_deferred(
    context: &Context,
    config: Config,
    runtime: AcpRuntime,
    agents: Arc<seekdeep_agent::AgentRegistry>,
) -> anyhow::Result<Arc<AcpBridge>> {
    apply_with_runtime_inner(context, config, runtime, Some(agents), false)
}

fn apply_with_runtime_inner(
    context: &Context,
    config: Config,
    runtime: AcpRuntime,
    agents: Option<Arc<seekdeep_agent::AgentRegistry>>,
    start: bool,
) -> anyhow::Result<Arc<AcpBridge>> {
    let transport = JsonRpcLineTransport::from_boxed(runtime.input, runtime.output);
    let config = AcpBridgeConfig {
        provider: config.provider,
        model: config.model,
    };
    let bridge = match agents {
        Some(agents) => AcpBridge::new_with_agents(context, &transport, config, agents)?,
        None => AcpBridge::new(context, &transport, config)?,
    };
    let marker = context.provide(ACP_BRIDGE, bridge.clone())?;
    if start {
        bridge.start();
    }
    let cleanup = Arc::clone(&bridge);
    let effect = EffectHandle::new("acp.connection", move || {
        Box::pin(async move { cleanup.shutdown().await })
    });
    if let Err(error) = context.own(effect) {
        let _ = futures::executor::block_on(marker.dispose());
        return Err(error.into());
    }
    Ok(bridge)
}

/// Applies the production stdio bridge.
///
/// # Errors
///
/// Returns ordinary [`apply_with_runtime`] failures.
pub fn apply(context: &Context, config: Config) -> anyhow::Result<Arc<AcpBridge>> {
    apply_with_runtime(
        context,
        config,
        AcpRuntime {
            input: Box::pin(tokio::io::stdin()),
            output: Box::pin(tokio::io::stdout()),
        },
    )
}

fn normalize_config(value: &Value) -> anyhow::Result<Value> {
    if value.is_null() {
        return Ok(serde_json::to_value(Config::default())?);
    }
    Ok(serde_json::to_value(serde_json::from_value::<Config>(
        value.clone(),
    )?)?)
}

/// Builds the Loader-compatible namespace plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(config)?;
            apply(&context, config)?;
            Ok(())
        })
    })
    .with_config_validator(normalize_config)
}
