//! The Web question feature's Host half must not grant a model-facing tool.

#![cfg(not(target_arch = "wasm32"))]

use seekdeep_client_ui_user_questions::{NAME, host_plugin};
use seekdeep_cordis::{Context, FiberState};
use seekdeep_tools::TOOLS;
use serde_json::{Value, json};

#[tokio::test]
async fn host_plugin_is_dependency_free_and_effect_free() {
    let context = Context::new();
    let system_prompt = context
        .plugin(seekdeep_system_prompt::plugin(), json!({}))
        .unwrap();
    system_prompt.await_settled().await.unwrap();
    let tools = context.plugin(seekdeep_tools::plugin(), json!({})).unwrap();
    tools.await_settled().await.unwrap();
    let user_questions = context
        .plugin(seekdeep_user_questions::plugin(), Value::Null)
        .unwrap();
    user_questions.await_settled().await.unwrap();

    let plugin = host_plugin();
    assert_eq!(plugin.name(), NAME);
    assert!(plugin.inject().is_empty());
    let fiber = context.plugin(plugin, Value::Null).unwrap();
    fiber.await_settled().await.unwrap();
    assert_eq!(fiber.fiber().state(), FiberState::Active);
    assert!(
        context
            .get(TOOLS)
            .unwrap()
            .get("ask_user_question", None)
            .is_none()
    );

    fiber.dispose().await.unwrap();
    user_questions.dispose().await.unwrap();
    tools.dispose().await.unwrap();
    system_prompt.dispose().await.unwrap();
}
