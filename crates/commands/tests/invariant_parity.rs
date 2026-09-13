//! Behavioral mirror of `packages/interaction/commands/tests/invariant.spec.ts`.

use std::sync::Arc;

use seekdeep_commands::register_invariant;
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use serde_json::json;

async fn setup(with_companion: bool) -> (Context, Arc<Session>, Arc<InvariantRegistry>) {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("commands-invariant")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    let invariants = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    if with_companion {
        let registration = register_invariant(&invariants).unwrap();
        registration.await_ready().await.unwrap();
    }
    (context, session, invariants)
}

fn append_run(session: &Session, id: &str) {
    session
        .append(
            "command/run",
            json!({
                "commandId": id,
                "name": "linked",
                "args": "",
                "source": {"kind": "user"}
            }),
            AppendOptions::default(),
        )
        .unwrap();
}

#[tokio::test]
async fn accepts_success_linked_to_an_earlier_non_command_event() {
    let (_, session, _) = setup(true).await;
    let source = session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .unwrap();
    append_run(&session, "cmd-valid");
    session
        .append(
            "command/done",
            json!({
                "commandId": "cmd-valid",
                "kind": "success",
                "sourceEventSeq": source.seq
            }),
            AppendOptions::default(),
        )
        .unwrap();
}

#[tokio::test]
async fn rejects_fractional_negative_non_prior_and_command_source_references() {
    for (index, source) in [json!(-1), json!(1.5), json!(1)].into_iter().enumerate() {
        let context = Context::new();
        let sessions = SessionStore::install(&context).unwrap();
        let session = sessions
            .create(
                &context,
                Some(SessionId::new(format!("invalid-{index}"))),
                CreateSessionOptions::default(),
            )
            .unwrap();
        let invariants = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
        let registration = register_invariant(&invariants).unwrap();
        registration.await_ready().await.unwrap();
        append_run(&session, "cmd-invalid");
        let error = session
            .append(
                "command/done",
                json!({
                    "commandId": "cmd-invalid",
                    "kind": "success",
                    "sourceEventSeq": source
                }),
                AppendOptions::default(),
            )
            .unwrap_err();
        assert!(error.to_string().contains("seekdeep-commands"));
        assert!(error.to_string().contains("invalid sourceEventSeq"));
    }
}

#[tokio::test]
async fn rejects_error_settlement_with_success_only_source() {
    let (_, session, _) = setup(true).await;
    let source = session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .unwrap();
    append_run(&session, "cmd-error-source");
    let error = session
        .append(
            "command/done",
            json!({
                "commandId": "cmd-error-source",
                "kind": "error",
                "text": "failed",
                "sourceEventSeq": source.seq
            }),
            AppendOptions::default(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("invalid sourceEventSeq"));
}

#[tokio::test]
async fn rejects_unpaired_done_and_duplicate_run() {
    let (_, session, _) = setup(true).await;
    let error = session
        .append(
            "command/done",
            json!({"commandId": "missing", "kind": "success"}),
            AppendOptions::default(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("pairs no prior command/run"));
    append_run(&session, "same");
    let error = session
        .append(
            "command/run",
            json!({
                "commandId": "same", "name": "linked", "args": "",
                "source": {"kind": "user"}
            }),
            AppendOptions::default(),
        )
        .unwrap_err();
    assert!(error.to_string().contains("repeats commandId"));
}

#[tokio::test]
async fn late_registration_attributes_invalid_durable_prefix() {
    let (_, session, invariants) = setup(false).await;
    append_run(&session, "cmd-late");
    session
        .append(
            "command/done",
            json!({
                "commandId": "cmd-late",
                "kind": "success",
                "sourceEventSeq": 0
            }),
            AppendOptions::default(),
        )
        .unwrap();
    let registration = register_invariant(&invariants).unwrap();
    let error = registration.await_ready().await.unwrap_err();
    assert!(error.to_string().contains("seekdeep-commands"));
    assert!(error.to_string().contains("invalid sourceEventSeq"));
}
