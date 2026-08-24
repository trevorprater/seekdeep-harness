//! Goal invariant companion mirror of `packages/goal/goal/tests/invariant.spec.ts`.

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_goal::invariant::register_invariant;
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use serde_json::{Value, json};

fn change() -> Value {
    json!({
        "kind": "goal/change",
        "version": 1,
        "operation": "create",
        "goal": {
            "id": "goal-invariant",
            "revision": 1,
            "objective": "check the stream",
            "phase": "active",
            "maxGoalRounds": 2,
        },
        "roundsStarted": 0,
        "createdAt": 1,
        "updatedAt": 1,
    })
}

async fn setup() -> (Context, Arc<SessionStore>) {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let invariants =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&invariants).expect("goal invariant");
    registration.await_ready().await.expect("invariant ready");
    (context, sessions)
}

fn create(context: &Context, sessions: &SessionStore, id: &str) -> Arc<Session> {
    sessions
        .create(
            context,
            Some(SessionId::new(id)),
            CreateSessionOptions::default(),
        )
        .expect("session")
}

fn append_round(session: &Session) -> anyhow::Result<()> {
    session.append("turn/start", json!({"turn": 1}), AppendOptions::default())?;
    session.append(
        "user/message",
        json!({
            "id": "goal-invariant-round",
            "role": "user",
            "content": [{"type": "text", "text": "continue"}],
            "source": {
                "kind": "goal", "goalId": "goal-invariant", "revision": 1, "round": 1,
            },
        }),
        AppendOptions {
            surface_op: Some(SurfaceOp::append()),
            ..AppendOptions::default()
        },
    )?;
    Ok(())
}

#[tokio::test]
async fn accepts_canonical_goal_snapshots_and_sequential_admitted_rounds() {
    let (context, sessions) = setup().await;
    let session = create(&context, &sessions, "goal-invariant-valid");
    session
        .append("goal/change", change(), AppendOptions::default())
        .expect("goal change");
    append_round(&session).expect("admitted round");
}

#[tokio::test]
async fn rejects_malformed_change_before_commit_and_keeps_the_fold_reusable() {
    let (context, sessions) = setup().await;
    let session = create(&context, &sessions, "goal-invariant-invalid");
    let mut malformed = change();
    malformed
        .as_object_mut()
        .expect("change object")
        .insert("extra".to_owned(), json!(true));
    let error = session
        .append("goal/change", malformed, AppendOptions::default())
        .expect_err("malformed change must be rejected");
    let rendered = error.to_string();
    assert!(rendered.contains("seekdeep-goal"), "{rendered}");
    assert!(rendered.contains("must have exactly"), "{rendered}");
    assert_eq!(session.seq(), 0, "rejected candidate reached the log");
    session
        .append("goal/change", change(), AppendOptions::default())
        .expect("fold remains reusable");
    assert_eq!(session.seq(), 1);
}

#[tokio::test]
async fn late_registration_reconstructs_existing_durable_goal_before_later_rounds() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let session = create(&context, &sessions, "goal-invariant-late-load");
    session
        .append("goal/change", change(), AppendOptions::default())
        .expect("preexisting goal");

    let invariants =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&invariants).expect("goal invariant");
    registration
        .await_ready()
        .await
        .expect("seed existing stream");
    append_round(&session).expect("round after late load");
}
