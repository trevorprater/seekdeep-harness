//! Empty Host half for the browser-owned Client Runtime package.

#![cfg(not(target_arch = "wasm32"))]

use seekdeep_cordis::{Context, FiberState};
use serde_json::Value;

#[tokio::test]
async fn host_plugin_is_a_dependency_free_no_op_placeholder() {
    let plugin = seekdeep_client_runtime::host_plugin();
    assert_eq!(plugin.name(), "client-runtime");
    assert!(plugin.inject().is_empty());
    let context = Context::new();
    let fiber = context.plugin(plugin, Value::Null).unwrap();
    fiber.await_settled().await.unwrap();
    assert_eq!(fiber.fiber().state(), FiberState::Active);
    fiber.dispose().await.unwrap();
}
