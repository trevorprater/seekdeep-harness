//! Prerequisite spine activation parity.

use std::sync::Arc;

use seekdeep_agent_loop::{AgentLoop, AgentLoopServices, DEFAULT_MAX_PARALLEL_TOOL_CALLS};
use seekdeep_cordis::Context;
use seekdeep_system_prompt::{AssembleContext, SystemPromptConfig, render_prompt};
use seekdeep_tools::{ToolPresentationMode, ToolRuntimeConfig};

#[tokio::test]
async fn mounts_configurable_prerequisites_that_can_activate_agent_loop() {
    let context = Context::new();
    let dependencies = seekdeep_agent_loop_testkit::mount_agent_loop_test_dependencies(
        &context,
        seekdeep_agent_loop_testkit::AgentLoopTestDependenciesOptions {
            system_prompt: SystemPromptConfig {
                persona: "Test persona.".to_owned(),
                ..SystemPromptConfig::default()
            },
            tools: ToolRuntimeConfig {
                mode: ToolPresentationMode::Native,
                ..ToolRuntimeConfig::default()
            },
        },
    )
    .unwrap();
    let prompt = dependencies
        .system_prompt
        .assemble(AssembleContext::default())
        .await
        .unwrap();
    assert!(render_prompt(&prompt).unwrap().contains("Test persona."));

    let agent_loop = AgentLoop::new(
        context.clone(),
        dependencies.sessions.clone(),
        (*dependencies.agents).clone(),
        AgentLoopServices {
            llm: dependencies.llm.clone(),
            system_prompt: dependencies.system_prompt.clone(),
            tools: dependencies.tools.clone(),
            max_parallel_tool_calls: DEFAULT_MAX_PARALLEL_TOOL_CALLS,
        },
    )
    .unwrap();
    let registration = dependencies
        .agents
        .set_factory(Arc::new(agent_loop.clone()))
        .unwrap();
    registration.dispose().await.unwrap();
    agent_loop.dispose().await.unwrap();
    context.fiber().restart().await.unwrap();
}
