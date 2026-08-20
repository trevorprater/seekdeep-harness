//! Behavioral parity tests for the model-facing goal control tools.

use std::sync::Arc;

use seekdeep_agent::AgentRegistry;
use seekdeep_cordis::Context;
use seekdeep_goal::{Config as GoalConfig, GoalService};
use seekdeep_system_prompt::{SystemPromptConfig, install as install_system_prompt};
use seekdeep_tool_goal::wrapup::render_wrapup_context;
use seekdeep_tool_goal::{Config, apply, config_schema};
use seekdeep_tools::{ToolPresentationMode, ToolRuntime, ToolRuntimeConfig};
use serde_json::json;

fn default_tool_config() -> ToolRuntimeConfig {
    ToolRuntimeConfig {
        mode: ToolPresentationMode::Native,
        ..ToolRuntimeConfig::default()
    }
}

fn harness(config: &Config) -> (Context, Arc<ToolRuntime>) {
    let ctx = Context::new();
    let agents = Arc::new(AgentRegistry::new(ctx.clone()));
    agents.provide(&ctx).expect("agents");
    GoalService::install(&ctx, GoalConfig::default()).expect("goals");
    let system_prompt = install_system_prompt(&ctx, SystemPromptConfig::default()).expect("prompt");
    let tools = ToolRuntime::new_with_system_prompt(&ctx, &system_prompt, default_tool_config())
        .expect("tools runtime");
    tools.provide(&ctx).expect("tools provide");
    apply(&ctx, config).expect("tool-goal apply");
    (ctx, tools)
}

#[test]
fn config_schema_rejects_invalid_and_fills_default() {
    let schema = config_schema();
    for invalid in [
        json!({ "blockedAfterConsecutiveRounds": 0 }),
        json!({ "blockedAfterConsecutiveRounds": 1.5 }),
    ] {
        assert!(schema.resolve(&invalid).is_err(), "{invalid} must reject");
    }
    let defaulted = schema.resolve(&json!({})).expect("defaults");
    assert_eq!(
        defaulted["blockedAfterConsecutiveRounds"].as_f64(),
        Some(3.0)
    );
}

#[test]
fn apply_rejects_fractional_blocked_after() {
    let ctx = Context::new();
    let config = Config {
        blocked_after_consecutive_rounds: Some(1.5),
    };
    let error = apply(&ctx, &config).expect_err("must reject");
    assert!(
        error
            .to_string()
            .contains("blockedAfterConsecutiveRounds must be a positive safe integer"),
        "{error}"
    );
}

#[test]
fn wrapup_renders_complete_and_blocked_envelopes() {
    let complete = render_wrapup_context("build the thing", None);
    assert_eq!(complete.len(), 1);
    let seekdeep_llm::ContentBlock::Text { text } = &complete[0] else {
        panic!("expected text block");
    };
    assert!(text.contains("<goal_complete>"), "{text}");
    assert!(text.contains("Objective: \"build the thing\""), "{text}");

    let blocked = render_wrapup_context("build the thing", Some("no internet"));
    let seekdeep_llm::ContentBlock::Text { text } = &blocked[0] else {
        panic!("expected text block");
    };
    assert!(text.contains("<goal_blocked>"), "{text}");
    assert!(text.contains("Blocked: \"no internet\""), "{text}");
}

#[test]
fn registers_three_goal_tools() {
    let (_ctx, tools) = harness(&Config::default());
    for name in ["get_goal", "create_goal", "update_goal"] {
        assert!(
            tools.get(name, None).is_some(),
            "tool {name:?} must be registered"
        );
    }
}
