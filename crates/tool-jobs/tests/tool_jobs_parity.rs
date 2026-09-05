//! Behavioral parity tests for the model-facing job control tools.

use std::sync::Arc;

use seekdeep_agent::AgentRegistry;
use seekdeep_cordis::Context;
use seekdeep_jobs_local::{Config as JobsConfig, LocalJobRegistry};
use seekdeep_system_prompt::{SystemPromptConfig, install as install_system_prompt};
use seekdeep_tool_jobs::{Config, apply, config_schema, status_line};
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
    let system_prompt = install_system_prompt(&ctx, SystemPromptConfig::default()).expect("prompt");
    let tools = ToolRuntime::new_with_system_prompt(&ctx, &system_prompt, default_tool_config())
        .expect("tools runtime");
    tools.provide(&ctx).expect("tools provide");
    LocalJobRegistry::new(&ctx, JobsConfig::default()).expect("jobs");
    apply(&ctx, config).expect("tool-jobs apply");
    (ctx, tools)
}

#[test]
fn config_schema_rejects_invalid_and_fills_default() {
    let schema = config_schema();
    for invalid in [
        json!({ "waitTimeoutMs": 0 }),
        json!({ "maxWaitTimeoutMs": -1 }),
        json!({ "completionDelivery": "unknown" }),
        json!({ "maxConsecutiveWakes": 0 }),
    ] {
        assert!(schema.resolve(&invalid).is_err(), "{invalid} must reject");
    }
    let defaulted = schema.resolve(&json!({})).expect("defaults");
    assert_eq!(defaulted["waitTimeoutMs"].as_f64(), Some(30_000.0));
    assert_eq!(defaulted["maxWaitTimeoutMs"].as_f64(), Some(600_000.0));
    assert_eq!(defaulted["completionDelivery"], json!("wakeup"));
    assert_eq!(defaulted["maxConsecutiveWakes"].as_f64(), Some(3.0));
}

#[test]
fn apply_rejects_wait_default_exceeding_cap() {
    let ctx = Context::new();
    let config = Config {
        wait_timeout_ms: Some(600_000.0),
        max_wait_timeout_ms: Some(30_000.0),
        ..Config::default()
    };
    let error = apply(&ctx, &config).expect_err("must reject");
    assert!(
        error
            .to_string()
            .contains("waitTimeoutMs (600000) exceeds maxWaitTimeoutMs (30000)"),
        "{error}"
    );
}

#[test]
fn apply_rejects_fractional_wake_budget() {
    let ctx = Context::new();
    let config = Config {
        max_consecutive_wakes: Some(1.5),
        ..Config::default()
    };
    let error = apply(&ctx, &config).expect_err("must reject");
    assert!(
        error
            .to_string()
            .contains("maxConsecutiveWakes (1.5) must be a whole number of turns"),
        "{error}"
    );
}

#[test]
fn status_line_renders_with_and_without_detail() {
    use seekdeep_jobs::JobStatus;
    assert_eq!(status_line(JobStatus::Running, None), "[status: running]");
    assert_eq!(
        status_line(JobStatus::Completed, Some("exit code: 0")),
        "[status: completed, exit code: 0]"
    );
}

#[test]
fn registers_three_tools_and_controller() {
    let (_ctx, tools) = harness(&Config::default());
    for name in ["job_output", "job_list", "job_kill"] {
        assert!(
            tools.get(name, None).is_some(),
            "tool {name:?} must be registered"
        );
    }
}
