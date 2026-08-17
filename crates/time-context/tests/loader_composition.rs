//! Declarative Loader coverage for the production plugin export path.

use std::sync::Arc;

use seekdeep_agent::{
    Agent, AgentEvents, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications, PreStepDecision,
};
use seekdeep_agent_loop::AgentPreStepEvent;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_core::session::{Session, SessionEvent, SessionId, SurfaceOp};
use seekdeep_llm::{AbortSignal, ContentBlock, MessageSource, UserMessage};
use seekdeep_loader::PluginCatalog;
use seekdeep_scope::ScopeKey;
use seekdeep_time_context::{NAME, plugin};
use serde_json::{Value, json};

fn agents_plugin() -> Plugin {
    Plugin::new("agents", std::iter::empty::<&str>(), |context, _| {
        Box::pin(async move {
            let agents = Arc::new(AgentRegistry::new(context.clone()));
            agents.provide(&context)?;
            Ok(())
        })
    })
}

fn event(event_type: &str, seq: u64, data: Value, surface: bool) -> SessionEvent {
    SessionEvent {
        event_type: event_type.to_owned(),
        seq,
        time: 1_783_987_200_000,
        data,
        source_event_seqs: None,
        surface_op: surface.then(SurfaceOp::append),
        ignorable: None,
    }
}

fn session() -> Arc<Session> {
    let user = UserMessage::new(
        vec![ContentBlock::Text {
            text: "hello".to_owned(),
        }],
        MessageSource::user(),
    );
    Session::create(
        &SessionId::new("loader-time-context"),
        Some(vec![
            event("turn/start", 0, json!({"turn": 1}), false),
            event(
                "user/message",
                1,
                serde_json::to_value(user).expect("user"),
                true,
            ),
        ]),
        None,
    )
    .expect("session")
}

fn agent(context: &Context, session: Arc<Session>) -> Arc<Agent> {
    let inbox =
        Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
    Arc::new(Agent::new(
        SessionId::new("loader-time-context"),
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ))
}

#[tokio::test]
async fn declarative_plugin_boots_injects_and_disposes_through_loader() {
    let context = Context::new();
    let catalog = PluginCatalog::new();
    catalog
        .register_named("agents", agents_plugin())
        .expect("agents catalog");
    catalog
        .register_named("seekdeep-time-context", plugin())
        .expect("time-context catalog");
    let composition = catalog
        .load_yaml(
            &context,
            r"
- id: agents
  name: agents
- id: time-context
  name: seekdeep-time-context
  config:
    timeZone: UTC
    refreshIntervalMs: 0
",
        )
        .await
        .expect("composition");
    assert_eq!(plugin().name(), NAME);
    assert_eq!(plugin().inject(), ["agents"]);

    let session = session();
    let proposed = UserMessage::new(
        vec![ContentBlock::Text {
            text: "proposal".to_owned(),
        }],
        MessageSource::plugin("loader-test"),
    );
    let decision = AgentEvents::new(context.clone(), agent(&context, session))
        .waterfall(
            "agent/pre-step",
            AgentPreStepEvent {
                messages: vec![proposed.clone()],
                turn: 1,
                step: 1,
                signal: AbortSignal::default(),
            },
            move || async move {
                Ok(PreStepDecision::Enter {
                    messages: vec![proposed],
                })
            },
        )
        .await
        .expect("pre-step");
    let PreStepDecision::Enter { messages } = decision else {
        panic!("listener must preserve entry");
    };
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1].source().fields["plugin"], NAME);
    let ContentBlock::Text { text } = &messages[1].content()[0] else {
        panic!("time context must be text");
    };
    assert!(text.contains("Time sampled while preparing turn 1, step 1:"));

    composition.dispose().await.expect("dispose");
    assert_eq!(
        context.events().listener_count(&context, "agent/pre-step"),
        0
    );
}

#[tokio::test]
async fn loader_surfaces_configuration_failure_with_entry_identity() {
    let context = Context::new();
    let catalog = PluginCatalog::new();
    catalog
        .register_named("agents", agents_plugin())
        .expect("agents catalog");
    catalog
        .register_named("seekdeep-time-context", plugin())
        .expect("time-context catalog");
    let error = catalog
        .load_yaml(
            &context,
            r"
- id: agents
  name: agents
- id: clock
  name: seekdeep-time-context
  config:
    refreshIntervalMs: -1
",
        )
        .await
        .expect_err("invalid config");
    let rendered = error.to_string();
    assert!(rendered.contains("clock"));
    assert!(rendered.contains("non-negative safe integer"));
    assert!(context.get(seekdeep_agent::AGENTS).is_none());
}

#[tokio::test]
async fn loader_config_schema_preserves_source_null_type_and_unknown_field_behavior() {
    for (yaml, expected) in [
        ("config: { timeZone: null }", "invalid IANA timeZone null"),
        (
            "config: { refreshIntervalMs: null }",
            "non-negative safe integer, got null",
        ),
        (
            "config: { timeZone: 1 }",
            "$.timeZone expected string but got 1",
        ),
        (
            "config: { refreshIntervalMs: \"1\" }",
            "$.refreshIntervalMs expected number but got 1",
        ),
    ] {
        let context = Context::new();
        let catalog = PluginCatalog::new();
        catalog
            .register_named("agents", agents_plugin())
            .expect("agents catalog");
        catalog
            .register_named("seekdeep-time-context", plugin())
            .expect("time-context catalog");
        let source = format!(
            "- id: agents\n  name: agents\n- id: clock\n  name: seekdeep-time-context\n  {yaml}\n"
        );
        let error = catalog
            .load_yaml(&context, &source)
            .await
            .expect_err("invalid source-shaped config");
        assert!(error.to_string().contains(expected), "{yaml}: {error}");
    }

    for yaml in ["config: null", "config: { futureField: 1, timeZone: UTC }"] {
        let context = Context::new();
        let catalog = PluginCatalog::new();
        catalog
            .register_named("agents", agents_plugin())
            .expect("agents catalog");
        catalog
            .register_named("seekdeep-time-context", plugin())
            .expect("time-context catalog");
        let source = format!(
            "- id: agents\n  name: agents\n- id: clock\n  name: seekdeep-time-context\n  {yaml}\n"
        );
        let composition = catalog
            .load_yaml(&context, &source)
            .await
            .expect("source accepts null root and unknown object fields");
        composition.dispose().await.expect("dispose");
    }
}
