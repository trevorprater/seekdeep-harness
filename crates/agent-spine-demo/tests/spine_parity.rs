//! Core composition, optional stacks, config forwarding, and Loader shape.

use std::path::PathBuf;

use seekdeep_agent_spine_demo::{
    Config, ConfiguredAgent, GoalConfig, OptionalFeature, SkillConfig, apply, plugin,
};
use seekdeep_cordis::Context;
use seekdeep_tools::{ToolDefinition, ToolOutputDefinition, assert_supported_json_schema};
use serde_json::json;
use std::sync::Arc;

fn minimal() -> Config {
    serde_json::from_value(json!({
        "workspaceContext":false,
        "skills":{"enabled":false},
        "toolBash":false,
        "toolJobs":false
    }))
    .unwrap()
}

#[tokio::test]
async fn minimal_spine_publishes_core_services_without_agents_or_optional_tools() {
    let context = Context::new();
    let runtime = apply(&context, minimal()).await.unwrap();
    assert!(context.get(seekdeep_llm::LLM).is_some());
    assert!(
        context
            .get(seekdeep_core::session_store::SESSIONS)
            .is_some()
    );
    assert!(context.get(seekdeep_tools::TOOLS).is_some());
    assert!(context.get(seekdeep_agent::AGENTS).is_some());
    assert!(context.get(seekdeep_agent_loop::AGENT_LOOP).is_some());
    assert!(context.get(seekdeep_jobs::JOBS).is_some());
    assert!(context.get(seekdeep_invariants::INVARIANTS).is_some());
    let invariants = context.get(seekdeep_invariants::INVARIANTS).unwrap();
    for package in [
        "@seekdeep-ai/seekdeep-session",
        "@seekdeep-ai/seekdeep-agent",
        "@seekdeep-ai/seekdeep-scope",
        "@seekdeep-ai/seekdeep-agent-loop",
    ] {
        assert!(invariants.is_registered(package));
    }
    assert!(runtime.agents.list().is_empty());
    assert!(runtime.tools.get("skill", None).is_none());
    assert!(runtime.tools.get("get_goal", None).is_none());
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn goals_parallelism_and_exact_configured_agent_identity_are_forwarded() {
    let context = Context::new();
    let mut config = minimal();
    config.max_parallel_tool_calls = Some(3);
    config.goals = Some(OptionalFeature::Config(GoalConfig::default()));
    config.agents = vec![ConfiguredAgent {
        id: "primary".to_owned(),
        session_id: Some("exact-primary".to_owned()),
        provider: Some("mock".to_owned()),
        model: Some("model".to_owned()),
        cwd: Some("/tmp".to_owned()),
        ..ConfiguredAgent::default()
    }];
    let runtime = apply(&context, config).await.unwrap();
    assert_eq!(runtime.agent_loop.max_parallel_tool_calls(), 3);
    assert!(
        runtime
            .agents
            .get(&seekdeep_core::session::SessionId::new("exact-primary"))
            .is_some()
    );
    assert!(runtime.tools.get("get_goal", None).is_some());
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn skill_home_mismatch_fails_before_mount_and_matching_home_loads_empty_catalog() {
    let root = tempfile::tempdir().unwrap();
    let explicit = root.path().join("explicit");
    let nested = root.path().join("nested");
    let context = Context::new();
    let mut mismatch = minimal();
    mismatch.seekdeep_home = Some(explicit.to_string_lossy().into_owned());
    mismatch.skills = Some(SkillConfig {
        enabled: Some(true),
        filesystem: Some(seekdeep_skill_filesystem::Config {
            seekdeep_home: Some(nested),
            watch: false,
            ..seekdeep_skill_filesystem::Config::default()
        }),
        ..SkillConfig::default()
    });
    assert!(
        apply(&context, mismatch)
            .await
            .unwrap_err()
            .to_string()
            .contains("must resolve to the same")
    );
    context.fiber().dispose().await.unwrap();

    let context = Context::new();
    let mut matching = minimal();
    matching.seekdeep_home = Some(explicit.to_string_lossy().into_owned());
    matching.skills = Some(SkillConfig {
        enabled: Some(true),
        filesystem: Some(seekdeep_skill_filesystem::Config {
            seekdeep_home: Some(PathBuf::from(&explicit)),
            agents_home: Some(root.path().join("agents")),
            watch: false,
            ..seekdeep_skill_filesystem::Config::default()
        }),
        ..SkillConfig::default()
    });
    let runtime = apply(&context, matching).await.unwrap();
    assert!(context.get(seekdeep_skill::SKILLS).is_some());
    assert!(runtime.tools.get("skill", None).is_some());
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn tool_order_and_model_facing_optional_tools_are_forwarded() {
    let context = Context::new();
    let mut config = minimal();
    config.tool_order = Some(vec![
        "zulu".to_owned(),
        seekdeep_system_prompt::TOOL_ORDER_REST.to_owned(),
    ]);
    let runtime = apply(&context, config).await.unwrap();
    for name in ["alpha", "zulu"] {
        runtime
            .tools
            .register(
                &context,
                ToolDefinition::new(
                    name,
                    name,
                    serde_json::Map::new(),
                    ToolOutputDefinition::new(
                        Arc::new(assert_supported_json_schema(json!({})).unwrap()),
                        Arc::new(|_, _| Ok(Vec::new())),
                    ),
                    Arc::new(|_, _| Box::pin(async { Ok(serde_json::Value::Null) })),
                ),
            )
            .unwrap();
    }
    let assembly = context
        .get(seekdeep_system_prompt::SYSTEM_PROMPT)
        .unwrap()
        .assemble(seekdeep_system_prompt::AssembleContext::default())
        .await
        .unwrap();
    assert_eq!(
        assembly
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["zulu", "alpha"]
    );
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn namespace_shape_and_required_workspace_config_fail_closed() {
    let definition = plugin();
    assert_eq!(definition.name(), "agent-spine-demo");
    assert!(definition.inject().is_empty());
    assert!(serde_json::from_value::<Config>(json!({})).is_err());
    let context = Context::new();
    let fiber = context
        .plugin(definition, json!({"workspaceContext":true}))
        .unwrap();
    assert!(fiber.await_settled().await.is_err());
}
