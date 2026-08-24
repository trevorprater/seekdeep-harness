//! Goal projection lifecycle mirror of `packages/goal/goal/tests/projection.spec.ts`.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentOptions, AgentRegistry, Inbox, InboxTarget, NoopInboxNotifications,
};
use seekdeep_cordis::{Context, Fiber};
use seekdeep_core::{
    session::{AppendOptions, Session, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_goal::{
    Config, CreateGoalRequest, GoalEnvironment, GoalId, GoalRef, GoalService, GoalView,
    apply_goal_projection,
};
use seekdeep_llm::{ContentBlock, MessageSource, UserMessage};
use seekdeep_scope::ScopeKey;
use seekdeep_session_projection::SessionProjectionRegistry;
use serde_json::json;

#[derive(Debug)]
struct FixedEnvironment;

impl GoalEnvironment for FixedEnvironment {
    fn now_millis(&self) -> u64 {
        100
    }

    fn goal_id(&self, _session: &Session, _now: u64) -> GoalId {
        GoalId::new("goal-projection")
    }
}

fn setup() -> (Context, Arc<SessionStore>, Arc<AgentRegistry>, Arc<Agent>) {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).expect("agents");
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("goal-projection-session")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    let inbox =
        Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
    let agent = Arc::new(Agent::new(
        session.id().clone(),
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ));
    agents.register(&context, &agent, None).expect("agent");
    (context, sessions, agents, agent)
}

fn goal_service(context: &Context) -> Arc<GoalService> {
    let goals =
        GoalService::new_with_environment(context, Config::default(), Arc::new(FixedEnvironment))
            .expect("goals");
    goals.provide(context).expect("provide goals");
    goals
}

fn reference(goal: &GoalView) -> GoalRef {
    GoalRef {
        id: goal.id.clone(),
        revision: goal.revision,
    }
}

#[test]
fn projection_is_null_before_create_tracks_mutations_and_stays_null_after_clear_and_inbox_work() {
    let (context, _sessions, _agents, agent) = setup();
    let projections = SessionProjectionRegistry::install(&context).expect("projections");
    let goals = goal_service(&context);

    let empty = projections
        .snapshot(agent.session())
        .expect("empty snapshot");
    assert_eq!(empty.as_of_seq, -1);
    assert_eq!(empty.values.len(), 1);
    assert_eq!(empty.values["goal"], json!(null));

    let created = goals
        .create(
            &agent,
            &CreateGoalRequest {
                objective: "ship the goal bar".to_owned(),
                max_goal_rounds: None,
            },
        )
        .expect("create");
    let after_create = projections.snapshot(agent.session()).expect("after create");
    assert_eq!(after_create.as_of_seq, 0);
    assert_eq!(
        after_create.values["goal"]["goal"]["id"],
        created.id.as_str()
    );
    assert_eq!(after_create.values["goal"]["goal"]["phase"], "active");
    assert_eq!(after_create.values["goal"]["roundsStarted"], 0);

    let paused = goals.pause(&agent, &reference(&created)).expect("pause");
    let after_pause = projections.snapshot(agent.session()).expect("after pause");
    assert_eq!(
        after_pause.values["goal"]["goal"]["revision"],
        paused.revision
    );
    assert_eq!(after_pause.values["goal"]["goal"]["phase"], "paused");
    goals.clear(&agent, &reference(&paused)).expect("clear");
    assert_eq!(
        projections.snapshot(agent.session()).unwrap().values["goal"],
        json!(null)
    );

    agent
        .inbox()
        .prepend(
            InboxTarget::NextStep,
            UserMessage::new(
                vec![ContentBlock::Text {
                    text: "unrelated pending context".to_owned(),
                }],
                MessageSource::plugin("test"),
            ),
        )
        .expect("inbox prepend");
    assert_eq!(
        projections.snapshot(agent.session()).unwrap().values["goal"],
        json!(null)
    );
}

#[test]
fn malformed_and_non_goal_events_leave_projection_unchanged_without_notifications() {
    let (context, _sessions, _agents, agent) = setup();
    let projections = SessionProjectionRegistry::install(&context).expect("projections");
    let _goals = goal_service(&context);
    let notifications = Arc::new(Mutex::new(Vec::new()));
    let seen = notifications.clone();
    projections
        .on_changed(
            &context,
            Arc::new(move |_, key, value, seq| {
                seen.lock().push((key.to_owned(), value.clone(), seq));
                Ok(())
            }),
        )
        .expect("listener");

    agent
        .session()
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("unrelated");
    agent
        .session()
        .append(
            "goal/change",
            json!({"kind": "goal/change", "version": 1, "operation": "create"}),
            AppendOptions::default(),
        )
        .expect("malformed without invariant");
    agent
        .session()
        .append(
            "goal/change",
            json!({"kind": "not-a-goal-change"}),
            AppendOptions::default(),
        )
        .expect("foreign kind");
    assert!(notifications.lock().is_empty());
    assert_eq!(
        projections.snapshot(agent.session()).unwrap().values["goal"],
        json!(null)
    );

    let current = seekdeep_goal::GoalProjection {
        goal: seekdeep_goal::GoalSnapshot {
            id: GoalId::new("g1"),
            revision: 1,
            objective: "x".to_owned(),
            phase: seekdeep_goal::GoalPhase::Active,
            blocked_reason: None,
            max_goal_rounds: 4,
        },
        rounds_started: 0,
        created_at: 1,
        updated_at: 1,
    };
    let unrelated = agent.session().events()[0].clone();
    assert_eq!(
        apply_goal_projection(Some(current.clone()), &unrelated),
        Some(current)
    );
}

#[tokio::test]
async fn goal_fiber_disposal_drops_the_projection_key() {
    let (context, _sessions, _agents, agent) = setup();
    let projections = SessionProjectionRegistry::install(&context).expect("projections");
    assert!(
        projections
            .snapshot(agent.session())
            .unwrap()
            .values
            .is_empty()
    );

    let goal_fiber = Fiber::active_child("goal projection generation");
    let goal_context = context.with_fiber(goal_fiber.clone());
    let goals = goal_service(&goal_context);
    assert_eq!(
        projections.snapshot(agent.session()).unwrap().values["goal"],
        json!(null)
    );
    goals
        .create(
            &agent,
            &CreateGoalRequest {
                objective: "survive projection reload".to_owned(),
                max_goal_rounds: None,
            },
        )
        .unwrap();
    goal_fiber.dispose().await.unwrap();
    assert!(
        projections
            .snapshot(agent.session())
            .unwrap()
            .values
            .is_empty()
    );
}

#[tokio::test]
async fn optional_projection_dependency_reconciles_when_the_registry_appears_disappears_and_reappears()
 {
    let (context, _sessions, _agents, agent) = setup();
    let goals = goal_service(&context);
    let created = goals
        .create(
            &agent,
            &CreateGoalRequest {
                objective: "late projection registry".to_owned(),
                max_goal_rounds: None,
            },
        )
        .unwrap();

    let first_fiber = Fiber::active_child("projection registry one");
    let first_context = context.with_fiber(first_fiber.clone());
    let first = SessionProjectionRegistry::install(&first_context).unwrap();
    assert_eq!(
        first.snapshot(agent.session()).unwrap().values["goal"]["goal"]["id"],
        created.id.as_str()
    );
    first_fiber.dispose().await.unwrap();
    assert!(first.snapshot(agent.session()).unwrap().values.is_empty());

    let second_fiber = Fiber::active_child("projection registry two");
    let second_context = context.with_fiber(second_fiber.clone());
    let second = SessionProjectionRegistry::install(&second_context).unwrap();
    assert_eq!(
        second.snapshot(agent.session()).unwrap().values["goal"]["goal"]["id"],
        created.id.as_str()
    );
    second_fiber.dispose().await.unwrap();
}
