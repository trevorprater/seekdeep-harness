//! Child-only report tool, guidance, rollback, and revocation.

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_scope::{ScopeKey, create_scope, scope_of};
use seekdeep_subagent::{SubagentReportDelivery, SubagentRuntime};
use seekdeep_system_prompt::{AssembleContext, SystemPrompt, SystemPromptConfig};
use seekdeep_tool_subagent_report::{Config, ReportDelivery, install_report_tool};
use seekdeep_tools::{
    ContentToolFixtureOptions, ToolRuntime, ToolRuntimeConfig, define_content_tool_fixture,
};
use serde_json::{Value, json};

struct Harness {
    root: Context,
    prompt: Arc<SystemPrompt>,
    tools: Arc<ToolRuntime>,
}

impl Harness {
    fn new() -> Self {
        let root = Context::new();
        let prompt = SystemPrompt::new(&root, SystemPromptConfig::default()).unwrap();
        prompt.provide(&root).unwrap();
        let tools =
            ToolRuntime::new_with_system_prompt(&root, &prompt, ToolRuntimeConfig::default())
                .unwrap();
        tools.provide(&root).unwrap();
        SubagentRuntime::new(&root).provide(&root).unwrap();
        Self {
            root,
            prompt,
            tools,
        }
    }

    async fn sections(&self, context: &Context) -> Vec<String> {
        self.prompt
            .assemble(AssembleContext {
                scope: scope_of(context),
                ..AssembleContext::default()
            })
            .await
            .unwrap()
            .sections
            .into_iter()
            .map(|section| section.name)
            .collect()
    }
}

#[tokio::test]
async fn report_tool_and_guidance_are_child_scoped_and_revoke_together() {
    let harness = Harness::new();
    let child = create_scope(&harness.root, ScopeKey::new(), None).unwrap();
    let sibling = create_scope(&harness.root, ScopeKey::new(), None).unwrap();
    let dispose =
        install_report_tool(&child.context, &harness.root, SubagentReportDelivery::Quiet).unwrap();
    assert!(
        harness
            .tools
            .schemas(scope_of(&child.context))
            .iter()
            .any(|schema| schema.name == "report")
    );
    assert!(
        !harness
            .tools
            .schemas(scope_of(&sibling.context))
            .iter()
            .any(|schema| schema.name == "report")
    );
    assert!(
        harness
            .sections(&child.context)
            .await
            .contains(&"tool:report".to_owned())
    );
    assert!(
        !harness
            .sections(&harness.root)
            .await
            .contains(&"tool:report".to_owned())
    );
    dispose();
    assert!(
        !harness
            .tools
            .schemas(scope_of(&child.context))
            .iter()
            .any(|schema| schema.name == "report")
    );
    assert!(
        !harness
            .sections(&child.context)
            .await
            .contains(&"tool:report".to_owned())
    );
}

#[tokio::test]
async fn tool_registration_failure_rolls_back_prompt_guidance() {
    let harness = Harness::new();
    let child = create_scope(&harness.root, ScopeKey::new(), None).unwrap();
    let conflict = ContentToolFixtureOptions::new(
        "report",
        "conflict",
        json!({}),
        Arc::new(|_: Value, _| Box::pin(async { Ok(Vec::new()) })),
    );
    harness
        .tools
        .register(
            &child.context,
            define_content_tool_fixture(conflict).unwrap(),
        )
        .unwrap();
    let Err(error) = install_report_tool(
        &child.context,
        &harness.root,
        SubagentReportDelivery::Wakeup,
    ) else {
        panic!("conflicting report registration succeeded");
    };
    assert!(error.to_string().contains("already registered"));
    assert!(
        !harness
            .sections(&child.context)
            .await
            .contains(&"tool:report".to_owned())
    );
}

#[test]
fn configuration_defaults_to_wakeup_and_rejects_unknown_delivery() {
    assert_eq!(Config::default().report_delivery, ReportDelivery::Wakeup);
    assert_eq!(
        serde_json::from_value::<Config>(json!({ "reportDelivery": "quiet" }))
            .unwrap()
            .report_delivery,
        ReportDelivery::Quiet
    );
    assert!(serde_json::from_value::<Config>(json!({ "reportDelivery": "shout" })).is_err());
}
