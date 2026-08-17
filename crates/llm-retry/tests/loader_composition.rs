//! Real declarative loader composition for the agents dependency and retry plugin.

use std::sync::Arc;

use seekdeep_agent::{
    AGENTS, Agent, AgentEvents, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications,
    RequestErrorAction,
};
use seekdeep_agent_loop::AgentRequestErrorEvent;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_core::session::{AppendOptions, Session, SessionId};
use seekdeep_llm::{AbortSignal, LlmFailure, ProviderId, resolve_retry_policy};
use seekdeep_loader::PluginCatalog;
use seekdeep_scope::ScopeKey;
use serde_json::json;

fn agents_plugin() -> Plugin {
    Plugin::new("agents", std::iter::empty::<&str>(), |context, _| {
        Box::pin(async move {
            let registry = Arc::new(AgentRegistry::new(context.clone()));
            registry.provide(&context)?;
            Ok(())
        })
    })
}

fn test_agent(context: &Context) -> Arc<Agent> {
    let session = Session::create(&SessionId::new("loader-retry"), None, None).unwrap();
    for (event_type, data) in [
        ("turn/start", json!({"turn":1})),
        ("step/start", json!({"turn":1,"step":1})),
        (
            "request/header",
            json!({"header":{"config":{"provider":"mock","model":"model"}},"reason":"initial"}),
        ),
    ] {
        session
            .append(event_type, data, AppendOptions::default())
            .unwrap();
    }
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    Arc::new(Agent::new(
        SessionId::new("loader-retry"),
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ))
}

#[tokio::test]
async fn yaml_loads_agents_then_retry_and_executes_the_provider_policy() {
    let catalog = PluginCatalog::new();
    catalog
        .register_named("seekdeep-agent", agents_plugin())
        .unwrap();
    catalog
        .register_named("seekdeep-llm-retry", seekdeep_llm_retry::plugin())
        .unwrap();
    let context = Context::new();
    let composition = catalog
        .load_yaml(
            &context,
            concat!(
                "- id: agents\n",
                "  name: seekdeep-agent\n",
                "- id: retry\n",
                "  name: seekdeep-llm-retry\n",
            ),
        )
        .await
        .unwrap();
    assert_eq!(composition.fibers().len(), 2);
    assert!(context.get(AGENTS).is_some());

    let agent = test_agent(&context);
    let policy = resolve_retry_policy(
        Some(&json!({
            "mode":"normal",
            "maxRetries":1,
            "retryableCodes":["SERVER"],
            "backoff":{"initialDelayMs":1,"maxDelayMs":1,"jitterRatio":0}
        })),
        "provider retryPolicy",
    )
    .unwrap();
    let action = AgentEvents::new(context.clone(), agent.clone())
        .waterfall(
            "agent/request-error",
            AgentRequestErrorEvent {
                turn: 1,
                step: 1,
                provider: ProviderId::new("mock"),
                failure: LlmFailure {
                    message: "temporary outage".to_owned(),
                    code: "SERVER".to_owned(),
                    status: None,
                    provider_retry_after_ms: None,
                    request_id: None,
                },
                retry_policy: Some(policy),
                signal: AbortSignal::default(),
            },
            || async { Ok(RequestErrorAction::Terminal) },
        )
        .await
        .unwrap();
    assert_eq!(action, RequestErrorAction::Retry);
    assert_eq!(
        agent
            .session()
            .events()
            .iter()
            .filter(|event| event.event_type == "llm/retry")
            .count(),
        1
    );
    composition.dispose().await.unwrap();
    assert!(context.get(AGENTS).is_none());
    context.fiber().dispose().await.unwrap();
}
