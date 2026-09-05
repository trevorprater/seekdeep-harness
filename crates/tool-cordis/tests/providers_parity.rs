//! Host inspect provider manifests and live Tool query parity.

use std::sync::Arc;

use seekdeep_agent::{Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_cordis_host_runner::HostCordisInspectQueryContext;
use seekdeep_core::session::{Session, SessionId};
use seekdeep_llm::{AbortSignal, ContentBlock};
use seekdeep_scope::ScopeKey;
use seekdeep_system_prompt::SystemPromptConfig;
use seekdeep_tool_cordis::providers::host_inspect_providers;
use seekdeep_tools::{
    ToolPresentationMode, ToolRuntimeConfig,
    testing::{ContentToolFixtureOptions, define_content_tool_fixture},
};
use serde_json::json;

#[tokio::test]
async fn manifests_are_ordered_and_static_queries_match_generated_catalogs() {
    let context = Context::new();
    let providers = host_inspect_providers(&context);
    assert_eq!(
        providers
            .iter()
            .map(|provider| provider.manifest.id.as_str())
            .collect::<Vec<_>>(),
        ["Service", "Event", "Builtin", "Tool"]
    );
    let query = HostCordisInspectQueryContext {
        signal: AbortSignal::default(),
        session_id: SessionId::new("provider-test"),
    };
    let services = (providers[0].query)("listService".to_owned(), None, query.clone())
        .await
        .unwrap();
    assert_eq!(services["mode"], "catalog");
    let events = (providers[1].query)("listEvents".to_owned(), None, query.clone())
        .await
        .unwrap();
    assert_eq!(events["mode"], "catalog");
    assert!(
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| !event["name"].as_str().unwrap().starts_with("cordis/"))
    );
    let builtins = (providers[2].query)("listBuiltins".to_owned(), None, query)
        .await
        .unwrap();
    assert!(builtins["builtins"].as_array().unwrap().len() >= 7);
}

#[tokio::test]
async fn tool_provider_uses_the_requesting_agents_scoped_tool_view() {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let prompt = seekdeep_system_prompt::install(&context, SystemPromptConfig::default()).unwrap();
    let tools = seekdeep_tools::install(
        &context,
        &prompt,
        ToolRuntimeConfig {
            mode: ToolPresentationMode::Native,
            ..ToolRuntimeConfig::default()
        },
    )
    .unwrap();
    tools
        .register(
            &context,
            define_content_tool_fixture(ContentToolFixtureOptions::new(
                "probe",
                "Probe",
                json!({}),
                Arc::new(|_: serde_json::Value, _| {
                    Box::pin(async {
                        Ok(vec![ContentBlock::Text {
                            text: "ok".to_owned(),
                        }])
                    })
                }),
            ))
            .unwrap(),
        )
        .unwrap();
    let id = SessionId::new("tool-provider");
    let session = Session::create(&id, None, None).unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    let agent = Arc::new(Agent::new(
        id.clone(),
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ));
    agents.register(&context, &agent, None).unwrap();

    let provider = host_inspect_providers(&context).pop().unwrap();
    let result = (provider.query)(
        "listTools".to_owned(),
        None,
        HostCordisInspectQueryContext {
            signal: AbortSignal::default(),
            session_id: id,
        },
    )
    .await
    .unwrap();
    assert_eq!(result["tools"][0]["name"], "probe");
}
