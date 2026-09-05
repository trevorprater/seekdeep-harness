//! Real declarative Loader composition for the required todo policy.

use std::sync::Arc;

use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::{Context, Plugin};
use seekdeep_core::session::{Session, SessionId};
use seekdeep_llm::{AbortSignal, CallId};
use seekdeep_loader::PluginCatalog;
use seekdeep_scope::ScopeKey;
use seekdeep_system_prompt::{SYSTEM_PROMPT, SystemPromptConfig};
use seekdeep_tool_todo::TOOL_NAME;
use seekdeep_tools::{TOOLS, ToolExecutionInput, ToolRuntimeConfig};
use serde_json::json;

fn catalog() -> PluginCatalog {
    let catalog = PluginCatalog::new();
    catalog
        .register_named(
            "seekdeep-system-prompt",
            Plugin::new("system-prompt", std::iter::empty::<&str>(), |context, _| {
                Box::pin(async move {
                    seekdeep_system_prompt::install(&context, SystemPromptConfig::default())?;
                    Ok(())
                })
            }),
        )
        .expect("register prompt");
    catalog
        .register_named(
            "seekdeep-tools",
            Plugin::new("tools", ["systemPrompt"], |context, _| {
                Box::pin(async move {
                    let prompt = context
                        .get(SYSTEM_PROMPT)
                        .ok_or_else(|| anyhow::anyhow!("tools requires systemPrompt"))?;
                    seekdeep_tools::install(&context, &prompt, ToolRuntimeConfig::default())?;
                    Ok(())
                })
            }),
        )
        .expect("register tools");
    catalog
        .register_named("seekdeep-tool-todo", seekdeep_tool_todo::plugin())
        .expect("register todo");
    catalog
}

fn yaml(config: Option<&str>) -> String {
    let mut source = concat!(
        "- id: prompt\n",
        "  name: seekdeep-system-prompt\n",
        "- id: tools\n",
        "  name: seekdeep-tools\n",
        "- id: todo\n",
        "  name: seekdeep-tool-todo\n",
    )
    .to_owned();
    if let Some(config) = config {
        source.push_str("  config:\n");
        source.push_str("    allowParallelInProgress: ");
        source.push_str(config);
        source.push('\n');
    }
    source
}

fn agent(id: &str) -> Arc<Agent> {
    let id = SessionId::new(id);
    let session = Session::create(&id, None, None).expect("session");
    let inbox =
        Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
    Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session,
        inbox,
        Context::new(),
        ScopeKey::new(),
    ))
}

fn input(owner: &Arc<Agent>) -> ToolExecutionInput {
    let todos = json!([
        {"content": "run subagent a", "status": "in_progress"},
        {"content": "run subagent b", "status": "in_progress"},
    ]);
    let mut input = ToolExecutionInput::new(
        CallId::new("parallel"),
        TOOL_NAME,
        json!({"todos": todos}),
        AbortSignal::default(),
    );
    input.agent = Some(owner.clone());
    input.agent_session = Some(owner.session().clone());
    input
}

fn rendered_text(result: &seekdeep_tools::ToolExecutionResult) -> String {
    result
        .content()
        .iter()
        .filter_map(|block| match block {
            seekdeep_llm::ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn yaml_false_narrows_description_and_rejects_parallel_write() {
    let context = Context::new();
    let composition = catalog()
        .load_yaml(&context, &yaml(Some("false")))
        .await
        .expect("composition");
    let tools = context.get(TOOLS).expect("tools");
    let description = tools
        .schemas(None)
        .into_iter()
        .find(|schema| schema.name == TOOL_NAME)
        .expect("todo schema")
        .description;
    assert!(description.contains("Keep AT MOST ONE todo `in_progress`"));
    assert!(!description.contains("several at once"));

    let owner = agent("loader-false");
    let result = tools.execute(input(&owner)).await;
    assert!(result.is_error());
    assert!(rendered_text(&result).contains("at most one task may be in_progress"));
    assert!(
        !owner
            .session()
            .events()
            .iter()
            .any(|event| event.event_type == "todo/write")
    );

    composition.dispose().await.expect("dispose composition");
    context.fiber().dispose().await.expect("dispose root");
}

#[tokio::test]
async fn yaml_true_allows_parallel_write_end_to_end() {
    let context = Context::new();
    let composition = catalog()
        .load_yaml(&context, &yaml(Some("true")))
        .await
        .expect("composition");
    let tools = context.get(TOOLS).expect("tools");
    let description = tools
        .schemas(None)
        .into_iter()
        .find(|schema| schema.name == TOOL_NAME)
        .expect("todo schema")
        .description;
    assert!(description.contains("several at once when work genuinely runs in parallel"));

    let owner = agent("loader-true");
    let result = tools.execute(input(&owner)).await;
    assert!(!result.is_error(), "error: {:?}", result.error());
    assert!(
        owner
            .session()
            .events()
            .iter()
            .any(|event| event.event_type == "todo/write")
    );

    composition.dispose().await.expect("dispose composition");
    assert!(context.get(TOOLS).is_none());
    context.fiber().dispose().await.expect("dispose root");
}

#[tokio::test]
async fn loader_rejects_missing_and_non_boolean_policy_at_mount() {
    for (label, source, expected) in [
        (
            "missing",
            yaml(None),
            "$.allowParallelInProgress missing required value",
        ),
        (
            "string",
            yaml(Some("\"no\"")),
            "$.allowParallelInProgress expected boolean",
        ),
    ] {
        let context = Context::new();
        let error = catalog()
            .load_yaml(&context, &source)
            .await
            .expect_err(label);
        assert!(
            error.to_string().contains(expected),
            "{label}: expected {expected:?} in {error:#}"
        );
        assert!(context.get(TOOLS).is_none(), "{label}: rollback tools");
        context.fiber().dispose().await.expect("dispose root");
    }
}
