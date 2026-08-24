//! Host command lifecycle and invariant identity parity.

use std::sync::Arc;

use seekdeep_agent::{Agent, AgentOptions, Inbox, NoopInboxNotifications};
use seekdeep_commands::{COMMANDS, CommandRuntime};
use seekdeep_core::session::{Session, SessionId};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_llm::AbortSignal;
use seekdeep_scope::ScopeKey;
use seekdeep_session_log_export::{plugin, register_invariant};
use serde_json::json;

fn agent() -> Arc<Agent> {
    let session = Session::create(&SessionId::new("command"), None, None).unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    Arc::new(Agent::new(
        session.id().clone(),
        AgentOptions::default(),
        session,
        inbox,
        seekdeep_cordis::Context::new(),
        ScopeKey::new(),
    ))
}

#[tokio::test]
async fn registers_pathless_export_command_and_withdraws_with_plugin() {
    let context = seekdeep_cordis::Context::new();
    let commands = CommandRuntime::new(&context);
    commands.provide(&context).unwrap();
    let mounted = context.plugin(plugin(), json!({})).unwrap();
    mounted.await_settled().await.unwrap();
    let owner = agent();
    let descriptor = commands.find(&owner, "export").unwrap();
    assert_eq!(
        descriptor.description,
        "Download this Session log as a ZIP archive"
    );
    let empty = commands
        .execute(owner.clone(), "/export", AbortSignal::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(empty.result.kind(), "success");
    assert_eq!(empty.result.text(), Some("Session log download requested."));
    let path = commands
        .execute(owner.clone(), "/export output.zip", AbortSignal::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(path.result.kind(), "error");
    assert_eq!(
        path.result.text(),
        Some("The Web /export command does not accept a path.")
    );
    mounted.dispose().await.unwrap();
    assert!(context.get(COMMANDS).is_some());
    assert!(commands.find(&owner, "export").is_none());
}

#[tokio::test]
async fn invariant_reserves_and_releases_exact_package_identity() {
    let context = seekdeep_cordis::Context::new();
    let registry = Arc::new(InvariantRegistry::new(&context, &InvariantConfig::default()).unwrap());
    let registration = register_invariant(&registry).unwrap();
    registration.await_ready().await.unwrap();
    assert!(
        register_invariant(&registry)
            .unwrap_err()
            .to_string()
            .contains("@deepseek-ai/seekdeep-session-log-export")
    );
    registration.dispose().await.unwrap();
    register_invariant(&registry)
        .unwrap()
        .await_ready()
        .await
        .unwrap();
}
