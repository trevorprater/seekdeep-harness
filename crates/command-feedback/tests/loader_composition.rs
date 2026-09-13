//! Feedback command composition through the compiled Rust loader catalog.

use std::sync::Arc;

use seekdeep_agent::{AGENTS, Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_commands::COMMANDS;
use seekdeep_core::{
    session::SessionId,
    session_store::{CreateSessionOptions, SESSIONS},
};
use seekdeep_llm::AbortSignal;
use seekdeep_scope::ScopeKey;

fn agent(context: &seekdeep_cordis::Context) -> Arc<Agent> {
    let id = SessionId::new("feedback-loader-agent");
    let sessions = context.get(SESSIONS).unwrap();
    let session = sessions
        .create(context, Some(id.clone()), CreateSessionOptions::default())
        .unwrap();
    let inbox = Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    let agent = Arc::new(Agent::new(
        id,
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ));
    context
        .get(AGENTS)
        .unwrap()
        .register(context, &agent, None)
        .unwrap();
    agent
}

fn catalog() -> seekdeep_loader::PluginCatalog {
    let catalog = seekdeep_loader::PluginCatalog::new();
    catalog
        .register_named(
            "@seekdeep-ai/seekdeep-agent",
            seekdeep_cordis::Plugin::new("agents", std::iter::empty::<String>(), |context, _| {
                Box::pin(async move {
                    let agents = Arc::new(AgentRegistry::new(context.clone()));
                    agents.provide(&context)?;
                    Ok(())
                })
            }),
        )
        .unwrap();
    catalog
        .register_named(
            "@seekdeep-ai/seekdeep-session",
            seekdeep_core::session_store::plugin(),
        )
        .unwrap();
    catalog
        .register_named(
            "@seekdeep-ai/seekdeep-commands",
            seekdeep_cordis::Plugin::new("commands", std::iter::empty::<String>(), |context, _| {
                Box::pin(async move {
                    seekdeep_commands::install(&context)?;
                    Ok(())
                })
            }),
        )
        .unwrap();
    catalog
        .register_named(
            "@seekdeep-ai/seekdeep-command-feedback",
            seekdeep_command_feedback::plugin(),
        )
        .unwrap();
    catalog
}

#[tokio::test]
async fn loader_mount_records_feedback_without_model_visible_output() {
    let context = seekdeep_cordis::Context::new();
    let catalog = catalog();
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("cordis.yml");
    tokio::fs::write(
        &config,
        [
            "- name: '@seekdeep-ai/seekdeep-agent'",
            "- name: '@seekdeep-ai/seekdeep-session'",
            "- name: '@seekdeep-ai/seekdeep-commands'",
            "- name: '@seekdeep-ai/seekdeep-command-feedback'",
            "",
        ]
        .join("\n"),
    )
    .await
    .unwrap();
    let composition = catalog.load_file(&context, &config).await.unwrap();
    assert_eq!(composition.fibers().len(), 4);
    let owner = agent(&context);
    let commands = context.get(COMMANDS).unwrap();
    assert!(
        commands
            .list(&owner)
            .iter()
            .any(|command| command.name == "feedback")
    );
    let accepted = commands
        .execute(
            owner.clone(),
            "/feedback the diff view is unreadable",
            AbortSignal::default(),
        )
        .await
        .unwrap()
        .unwrap();
    let text = accepted.result.text().unwrap();
    assert!(
        text.starts_with("Feedback recorded for session feedback-loader-agent\nAnonymous user:")
    );
    assert!(text.ends_with("Session sharing is not configured."));
    let rejected = commands
        .execute(owner.clone(), "/feedback", AbortSignal::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        rejected.result.text(),
        Some("Feedback text is required. Usage: /feedback <text>")
    );
    let events = owner.session().events();
    assert_eq!(
        events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        [
            "command/run",
            "feedback/record",
            "command/done",
            "command/run",
            "command/done"
        ]
    );
    assert!(events[0].data.get("args").is_none());
    assert_eq!(events[1].data["text"], "the diff view is unreadable");
    assert_eq!(
        serde_json::to_string(&events)
            .unwrap()
            .matches("the diff view is unreadable")
            .count(),
        1
    );
    assert!(owner.session().derive_messages().is_empty());
    composition.dispose().await.unwrap();
    context.fiber().restart().await.unwrap();
}
