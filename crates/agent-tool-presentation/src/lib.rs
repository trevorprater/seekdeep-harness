//! Agent-scope selection of Native, Code, or combined tool presentation.

use std::sync::Arc;

use seekdeep_cordis::Plugin;
use seekdeep_invariants::{InvariantInstaller, InvariantRegistration, InvariantRegistry};
use seekdeep_tools::{TOOLS, ToolPresentationMode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Cordis plugin name retained by loader-facing diagnostics.
pub const NAME: &str = "tool-presentation";
/// Static required services. Code runtime availability is mode-dependent.
pub const INJECT: &[&str] = &["tools"];

/// Required per-agent presentation selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentToolPresentationConfig {
    /// Form of this scope's tools exposed to its model.
    pub mode: ToolPresentationMode,
}

/// Builds the lifecycle plugin corresponding to the source package row.
///
/// `native` applies immediately with only `tools`. Code-bearing modes mount a
/// dependency-controlled inner effect, so they remain unapplied until
/// `codeRuntime` exists and automatically unwind if that service disappears.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: AgentToolPresentationConfig = serde_json::from_value(config)?;
            if config.mode == ToolPresentationMode::Native {
                let tools = context.get(TOOLS).ok_or_else(|| {
                    anyhow::anyhow!("tool-presentation lost required tools service")
                })?;
                tools.present_as(&context, config.mode)?;
                return Ok(());
            }

            let mode = config.mode;
            let wait_for_runtime = Plugin::new(
                "tool-presentation:code-runtime",
                ["codeRuntime"],
                move |runtime_context, _| {
                    Box::pin(async move {
                        let tools = runtime_context.get(TOOLS).ok_or_else(|| {
                            anyhow::anyhow!("tool-presentation lost required tools service")
                        })?;
                        tools.present_as(&runtime_context, mode)?;
                        Ok(())
                    })
                },
            );
            context.plugin(wait_for_runtime, Value::Null)?;
            Ok(())
        })
    })
    .with_config_validator(|value| {
        let config: AgentToolPresentationConfig = serde_json::from_value(value.clone())?;
        Ok(serde_json::to_value(config)?)
    })
}

