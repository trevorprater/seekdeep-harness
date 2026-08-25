//! Automation-only Agent Client Protocol bridge and subprocess client.

mod bridge;
mod client;
mod codec;
pub mod types;

use std::sync::Arc;

use seekdeep_cordis::{Context, Plugin, fiber::EffectHandle};
use seekdeep_sdk_protocol::{BoxedJsonRpcInput, BoxedJsonRpcOutput, JsonRpcLineTransport};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use bridge::{AcpBridge, AcpBridgeConfig, AcpContinuableDrainHook};
pub use client::{AcpClient, AcpUpdateObserver};
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
    let transport = JsonRpcLineTransport::from_boxed(runtime.input, runtime.output);
    let bridge = AcpBridge::new(
        context,
        &transport,
        AcpBridgeConfig {
            provider: config.provider,
            model: config.model,
        },
    )?;
    bridge.start();
    let cleanup = Arc::clone(&bridge);
    let effect = EffectHandle::new("acp.connection", move || {
        Box::pin(async move { cleanup.shutdown().await })
    });
    context.own(effect)?;
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
