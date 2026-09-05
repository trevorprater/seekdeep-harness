//! Semantic Rust counterparts of the source preset plugin fixtures.

use std::{sync::Arc, time::Duration};

use seekdeep_agent_presets::{
    AgentPresetConfig, AgentPresetRegistry, AgentPresetRegistryConfig, COMPOSITION_FILE,
    PresetRoot, PresetTrust,
};
use seekdeep_cordis::{Context, Plugin};
use seekdeep_loader::PluginCatalog;
use seekdeep_scope::{Scope, ScopeKey, create_scope, scope_of};
use seekdeep_system_prompt::{AssembleContext, PromptSection, SystemPrompt, SystemPromptConfig};
use seekdeep_tools::{
    ContentToolFixtureOptions, ToolPresentationMode, ToolRuntime, ToolRuntimeConfig,
    define_content_tool_fixture,
};
use serde_json::{Value, json};
use tokio::sync::Notify;

async fn preset(root: &std::path::Path, id: &str, body: &str) -> std::path::PathBuf {
    let directory = root.join(id);
    tokio::fs::create_dir_all(&directory).await.unwrap();
    let path = directory.join(COMPOSITION_FILE);
    tokio::fs::write(&path, body).await.unwrap();
    path
}

fn roster(
    context: &Context,
    catalog: PluginCatalog,
    root: &std::path::Path,
    default: &str,
) -> Arc<AgentPresetRegistry> {
    AgentPresetRegistry::new(
        context,
        catalog,
        AgentPresetRegistryConfig {
            roster: AgentPresetConfig {
                default: default.to_owned(),
                roots: vec![PresetRoot {
                    path: root.to_string_lossy().into_owned(),
                    trust: PresetTrust::User,
                }],
                include_user_root: false,
            },
            user_root: None,
        },
    )
    .unwrap()
}

fn agent_scope(context: &Context) -> Scope {
    create_scope(context, ScopeKey::new(), None).unwrap()
}

fn tools(context: &Context) -> Arc<ToolRuntime> {
    let tools = ToolRuntime::new(
        context.clone(),
        ToolRuntimeConfig {
            mode: ToolPresentationMode::Native,
            max_parallel_sub_calls: 4,
        },
    )
    .unwrap();
    tools.provide(context).unwrap();
    tools
}

#[tokio::test]
async fn contribute_fixture_registers_config_named_tool_and_prompt_section_in_the_preset_scope() {
    let context = Context::new();
    let tools = tools(&context);
    let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    prompt.provide(&context).unwrap();
    let catalog = PluginCatalog::new();
    catalog
        .register_named(
            "fixture:contribute",
            Plugin::new(
                "contribute",
                ["tools", "systemPrompt"],
                move |plugin_context, config| {
                    let tools = tools.clone();
                    let prompt = prompt.clone();
                    Box::pin(async move {
                        let name = config
                            .get("tool")
                            .and_then(Value::as_str)
                            .ok_or_else(|| anyhow::anyhow!("fixture tool name is missing"))?
                            .to_owned();
                        let fixture_name = name.clone();
                        let definition = ContentToolFixtureOptions::new(
                            name.clone(),
                            format!("fixture tool {name}"),
                            json!({}),
                            Arc::new(move |_: Value, _| {
                                let value = fixture_name.clone();
                                Box::pin(async move {
                                    Ok(vec![seekdeep_llm::ContentBlock::Text { text: value }])
                                })
                            }),
                        );
                        tools
                            .register(&plugin_context, define_content_tool_fixture(definition)?)?;
                        prompt.section(
                            &plugin_context,
                            PromptSection::new(
                                format!("preset:{name}"),
                                10.0,
                                format!("section for {name}"),
                            ),
                        )?;
                        Ok(())
                    })
                },
            ),
        )
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    preset(
        root.path(),
        "contributed",
        "- id: contribution\n  name: fixture:contribute\n  config:\n    tool: alpha\n",
    )
    .await;
    let roster = roster(&context, catalog, root.path(), "contributed");
    let scope = agent_scope(&context);
    roster.mount(&scope.context, None).await.unwrap();
    let scope_key = scope_of(&scope.context);
    assert!(context.get(seekdeep_tools::TOOLS).is_some());
    let tool_runtime = context.get(seekdeep_tools::TOOLS).unwrap();
    assert!(tool_runtime.get("alpha", scope_key).is_some());
    let assembled = context
        .get(seekdeep_system_prompt::SYSTEM_PROMPT)
        .unwrap()
        .assemble(AssembleContext {
            scope: scope_key,
            ..AssembleContext::default()
        })
        .await
        .unwrap();
    assert!(
        assembled
            .sections
            .iter()
            .any(|section| section.name == "preset:alpha" && section.text == "section for alpha")
    );
}

#[tokio::test]
async fn needs_missing_fixture_fails_the_mount_with_its_unresolved_service() {
    let context = Context::new();
    let catalog = PluginCatalog::new();
    catalog
        .register_named(
            "fixture:needs-missing",
            Plugin::new("needs-missing", ["serviceThatDoesNotExist"], |_, _| {
                Box::pin(async { Ok(()) })
            }),
        )
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    preset(
        root.path(),
        "pending",
        "- id: waits\n  name: fixture:needs-missing\n",
    )
    .await;
    let roster = roster(&context, catalog, root.path(), "pending");
    let scope = agent_scope(&context);
    let error = tokio::time::timeout(Duration::from_secs(1), roster.mount(&scope.context, None))
        .await
        .expect("pending mount audit must settle")
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("serviceThatDoesNotExist"), "{message}");
}

#[tokio::test]
async fn self_dispose_fixture_never_rewrites_the_shared_composition_file() {
    let context = Context::new();
    let catalog = PluginCatalog::new();
    let dispose = Arc::new(Notify::new());
    let disposed = Arc::new(Notify::new());
    catalog
        .register_named(
            "fixture:self-dispose",
            Plugin::new("self-dispose", std::iter::empty::<&str>(), {
                let dispose = dispose.clone();
                let disposed = disposed.clone();
                move |plugin_context, _| {
                    let fiber = plugin_context.fiber().clone();
                    let dispose = dispose.clone();
                    let disposed = disposed.clone();
                    Box::pin(async move {
                        tokio::spawn(async move {
                            dispose.notified().await;
                            fiber.dispose().await.unwrap();
                            disposed.notify_one();
                        });
                        Ok(())
                    })
                }
            }),
        )
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let composition = "- id: goes-away\n  name: fixture:self-dispose\n";
    let path = preset(root.path(), "self-disposing", composition).await;
    let roster = roster(&context, catalog, root.path(), "self-disposing");
    let scope = agent_scope(&context);
    roster.mount(&scope.context, None).await.unwrap();
    dispose.notify_one();
    tokio::time::timeout(Duration::from_secs(1), disposed.notified())
        .await
        .expect("self disposal must settle");
    assert_eq!(tokio::fs::read_to_string(path).await.unwrap(), composition);
}
