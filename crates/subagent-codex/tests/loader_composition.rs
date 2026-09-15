//! Portable Loader composition without resolving or starting a Codex process.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use seekdeep_cordis::Context;
use seekdeep_llm::AbortSignal;
use seekdeep_loader::PluginCatalog;
use seekdeep_subagent_codex::plugin;
use seekdeep_tools::ToolRuntimeConfig;
use serde_json::json;

#[derive(Debug, Default)]
struct CountingSubprocess(AtomicUsize);

#[async_trait]
impl seekdeep_subprocess::SubprocessRuntime for CountingSubprocess {
    async fn resolve_executable(
        &self,
        _command: &str,
        _env: Option<&seekdeep_subprocess::SubprocessLookupEnvironment>,
        _signal: Option<AbortSignal>,
    ) -> anyhow::Result<String> {
        self.0.fetch_add(1, Ordering::AcqRel);
        anyhow::bail!("loader must not resolve Codex")
    }

    fn spawn(
        &self,
        _spec: seekdeep_subprocess::SubprocessSpawnSpec,
    ) -> anyhow::Result<seekdeep_subprocess::SubprocessHandleRef> {
        self.0.fetch_add(1, Ordering::AcqRel);
        anyhow::bail!("loader must not spawn Codex")
    }

    async fn spawn_terminal(
        &self,
        _spec: seekdeep_subprocess::SubprocessTerminalSpawnSpec,
    ) -> anyhow::Result<seekdeep_subprocess::SubprocessTerminalHandleRef> {
        self.0.fetch_add(1, Ordering::AcqRel);
        anyhow::bail!("loader must not spawn a terminal")
    }
}

fn loader_catalog(starts: &Arc<CountingSubprocess>) -> PluginCatalog {
    let catalog = PluginCatalog::new();
    catalog
        .register_named("subagents", seekdeep_subagent::plugin())
        .unwrap();
    let subprocess = Arc::clone(starts);
    catalog
        .register_named(
            "subprocess",
            seekdeep_cordis::Plugin::new(
                "subprocess",
                std::iter::empty::<&str>(),
                move |context, _| {
                    let subprocess = Arc::clone(&subprocess);
                    Box::pin(async move {
                        let erased: Arc<dyn seekdeep_subprocess::SubprocessRuntime> = subprocess;
                        seekdeep_subprocess::SubprocessService::new(erased).provide(&context)?;
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
    catalog.register_named("codex", plugin()).unwrap();
    catalog
        .register_named("tool", seekdeep_tool_subagent::plugin())
        .unwrap();
    catalog
}

#[tokio::test]
async fn declarative_loader_composes_the_opt_in_provider_and_foreground_tool_without_spawn() {
    let starts = Arc::new(CountingSubprocess::default());
    let catalog = loader_catalog(&starts);
    let context = Context::new();
    let composition = catalog
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
                "- id: codex\n",
                "  name: codex\n",
                "- id: tool\n",
                "  name: tool\n",
                "  config:\n",
                "    provider: codex\n",
                "    toolName: subagent_codex\n",
                "    enableRunInBackground: false\n",
                "    backgroundMode: one-shot\n",
                "    maxDepth: provider-managed\n",
            ),
        )
        .await
        .unwrap();
    assert_eq!(composition.fibers().len(), 6);
    let subagents = context.get(seekdeep_subagent::SUBAGENTS).unwrap();
    assert_eq!(subagents.list(), ["codex"]);
    let provider = subagents.get_provider("codex").unwrap();
    assert_eq!(
        provider.capabilities(),
        &seekdeep_subagent::no_start_capabilities()
    );
    let tools = context.get(seekdeep_tools::TOOLS).unwrap();
    let tool = tools.get("subagent_codex", None).unwrap();
    assert_eq!(
        tool.parameters["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["description", "prompt"]
    );
    assert_eq!(
        tool.parameters["required"],
        json!(["description", "prompt"])
    );
    assert_eq!(starts.0.load(Ordering::Acquire), 0);
    composition.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}
