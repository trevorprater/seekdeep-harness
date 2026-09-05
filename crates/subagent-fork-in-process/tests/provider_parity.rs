//! Completed-prefix, capability, coexistence, and lifecycle parity.

use std::sync::Arc;

use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_cordis::Context;
use seekdeep_core::session::{AppendOptions, Session, SessionId};
use seekdeep_scope::ScopeKey;
use seekdeep_subagent::{ContinuableCreateRequest, SubagentRuntime};
use seekdeep_subagent_fork_in_process::{INJECT, NAME, completed_turn_prefix, plugin};
use serde_json::{Value, json};

fn parent(context: &Context) -> Arc<Agent> {
    let id = SessionId::new("parent");
    let session = Session::create(&id, None, None).unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ))
}

#[tokio::test]
async fn provider_owns_completed_prefix_semantics_capabilities_and_lifecycle() {
    let context = Context::new();
    let subagents = SubagentRuntime::new(&context);
    subagents.provide(&context).unwrap();
    let definition = plugin();
    assert_eq!(definition.name(), NAME);
    assert_eq!(definition.inject(), INJECT);
    let mounted = context.plugin(definition, Value::Null).unwrap();
    mounted.await_settled().await.unwrap();
    let provider = subagents.get_provider("fork").unwrap();
    assert!(provider.inherits_parent_context());
    assert!(provider.supports_continuable());
    assert_eq!(
        *provider.capabilities(),
        seekdeep_subagent::SubagentCapabilities {
            output_schema: true,
            depth_limit: true,
            tool_filter: true,
            persona: true,
        }
    );

    let parent = parent(&context);
    parent
        .session()
        .append("turn/start", json!({ "turn": 1 }), AppendOptions::default())
        .unwrap();
    assert!(completed_turn_prefix(&parent).is_empty());
    let fresh = provider
        .prepare_continuable(ContinuableCreateRequest {
            session_id: SessionId::new("fresh"),
            parent: parent.clone(),
            signal: seekdeep_llm::AbortSignal::default(),
        })
        .await
        .unwrap();
    assert!(fresh.seed.is_none());

    parent
        .session()
        .append(
            "turn/end",
            json!({ "turn": 1, "reason": { "kind": "completed" } }),
            AppendOptions::default(),
        )
        .unwrap();
    parent
        .session()
        .append("turn/start", json!({ "turn": 2 }), AppendOptions::default())
        .unwrap();
    let prefix = completed_turn_prefix(&parent);
    assert_eq!(prefix.len(), 2);
    assert_eq!(prefix.last().unwrap().event_type, "turn/end");
    assert_eq!(
        prefix.iter().map(|event| event.seq).collect::<Vec<_>>(),
        [0, 1]
    );
    let seeded = provider
        .prepare_continuable(ContinuableCreateRequest {
            session_id: SessionId::new("seeded"),
            parent,
            signal: seekdeep_llm::AbortSignal::default(),
        })
        .await
        .unwrap();
    assert_eq!(seeded.seed.unwrap().len(), 2);

    mounted.dispose().await.unwrap();
    assert!(subagents.get_provider("fork").is_none());
}

#[tokio::test]
async fn fork_and_spawn_names_coexist_in_the_same_registry() {
    let context = Context::new();
    let subagents = SubagentRuntime::new(&context);
    subagents.provide(&context).unwrap();
    let fork = context.plugin(plugin(), Value::Null).unwrap();
    fork.await_settled().await.unwrap();
    seekdeep_subagent_spawn_in_process::apply(
        &context,
        seekdeep_subagent_spawn_in_process::Config::default(),
    )
    .unwrap();
    let mut names = subagents.list();
    names.sort();
    assert_eq!(names, ["fork", "spawn"]);
}
