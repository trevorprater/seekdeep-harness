//! Behavioral mirror of `packages/feedback/command-feedback/tests/command-feedback.spec.ts`.

use std::sync::Arc;

use seekdeep_agent::{Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_command_feedback::apply;
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionId};
use seekdeep_llm::AbortSignal;
use seekdeep_scope::ScopeKey;

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

fn feedback_texts(session: &Session) -> Vec<String> {
    session
        .events()
        .iter()
        .filter(|event| event.event_type == "feedback/record")
        .map(|event| event.data["text"].as_str().unwrap_or_default().to_owned())
        .collect()
}

#[tokio::test]
async fn records_and_acknowledges_feedback() {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).expect("agents");
    let commands = seekdeep_commands::install(&context).expect("commands");
    apply(&context).expect("feedback command");

    let owner = agent("owner");
    let session = owner.session().clone();
    let execution = commands
        .execute(
            owner.clone(),
            "/feedback  helpful  ",
            AbortSignal::default(),
        )
        .await
        .expect("execute")
        .expect("command");
    let text = execution.result.text().expect("acknowledgement").to_owned();
    assert!(text.contains("Feedback recorded for session"), "{text}");
    assert!(text.contains("Anonymous user:"), "{text}");
    assert!(
        text.contains("Session sharing is not configured."),
        "{text}"
    );
    assert_eq!(feedback_texts(&session), ["helpful"]);

    let events = session.events();
    let types: Vec<&str> = events
        .iter()
        .map(|event| event.event_type.as_str())
        .collect();
    assert_eq!(types, ["command/run", "feedback/record", "command/done"]);
}

#[tokio::test]
async fn rejects_empty_and_whitespace_only_input() {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).expect("agents");
    let commands = seekdeep_commands::install(&context).expect("commands");
    apply(&context).expect("feedback command");

    let owner = agent("owner");
    let session = owner.session().clone();
    for input in ["/feedback", "/feedback    "] {
        let execution = commands
            .execute(owner.clone(), input, AbortSignal::default())
            .await
            .expect("execute")
            .expect("command");
        assert_eq!(
            execution.result.text(),
            Some("Feedback text is required. Usage: /feedback <text>")
        );
    }
    assert!(feedback_texts(&session).is_empty());
}
