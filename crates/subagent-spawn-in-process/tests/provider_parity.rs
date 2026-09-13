//! Provider metadata, capability, default config, and lifecycle parity.

use seekdeep_cordis::Context;
use seekdeep_subagent::SubagentRuntime;
use seekdeep_subagent_spawn_in_process::{INJECT, NAME, plugin};
use serde_json::Value;

#[tokio::test]
async fn loader_registers_the_default_fresh_provider_and_disposal_unregisters_it() {
    let context = Context::new();
    let subagents = SubagentRuntime::new(&context);
    subagents.provide(&context).unwrap();
    let definition = plugin();
    assert_eq!(definition.name(), NAME);
    assert_eq!(definition.inject(), INJECT);
    let mounted = context.plugin(definition, Value::Null).unwrap();
    mounted.await_settled().await.unwrap();

    let provider = subagents.get_provider("spawn").unwrap();
    assert_eq!(provider.name(), "spawn");
    assert!(!provider.inherits_parent_context());
    assert!(provider.supports_continuable());
    assert_eq!(
        *provider.capabilities(),
        seekdeep_subagent::SubagentCapabilities {
            output_schema: true,
            depth_limit: true,
            tool_filter: true,
            persona: true,
        }
    );

    mounted.dispose().await.unwrap();
    assert!(subagents.get_provider("spawn").is_none());
}

#[tokio::test]
async fn loader_honors_a_configured_provider_name() {
    let context = Context::new();
    let subagents = SubagentRuntime::new(&context);
    subagents.provide(&context).unwrap();
    let mounted = context
        .plugin(plugin(), serde_json::json!({ "providerName": "local" }))
        .unwrap();
    mounted.await_settled().await.unwrap();
    assert!(subagents.get_provider("spawn").is_none());
    assert!(subagents.get_provider("local").is_some());
}
