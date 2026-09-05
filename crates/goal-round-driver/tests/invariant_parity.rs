//! Prompt invariant mirror of `packages/goal/goal-round-driver/tests/invariant.spec.ts`.

use std::sync::Arc;

use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{AppendOptions, Session, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_goal::{GoalActivation, GoalId, GoalPhase, GoalView};
use seekdeep_goal_round_driver::{register_invariant, render_goal_round_prompt};
use seekdeep_invariants::{InvariantConfig, InvariantError, InvariantRegistry};
use seekdeep_llm::{ContentBlock, MessageSource, UserMessage};
use serde_json::{Map, Value, json};

fn change() -> Value {
    json!({
        "kind": "goal/change",
        "version": 1,
        "operation": "create",
        "goal": {
            "id": "goal-round-driver-invariant",
            "revision": 1,
            "objective": "verify every continuation prompt",
            "phase": "active",
            "maxGoalRounds": 2,
        },
        "roundsStarted": 0,
        "createdAt": 1,
        "updatedAt": 1,
    })
}

fn view(rounds_started: u64) -> GoalView {
    GoalView {
        id: GoalId::new("goal-round-driver-invariant"),
        revision: 1,
        objective: "verify every continuation prompt".to_owned(),
        phase: GoalPhase::Active,
        blocked_reason: None,
        max_goal_rounds: 2,
        rounds_started,
        created_at: 1,
        updated_at: 1,
        activation: GoalActivation::Armed,
    }
}

fn goal_source(round: u64) -> MessageSource {
    MessageSource {
        kind: "goal".to_owned(),
        fields: Map::from_iter([
            ("goalId".to_owned(), json!("goal-round-driver-invariant")),
            ("revision".to_owned(), json!(1)),
            ("round".to_owned(), json!(round)),
        ]),
    }
}

fn append_change(session: &Session) {
    session
        .append("goal/change", change(), AppendOptions::default())
        .expect("goal change");
}

fn append_round(
    session: &Session,
    turn: u64,
    content: Option<Vec<ContentBlock>>,
) -> Result<(), seekdeep_core::session::SessionError> {
    let round = turn - 1;
    session.append(
        "turn/start",
        json!({"turn": turn}),
        AppendOptions::default(),
    )?;
    let message = UserMessage::new(
        content.unwrap_or_else(|| render_goal_round_prompt(&view(round - 1), round)),
        goal_source(round),
    );
    session.append(
        "user/message",
        serde_json::to_value(message).expect("message wire"),
        AppendOptions {
            surface_op: Some(SurfaceOp::append()),
            ..AppendOptions::default()
        },
    )?;
    session.append(
        "turn/end",
        json!({"turn": turn, "reason": {"kind": "completed"}}),
        AppendOptions::default(),
    )?;
    Ok(())
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

async fn install(context: &Context) -> Arc<InvariantRegistry> {
    let invariants =
        InvariantRegistry::install(context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&invariants).expect("driver invariant");
    registration.await_ready().await.expect("invariant ready");
    invariants
}

#[tokio::test]
async fn reconstructs_existing_rounds_and_accepts_the_next_canonical_prompt() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let session = create(&context, &sessions, "driver-invariant-replay");
    append_change(&session);
    append_round(&session, 2, None).expect("first round before invariant");
    install(&context).await;
    append_round(&session, 3, None).expect("next canonical round");

    create(&context, &sessions, "driver-invariant-dispatch");
    session
        .append("turn/start", json!({"turn": 4}), AppendOptions::default())
        .unwrap();
    session
        .append(
            "user/message",
            serde_json::to_value(UserMessage::new(
                vec![ContentBlock::Text {
                    text: "ordinary human message".to_owned(),
                }],
                MessageSource::user(),
            ))
            .unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("ordinary message");
    session
        .append(
            "turn/end",
            json!({"turn": 4, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .unwrap();

    session
        .append("turn/start", json!({"turn": 5}), AppendOptions::default())
        .unwrap();
    session
        .append(
            "user/message",
            serde_json::to_value(UserMessage::new(
                vec![ContentBlock::Text {
                    text: "round zero is not a driver continuation".to_owned(),
                }],
                goal_source(0),
            ))
            .unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("round zero ignored");
}

#[tokio::test]
async fn rejects_a_continuation_whose_content_differs_from_the_renderer() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    install(&context).await;
    let session = create(&context, &sessions, "driver-invariant-counterfeit");
    append_change(&session);
    let error = append_round(
        &session,
        2,
        Some(vec![ContentBlock::Text {
            text: "counterfeit continuation".to_owned(),
        }]),
    )
    .expect_err("counterfeit prompt");
    let rendered = error.to_string();
    assert!(
        rendered.contains("seekdeep-goal-round-driver"),
        "{rendered}"
    );
    assert!(rendered.contains("content does not match"), "{rendered}");
}

#[tokio::test]
async fn rejects_a_goal_round_without_a_reconstructable_active_goal() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    install(&context).await;
    let session = create(&context, &sessions, "driver-invariant-no-goal");
    let error = append_round(&session, 2, None).expect_err("missing goal state");
    assert!(
        error
            .to_string()
            .contains("cannot be reconstructed from the preceding durable goal state")
    );
    assert_eq!(session.seq(), 1, "rejected prompt reached the log");
}

#[tokio::test]
async fn attributes_an_invalid_durable_prefix_during_late_loading() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let session = create(&context, &sessions, "driver-invariant-bad-prefix");
    let mut malformed = change();
    malformed
        .as_object_mut()
        .unwrap()
        .insert("extra".to_owned(), json!(true));
    session
        .append("goal/change", malformed, AppendOptions::default())
        .expect("unvalidated bad prefix");
    append_round(&session, 2, None).expect("unvalidated prompt");
    let invariants =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&invariants).expect("registration");
    let error = registration
        .await_ready()
        .await
        .expect_err("late loading must reject the prefix");
    let invariant = error
        .downcast_ref::<InvariantError>()
        .expect("classified invariant");
    assert_eq!(invariant.package_name, "seekdeep-goal-round-driver");
    assert!(invariant.message.contains("cannot reconstruct the goal"));
}