/// Registers the package's explained empty invariant companion.
///
/// Scope isolation, uniqueness, prompt consistency, and teardown are enforced
/// by the Tools and System Prompt registries at their authoritative boundaries.
///
/// # Errors
///
/// Returns ordinary invariant registration failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        "seekdeep-agent-tool-presentation",
        InvariantInstaller::noop(),
    )
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use seekdeep_code_runtime::{CodeRunRequest, CodeRunResult, CodeRuntime, CodeRuntimeBackend};
    use seekdeep_cordis::{Context, FiberState, PluginFiber};
    use seekdeep_llm::ContentBlock;
    use seekdeep_scope::{Scope, ScopeKey, create_scope};
    use seekdeep_system_prompt::{
        AssembleContext, PromptAssembly, SystemPromptConfig, install as install_prompt,
    };
    use seekdeep_tools::{
        RUN_CODE_NAME, ToolDefinition, ToolOutputDefinition, ToolRuntime, ToolRuntimeConfig,
        assert_supported_json_schema, install as install_tools,
    };
    use serde_json::{Map, json};

    use super::*;

    #[derive(Debug)]
    struct StubRuntime;

    #[async_trait]
    impl CodeRuntimeBackend for StubRuntime {
        fn language(&self) -> &'static str {
            "typescript"
        }

        fn isolation(&self) -> &'static str {
            "stub"
        }

        async fn run(&self, _request: CodeRunRequest) -> anyhow::Result<CodeRunResult> {
            Ok(CodeRunResult::default())
        }
    }

    struct Host {
        context: Context,
        prompt: Arc<seekdeep_system_prompt::SystemPrompt>,
        tools: Arc<ToolRuntime>,
    }

    fn echo_definition() -> ToolDefinition {
        ToolDefinition::new(
            "echo",
            "Echo tool.",
            Map::from_iter([
                ("type".to_owned(), json!("object")),
                (
                    "properties".to_owned(),
                    json!({ "value": { "type": "string" } }),
                ),
                ("required".to_owned(), json!(["value"])),
                ("additionalProperties".to_owned(), json!(false)),
            ]),
            ToolOutputDefinition::new(
                Arc::new(
                    assert_supported_json_schema(json!({ "type": "string" }))
                        .expect("output schema"),
                ),
                Arc::new(|_, value| {
                    Ok(vec![ContentBlock::Text {
                        text: value.as_str().unwrap_or_default().to_owned(),
                    }])
                }),
            ),
            Arc::new(|arguments, _| {
                Box::pin(async move { Ok(arguments.get("value").cloned().unwrap_or(Value::Null)) })
            }),
        )
    }

    fn host(with_runtime: bool) -> Host {
        let context = Context::new();
        let prompt = install_prompt(
            &context,
            SystemPromptConfig {
                include_harness_identity: false,
                ..SystemPromptConfig::default()
            },
        )
        .expect("install prompt");
        let tools =
            install_tools(&context, &prompt, ToolRuntimeConfig::default()).expect("install tools");
        tools
            .register(&context, echo_definition())
            .expect("register echo");
        if with_runtime {
            let runtime = Arc::new(CodeRuntime::new(Arc::new(StubRuntime)));
            runtime.provide(&context).expect("provide runtime");
        }
        Host {
            context,
            prompt,
            tools,
        }
    }

    struct Mounted {
        key: ScopeKey,
        _scope: Scope,
        row: Arc<PluginFiber>,
    }

    async fn mount(host: &Host, mode: ToolPresentationMode) -> Mounted {
        let key = ScopeKey::new();
        let scope = create_scope(&host.context, key, None).expect("create scope");
        let row = scope
            .context
            .plugin(
                plugin(),
                serde_json::to_value(AgentToolPresentationConfig { mode }).expect("config"),
            )
            .expect("mount row");
        row.await_settled().await.expect("settle row");
        Mounted {
            key,
            _scope: scope,
            row,
        }
    }

    async fn assembly(host: &Host, key: ScopeKey) -> PromptAssembly {
        host.prompt
            .assemble(AssembleContext {
                scope: Some(key),
                ..AssembleContext::default()
            })
            .await
            .expect("assemble")
    }

    async fn wait_for_mode(
        host: &Host,
        key: ScopeKey,
        expected: ToolPresentationMode,
    ) -> PromptAssembly {
        for _ in 0..100 {
            if host.tools.mode_for(Some(key)) == expected {
                return assembly(host, key).await;
            }
            tokio::task::yield_now().await;
        }
        panic!("presentation mode did not activate")
    }

    #[test]
    fn metadata_declares_only_tools() {
        let plugin = plugin();
        assert_eq!(plugin.name(), NAME);
        assert_eq!(plugin.inject(), ["tools"]);
    }

    #[tokio::test]
    async fn code_is_scoped_while_native_agent_remains_plain() {
        let host = host(true);
        let coded = mount(&host, ToolPresentationMode::Code).await;
        let plain = mount(&host, ToolPresentationMode::Native).await;
        let coded_assembly = wait_for_mode(&host, coded.key, ToolPresentationMode::Code).await;
        let plain_assembly = assembly(&host, plain.key).await;

        assert_eq!(
            coded_assembly
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            [RUN_CODE_NAME]
        );
        assert!(
            coded_assembly
                .sections
                .iter()
                .find(|section| section.name == "tools:sdk")
                .is_some_and(|section| section.text.contains("echo"))
        );
        assert_eq!(
            plain_assembly
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["echo"]
        );
    }

    #[tokio::test]
    async fn both_presents_native_and_code_forms() {
        let host = host(true);
        let mounted = mount(&host, ToolPresentationMode::Both).await;
        let assembly = wait_for_mode(&host, mounted.key, ToolPresentationMode::Both).await;
        assert_eq!(
            assembly
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["echo", RUN_CODE_NAME]
        );
    }

    #[tokio::test]
    async fn disposal_restores_deployment_default() {
        let host = host(true);
        let mounted = mount(&host, ToolPresentationMode::Code).await;
        wait_for_mode(&host, mounted.key, ToolPresentationMode::Code).await;
        mounted.row.dispose().await.expect("dispose row");

        let restored = assembly(&host, mounted.key).await;
        assert_eq!(
            restored
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["echo"]
        );
        assert!(
            restored
                .sections
                .iter()
                .all(|section| section.name != "tools:sdk")
        );
    }

    #[tokio::test]
    async fn code_waits_unapplied_until_runtime_arrives() {
        let host = host(false);
        let mounted = mount(&host, ToolPresentationMode::Code).await;
        assert_eq!(mounted.row.fiber().state(), FiberState::Active);
        assert_eq!(
            host.tools.mode_for(Some(mounted.key)),
            ToolPresentationMode::Native
        );
        assert_eq!(
            assembly(&host, mounted.key)
                .await
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            ["echo"]
        );
    }

    #[tokio::test]
    async fn pending_code_applies_when_runtime_arrives() {
        let host = host(false);
        let mounted = mount(&host, ToolPresentationMode::Code).await;
        let runtime = Arc::new(CodeRuntime::new(Arc::new(StubRuntime)));
        runtime
            .provide(&host.context)
            .expect("provide late runtime");

        let assembly = wait_for_mode(&host, mounted.key, ToolPresentationMode::Code).await;
        assert_eq!(
            assembly
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            [RUN_CODE_NAME]
        );
    }

    #[test]
    fn config_requires_mode() {
        let error = serde_json::from_value::<AgentToolPresentationConfig>(json!({}))
            .expect_err("missing mode must fail");
        assert!(error.to_string().contains("missing field `mode`"));
    }
}
