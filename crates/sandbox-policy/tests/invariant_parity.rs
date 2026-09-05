//! Replay and pre-commit durable sandbox-mode invariant parity.

use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, SessionEvent, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_sandbox::SandboxMode;
use seekdeep_sandbox_policy::{invariant::register_invariant, set_sandbox_mode};

fn raw_mode(mode: &str) -> SessionEvent {
    SessionEvent {
        event_type: "sandbox/mode".into(),
        seq: 0,
        time: 0,
        data: serde_json::json!({"mode": mode}),
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    }
}

async fn setup() -> (Context, std::sync::Arc<SessionStore>) {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let registration = register_invariant(&registry).unwrap();
    registration.await_ready().await.unwrap();
    (context, sessions)
}

#[tokio::test]
async fn accepts_all_modes_ignores_unrelated_events_and_rejects_before_commit() {
    let (context, sessions) = setup().await;
    let active = sessions
        .create(
            &context,
            Some(SessionId::new("sess-live")),
            CreateSessionOptions::default(),
        )
        .unwrap();
    for mode in [
        SandboxMode::ReadOnly,
        SandboxMode::WorkspaceWrite,
        SandboxMode::DangerFullAccess,
    ] {
        set_sandbox_mode(&active, mode).unwrap();
    }
    active
        .append(
            "turn/start",
            serde_json::json!({"turn": 1}),
            AppendOptions::default(),
        )
        .unwrap();
    let before = active.events();
    let error = active
        .append(
            "sandbox/mode",
            serde_json::json!({"mode": "host-root"}),
            AppendOptions::default(),
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("sandbox/mode carries unknown mode \"host-root\"")
    );
    assert_eq!(active.events(), before);
}

#[tokio::test]
async fn rejects_and_attributes_an_unknown_mode_already_present_on_registration() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    sessions
        .create(
            &context,
            Some(SessionId::new("sess-replay")),
            CreateSessionOptions {
                seed: Some(vec![raw_mode("host-root")]),
                ..CreateSessionOptions::default()
            },
        )
        .unwrap();
    let registry = InvariantRegistry::install(&context, &InvariantConfig::default()).unwrap();
    let registration = register_invariant(&registry).unwrap();
    let error = registration.await_ready().await.unwrap_err();
    assert!(error.to_string().contains("seekdeep-sandbox-policy"));
    assert!(
        error
            .to_string()
            .contains("sandbox/mode carries unknown mode \"host-root\"")
    );
}
