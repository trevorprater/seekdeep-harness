//! Declarative opt-in provider and foreground tool composition without process startup.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use seekdeep_cordis::Context;
use seekdeep_llm::AbortSignal;
use seekdeep_loader::PluginCatalog;
use seekdeep_subagent_claude_code::plugin;
use seekdeep_tools::ToolRuntimeConfig;
use serde_json::json;

#[derive(Debug, Default)]
struct CountingSubprocess {
    resolves: AtomicUsize,
    spawns: AtomicUsize,
}

#[async_trait]
impl seekdeep_subprocess::SubprocessRuntime for CountingSubprocess {
    async fn resolve_executable(
        &self,
        _command: &str,
        _env: Option<&seekdeep_subprocess::SubprocessLookupEnvironment>,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<String> {
        self.resolves.fetch_add(1, Ordering::AcqRel);
        anyhow::bail!("Loader composition must not resolve Claude")
    }

    fn spawn(
        &self,
        _spec: seekdeep_subprocess::SubprocessSpawnSpec,
    ) -> anyhow::Result<seekdeep_subprocess::SubprocessHandleRef> {
        self.spawns.fetch_add(1, Ordering::AcqRel);
        anyhow::bail!("Loader composition must not spawn Claude")
    }

    async fn spawn_terminal(
        &self,
        _spec: seekdeep_subprocess::SubprocessTerminalSpawnSpec,
    ) -> anyhow::Result<seekdeep_subprocess::SubprocessTerminalHandleRef> {
        anyhow::bail!("Loader composition must not spawn a terminal")
    }
}

fn catalog(processes: &Arc<CountingSubprocess>) -> PluginCatalog {
    let catalog = PluginCatalog::new();
    catalog
        .register_named("subagents", seekdeep_subagent::plugin())
        .unwrap();
    let processes = Arc::clone(processes);
    catalog
        .register_named(
            "subprocess",
            seekdeep_cordis::Plugin::new(
                "subprocess",
                std::iter::empty::<&str>(),
                move |context, _| {
                    let processes = Arc::clone(&processes);
                    Box::pin(async move {
                        let runtime: Arc<dyn seekdeep_subprocess::SubprocessRuntime> = processes;
                        seekdeep_subprocess::SubprocessService::new(runtime).provide(&context)?;
                        Ok(())
                    })
                },
            ),
        )
        .unwrap();
    catalog
        .register_named(
            "prompt",
            seekdeep_cordis::Plugin::new("prompt", std::iter::empty::<&str>(), |context, _| {
                Box::pin(async move {
                    seekdeep_system_prompt::install(
                        &context,
                        seekdeep_system_prompt::SystemPromptConfig::default(),
                    )?;
                    Ok(())
                })
            }),
        )
        .unwrap();
    catalog
        .register_named(
            "tools",
            seekdeep_cordis::Plugin::new("tools", ["systemPrompt"], |context, _| {
                Box::pin(async move {
                    let prompt = context
                        .get(seekdeep_system_prompt::SYSTEM_PROMPT)
                        .ok_or_else(|| anyhow::anyhow!("tools requires systemPrompt"))?;
                    seekdeep_tools::install(&context, &prompt, ToolRuntimeConfig::default())?;
                    Ok(())
                })
            }),
        )
        .unwrap();
    catalog.register_named("claude", plugin()).unwrap();
    catalog
        .register_named("tool", seekdeep_tool_subagent::plugin())
        .unwrap();
    catalog
}

#[tokio::test]
async fn loader_mounts_provider_and_foreground_tool_without_starting_claude() {
    let processes = Arc::new(CountingSubprocess::default());
    let context = Context::new();
    let composition = catalog(&processes)
        .load_yaml(
            &context,
            concat!(
                "- id: subagents\n",
                "  name: subagents\n",
                "- id: subprocess\n",
                "  name: subprocess\n",
                "- id: prompt\n",
                "  name: prompt\n",
                "- id: tools\n",
                "  name: tools\n",
                "- id: claude\n",
                "  name: claude\n",
                "- id: tool\n",
                "  name: tool\n",
                "  config:\n",
                "    provider: claude-code\n",
                "    toolName: subagent_claude_code\n",
                "    enableRunInBackground: false\n",
                "    maxDepth: provider-managed\n",
            ),
        )
        .await
        .unwrap();
    assert_eq!(composition.fibers().len(), 6);
    let subagents = context.get(seekdeep_subagent::SUBAGENTS).unwrap();
    assert_eq!(subagents.list(), ["claude-code"]);
    let provider = subagents.get_provider("claude-code").unwrap();
    assert_eq!(
        provider.capabilities(),
        &seekdeep_subagent::no_start_capabilities()
    );
    assert!(!provider.inherits_parent_context());
    let tools = context.get(seekdeep_tools::TOOLS).unwrap();
    let tool = tools.get("subagent_claude_code", None).unwrap();
    assert_eq!(
        tool.parameters["required"],
        json!(["description", "prompt"])
    );
    assert_eq!(processes.resolves.load(Ordering::Acquire), 0);
    assert_eq!(processes.spawns.load(Ordering::Acquire), 0);
    composition.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}
