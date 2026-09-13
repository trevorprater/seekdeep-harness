//! Cold-restart proof through a real Rust Loader composition and JSONL persistence.

mod support;

use seekdeep_core::session::{SessionId, derive_event_message};
use seekdeep_loader::PluginCatalog;
use seekdeep_message_feedback::{
    MESSAGE_FEEDBACK, MessageFeedbackPutRequest, MessageFeedbackRating,
};
use seekdeep_session_persistence::SESSION_PERSISTENCE;

fn catalog() -> PluginCatalog {
    let catalog = PluginCatalog::new();
    for (name, plugin) in [
        ("fixture:sessions", seekdeep_core::session_store::plugin()),
        (
            "fixture:session-persistence-jsonl",
            seekdeep_session_persistence_jsonl::plugin(),
        ),
        ("fixture:storage", seekdeep_storage::plugin()),
        ("fixture:storage-json", seekdeep_storage_json::plugin()),
        ("fixture:storage-domain", seekdeep_storage_domain::plugin()),
        (
            "fixture:message-feedback",
            seekdeep_message_feedback::plugin(),
        ),
    ] {
        catalog.register_named(name, plugin).unwrap();
    }
    catalog
}

fn composition(root: &std::path::Path) -> String {
    format!(
        concat!(
            "- id: sessions\n  name: fixture:sessions\n",
            "- id: persistence\n  name: fixture:session-persistence-jsonl\n  config:\n",
            "    root: {}\n    compression: none\n    writeBatchMaxDelayMs: 1\n",
            "- id: storage\n  name: fixture:storage\n",
            "- id: storage-json\n  name: fixture:storage-json\n  config:\n    root: {}\n",
            "- id: storage-domain\n  name: fixture:storage-domain\n  config:\n    backend: json\n",
            "- id: feedback\n  name: fixture:message-feedback\n  config:\n    maxNoteBytes: 32\n",
        ),
        serde_json::to_string(&root.join("sessions").to_string_lossy()).unwrap(),
        serde_json::to_string(&root.join("storage").to_string_lossy()).unwrap(),
    )
}

async fn load(root: &std::path::Path) -> seekdeep_cordis::Context {
    let context = seekdeep_cordis::Context::new();
    let loaded = catalog()
        .load_yaml(&context, &composition(root))
        .await
        .unwrap();
    assert!(
        loaded
            .entries()
            .iter()
            .filter(|entry| !entry.group && !entry.disabled)
            .all(|entry| entry.state == Some(seekdeep_cordis::FiberState::Active))
    );
    context
}

#[tokio::test]
async fn checkpointed_target_and_feedback_sidecar_survive_a_cold_loader_restart() {
    let root = tempfile::tempdir().unwrap();
    let first = load(root.path()).await;
    let sessions = first.get(seekdeep_core::session_store::SESSIONS).unwrap();
    let session = sessions
        .create(
            &first,
            Some(SessionId::new("loader-feedback")),
            seekdeep_core::session_store::CreateSessionOptions {
                cwd: Some(root.path().to_string_lossy().into_owned()),
                ..Default::default()
            },
        )
        .unwrap();
    let fixture = support::append_message_fixture(session.clone());
    let feedback = first.get(MESSAGE_FEEDBACK).unwrap();
    let item = feedback
        .put(MessageFeedbackPutRequest {
            session_id: session.id().clone(),
            message_id: fixture.assistant_message_ids[0].clone(),
            rating: MessageFeedbackRating::Positive,
            note: Some("survives restart".to_owned()),
            if_version: None,
        })
        .await
        .unwrap()
        .unwrap();
    let durable = first
        .get(SESSION_PERSISTENCE)
        .unwrap()
        .persistence()
        .read_from(session.id(), 0, None)
        .await
        .unwrap();
    assert!(durable.events.iter().any(|event| {
        derive_event_message(event).is_some_and(|message| message.id() == &item.message_id)
    }));
    first.fiber().dispose().await.unwrap();

    let second = load(root.path()).await;
    let listed = second
        .get(MESSAGE_FEEDBACK)
        .unwrap()
        .list(session.id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(listed, [item]);
    second.fiber().dispose().await.unwrap();
}
