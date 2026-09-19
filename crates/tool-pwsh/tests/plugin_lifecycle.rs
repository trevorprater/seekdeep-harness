//! Cordis dependency activation and reversible Pwsh-tool registration parity.

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_pwsh_local::Config as PwshConfig;
use seekdeep_shell_env::ShellEnvConfig;
use seekdeep_subprocess_local::LocalSubprocessRuntime;
use seekdeep_system_prompt::{AssembleContext, SystemPromptConfig};
use seekdeep_tools::{ToolPresentationMode, ToolRuntimeConfig};
use serde_json::Value;

#[tokio::test]
async fn missing_shell_keeps_plugin_pending_then_hmr_disposal_removes_tool_and_prompt() {
    let context = Context::new();
    let prompt =
        seekdeep_system_prompt::install(&context, SystemPromptConfig::default()).expect("prompt");
    let tools = seekdeep_tools::install(
        &context,
        &prompt,
        ToolRuntimeConfig {
            mode: ToolPresentationMode::Native,
            ..ToolRuntimeConfig::default()
        },
    )
    .expect("tools");
    seekdeep_shell_env::apply(&context, &ShellEnvConfig::default()).expect("shell env");

    let plugin = context
        .plugin(seekdeep_tool_pwsh::plugin(), Value::Null)
        .expect("mount tool pwsh");
    plugin.await_settled().await.expect("pending settles");
    assert!(tools.schemas(None).is_empty());

    let spill = tempfile::tempdir().expect("spill");
    LocalSubprocessRuntime::install_runtime(
        &context,
        Arc::new(LocalSubprocessRuntime::with_spill_dir(spill.path())),
    )
    .expect("subprocess");
    seekdeep_pwsh_local::apply(&context, PwshConfig::default())
        .await
        .expect("pwsh provider");
    plugin.await_settled().await.expect("activated");
    assert_eq!(tools.schemas(None).len(), 1);
    assert!(
        prompt
            .assemble(AssembleContext::default())
            .await
            .expect("prompt assembly")
            .sections
            .iter()
            .any(|section| section.name == "tool:pwsh")
    );

    plugin.dispose().await.expect("dispose tool pwsh");
    assert!(tools.schemas(None).is_empty());
    assert!(
        prompt
            .assemble(AssembleContext::default())
            .await
            .expect("prompt assembly")
            .sections
            .iter()
            .all(|section| section.name != "tool:pwsh")
    );
}
