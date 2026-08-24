//! Service, lifecycle, persistence, and fork mirror of the source goal suite.

use std::{collections::VecDeque, sync::Arc};

use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentEvents, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications,
    SessionStartSource,
};
use seekdeep_agent_loop::SessionStartEvent;
use seekdeep_cordis::{Context, EventOptions, EventReply, Fiber};
use seekdeep_core::{
    session::{AppendOptions, Session, SessionEvent, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_goal::{
    Config, CreateGoalRequest, EditGoalRequest, GOAL, GoalActivation, GoalEnvironment,
    GoalErrorCode, GoalId, GoalOperation, GoalPhase, GoalRef, GoalService, GoalView,
    fold::fold_goal, runtime::GoalError,
};
use seekdeep_scope::ScopeKey;
use serde_json::{Value, json};

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug)]
struct ScriptedEnvironment {
    times: Mutex<VecDeque<u64>>,
    ids: Mutex<VecDeque<GoalId>>,
}

impl ScriptedEnvironment {
    fn new(
        times: impl IntoIterator<Item = u64>,
        ids: impl IntoIterator<Item = &'static str>,
    ) -> Arc<Self> {
        Arc::new(Self {
            times: Mutex::new(times.into_iter().collect()),
            ids: Mutex::new(ids.into_iter().map(GoalId::new).collect()),
        })
    }
}

impl GoalEnvironment for ScriptedEnvironment {
    fn now_millis(&self) -> u64 {
        self.times.lock().pop_front().expect("scripted goal time")
    }

    fn goal_id(&self, _session: &Session, _now: u64) -> GoalId {
        self.ids.lock().pop_front().expect("scripted goal id")
    }
}

struct Harness {
    context: Context,
    sessions: Arc<SessionStore>,
    registry: Arc<AgentRegistry>,
    agent: Arc<Agent>,
    goals: Arc<GoalService>,
}

impl Harness {
    fn new(
        id: &str,
        config: Config,
        times: impl IntoIterator<Item = u64>,
        ids: impl IntoIterator<Item = &'static str>,
    ) -> Self {
        let context = Context::new();
        let sessions = SessionStore::install(&context).expect("sessions");
        let registry = Arc::new(AgentRegistry::new(context.clone()));
        registry.provide(&context).expect("agents");
        let session = sessions
            .create(
                &context,
                Some(SessionId::new(id)),
                CreateSessionOptions::default(),
            )
            .expect("session");
        let agent = agent_for(&context, session);
        registry
            .register(&context, &agent, None)
            .expect("register agent");
        let goals = GoalService::new_with_environment(
            &context,
            config,
            ScriptedEnvironment::new(times, ids),
        )
        .expect("goal service");
        goals.provide(&context).expect("provide goals");
        Self {
            context,
            sessions,
            registry,
            agent,
            goals,
        }
    }
}

fn agent_for(context: &Context, session: Arc<Session>) -> Arc<Agent> {
    let inbox =
        Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).expect("inbox"));
    Arc::new(Agent::new(
        session.id().clone(),
        AgentOptions::default(),
        session,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ))
}

fn reference(goal: &GoalView) -> GoalRef {
    GoalRef {
        id: goal.id.clone(),
        revision: goal.revision,
    }
}

fn code(error: &anyhow::Error) -> GoalErrorCode {
    error
        .downcast_ref::<GoalError>()
        .expect("classified goal error")
        .code
}

fn append_round(session: &Session, goal: &GoalRef, round: u64) {
    let turn = round;
    session
        .append(
            "turn/start",
            json!({"turn": turn}),
            AppendOptions::default(),
        )
        .expect("turn start");
    session
        .append(
            "user/message",
            json!({
                "id": format!("goal-round-{round}"),
                "role": "user",
                "content": [{"type": "text", "text": format!("round {round}")}],
                "source": {
                    "kind": "goal", "goalId": goal.id.as_str(),
                    "revision": goal.revision, "round": round,
                },
            }),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("goal message");
    session
        .append(
            "turn/end",
            json!({"turn": turn, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("turn end");
}

#[test]
fn creation_applies_defaults_validates_safe_integers_and_writes_only_a_durable_change() {
    let harness = Harness::new(
        "goal-create",
        Config {
            default_max_goal_rounds: Some(17),
        },
        [1_700_000_000_000],
        ["goal-created"],
    );
    let created = harness
        .goals
        .create(
            &harness.agent,
            &CreateGoalRequest {
                objective: "  finish the feature  ".to_owned(),
                max_goal_rounds: None,
            },
        )
        .expect("create");
    assert_eq!(created.objective, "finish the feature");
    assert_eq!(created.phase, GoalPhase::Active);
    assert_eq!(created.revision, 1);
    assert_eq!(created.max_goal_rounds, 17);
    assert_eq!(created.rounds_started, 0);
    assert_eq!(
        (created.created_at, created.updated_at),
        (1_700_000_000_000, 1_700_000_000_000)
    );
    assert_eq!(created.activation, GoalActivation::Armed);
    assert!(harness.agent.inbox().next_step().is_empty());
    assert!(harness.agent.session().derive_messages().is_empty());

    let events = harness.agent.session().events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_type, "goal/change");
    assert_eq!(events[0].data["operation"], "create");
    assert_eq!(events[0].data["goal"]["id"], created.id.as_str());
    assert!(events[0].data.pointer("/goal/activation").is_none());
    assert_eq!(
        fold_goal(&events).expect("fold").goal.unwrap().id,
        created.id
    );

    assert_eq!(
        code(
            &harness
                .goals
                .create(
                    &harness.agent,
                    &CreateGoalRequest {
                        objective: " ".to_owned(),
                        max_goal_rounds: None,
                    },
                )
                .expect_err("blank objective"),
        ),
        GoalErrorCode::GoalInvalidObjective
    );
    for invalid in [0, MAX_SAFE_INTEGER + 1] {
        assert_eq!(
            code(
                &harness
                    .goals
                    .create(
                        &harness.agent,
                        &CreateGoalRequest {
                            objective: "x".to_owned(),
                            max_goal_rounds: Some(invalid),
                        },
                    )
                    .expect_err("invalid round cap"),
            ),
            GoalErrorCode::GoalInvalidMaxRounds
        );
    }

    let context = Context::new();
    let invalid = GoalService::new(
        &context,
        Config {
            default_max_goal_rounds: Some(MAX_SAFE_INTEGER + 1),
        },
    )
    .err()
    .expect("invalid direct configuration");
    assert_eq!(code(&invalid), GoalErrorCode::GoalInvalidMaxRounds);
}

#[test]
#[allow(clippy::too_many_lines)]
fn edits_use_compare_and_set_and_lifecycle_transitions_match_the_source() {
    let harness = Harness::new(
        "goal-lifecycle",
        Config::default(),
        100..120,
        ["goal-lifecycle", "goal-replacement"],
    );
    let created = harness
        .goals
        .create(
            &harness.agent,
            &CreateGoalRequest {
                objective: "old".to_owned(),
                max_goal_rounds: Some(4),
            },
        )
        .expect("create");
    assert_eq!(
        code(
            &harness
                .goals
                .edit(
                    &harness.agent,
                    &reference(&created),
                    &EditGoalRequest {
                        objective: None,
                        max_goal_rounds: None,
                    },
                )
                .expect_err("empty edit"),
        ),
        GoalErrorCode::GoalInvalidEdit
    );
    let edited = harness
        .goals
        .edit(
            &harness.agent,
            &reference(&created),
            &EditGoalRequest {
                objective: Some(" new ".to_owned()),
                max_goal_rounds: Some(8),
            },
        )
        .expect("edit");
    assert_eq!(
        (
            edited.objective.as_str(),
            edited.max_goal_rounds,
            edited.revision
        ),
        ("new", 8, 2)
    );
    assert_eq!(
        code(
            &harness
                .goals
                .pause(&harness.agent, &reference(&created))
                .expect_err("stale ref"),
        ),
        GoalErrorCode::GoalStaleRevision
    );

    let paused = harness
        .goals
        .pause(&harness.agent, &reference(&edited))
        .expect("pause");
    assert_eq!(
        (paused.phase, paused.activation),
        (GoalPhase::Paused, GoalActivation::Disarmed)
    );
    assert_eq!(
        code(
            &harness
                .goals
                .pause(&harness.agent, &reference(&paused))
                .expect_err("double pause"),
        ),
        GoalErrorCode::GoalInvalidTransition
    );
    let resumed = harness
        .goals
        .resume(&harness.agent, &reference(&paused))
        .expect("resume");
    assert_eq!(
        (resumed.phase, resumed.activation),
        (GoalPhase::Active, GoalActivation::Armed)
    );
    let blocked = harness
        .goals
        .block(
            &harness.agent,
            &reference(&resumed),
            &json!({"code": "needs-input", "message": "  A choice is required.  "}),
        )
        .expect("block");
    assert_eq!(blocked.phase, GoalPhase::Blocked);
    assert_eq!(
        blocked.blocked_reason.as_ref().unwrap().message,
        "A choice is required."
    );
    let resumed = harness
        .goals
        .resume(&harness.agent, &reference(&blocked))
        .expect("resume blocked");
    let complete = harness
        .goals
        .complete(&harness.agent, &reference(&resumed))
        .expect("complete");
    assert_eq!(
        (complete.phase, complete.activation),
        (GoalPhase::Complete, GoalActivation::Disarmed)
    );
    assert_eq!(
        code(
            &harness
                .goals
                .resume(&harness.agent, &reference(&complete))
                .expect_err("resume complete"),
        ),
        GoalErrorCode::GoalInvalidTransition
    );

    let replacement = harness
        .goals
        .create(
            &harness.agent,
            &CreateGoalRequest {
                objective: "replacement".to_owned(),
                max_goal_rounds: None,
            },
        )
        .expect("replace completed goal");
    assert_eq!(replacement.id, GoalId::new("goal-replacement"));
    assert_eq!(replacement.revision, 1);
}

#[test]
fn blocker_validation_round_caps_timestamps_clear_and_remote_creation_are_exact() {
    let harness = Harness::new(
        "goal-round-cap",
        Config::default(),
        [100, 90, 80, 70, 60, 50],
        ["goal-bounded", "goal-fresh"],
    );
    let created = harness
        .goals
        .create_remote(
            &harness.agent,
            &CreateGoalRequest {
                objective: "bounded".to_owned(),
                max_goal_rounds: Some(2),
            },
        )
        .expect("remote create");
    for reason in [
        Value::Null,
        json!([]),
        json!({"code": 1, "message": "invalid"}),
        json!({"code": "Not Canonical", "message": "invalid"}),
        json!({"code": "round-limit", "message": " "}),
    ] {
        assert_eq!(
            code(
                &harness
                    .goals
                    .block(&harness.agent, &created.goal_ref, &reason)
                    .expect_err("invalid blocker"),
            ),
            GoalErrorCode::GoalInvalidBlockReason
        );
    }

    append_round(harness.agent.session(), &created.goal_ref, 1);
    append_round(harness.agent.session(), &created.goal_ref, 2);
    assert_eq!(
        harness
            .goals
            .get(&harness.agent)
            .unwrap()
            .unwrap()
            .rounds_started,
        2
    );
    let blocked = harness
        .goals
        .block(
            &harness.agent,
            &created.goal_ref,
            &json!({"code": "round-limit", "message": "Goal round limit reached."}),
        )
        .expect("block at limit");
    assert_eq!(blocked.updated_at, 100, "backward time must clamp");
    assert_eq!(
        code(
            &harness
                .goals
                .resume(&harness.agent, &reference(&blocked))
                .expect_err("exhausted resume"),
        ),
        GoalErrorCode::GoalInvalidTransition
    );
    let expanded = harness
        .goals
        .edit(
            &harness.agent,
            &reference(&blocked),
            &EditGoalRequest {
                objective: None,
                max_goal_rounds: Some(3),
            },
        )
        .expect("expand cap");
    assert!(expanded.blocked_reason.is_some());
    let resumed = harness
        .goals
        .resume(&harness.agent, &reference(&expanded))
        .expect("resume after expansion");
    assert!(resumed.blocked_reason.is_none());
    let tombstone = harness
        .goals
        .clear(&harness.agent, &reference(&resumed))
        .expect("clear");
    assert_eq!(tombstone.revision, resumed.revision + 1);
    assert!(harness.goals.get(&harness.agent).unwrap().is_none());
    assert_eq!(
        code(
            &harness
                .goals
                .clear(&harness.agent, &reference(&resumed))
                .expect_err("clear absent goal"),
        ),
        GoalErrorCode::GoalNotFound
    );
    let clear = harness.agent.session().events().pop().expect("clear event");
    assert_eq!(clear.data["clearedAt"], 100);
}

#[test]
#[allow(clippy::too_many_lines)]
fn exact_live_identity_external_appends_corruption_and_reentrant_observers_are_safe() {
    let harness = Harness::new("goal-observers", Config::default(), [12], ["goal-observed"]);
    let impostor_session =
        Session::create(harness.agent.id(), None, None).expect("impostor session");
    let impostor = agent_for(&harness.context, impostor_session);
    assert_eq!(
        code(&harness.goals.get(&impostor).expect_err("impostor read")),
        GoalErrorCode::GoalAgentNotLive
    );

    let observed = Arc::new(Mutex::new(None::<GoalView>));
    let observed_for_listener = observed.clone();
    let agent = harness.agent.clone();
    let goals = harness.goals.clone();
    harness
        .context
        .events()
        .on_sync(
            &harness.context,
            "session/event",
            move |_, args| {
                let session = args.get::<Session>(0).expect("session");
                let event = args.get::<SessionEvent>(1).expect("event");
                if Arc::ptr_eq(&session, agent.session()) && event.event_type == "goal/change" {
                    *observed_for_listener.lock() = goals.get(&agent)?;
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .expect("session observer");

    let notification_operations = Arc::new(Mutex::new(Vec::new()));
    harness
        .context
        .events()
        .on_sync(
            &harness.context,
            "goal/changed",
            |_, _| anyhow::bail!("broken observer"),
            EventOptions::default(),
        )
        .expect("broken listener");
    let seen = notification_operations.clone();
    harness
        .context
        .events()
        .on_sync(
            &harness.context,
            "goal/changed",
            move |_, args| {
                let event = args
                    .get::<seekdeep_agent::AgentEvent<seekdeep_goal::GoalChangedEvent>>(0)
                    .expect("goal changed");
                seen.lock().push(event.payload.change.operation);
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .expect("later listener");

    let created = harness
        .goals
        .create(
            &harness.agent,
            &CreateGoalRequest {
                objective: "publish once".to_owned(),
                max_goal_rounds: Some(4),
            },
        )
        .expect("create despite observer failure");
    assert_eq!(observed.lock().as_ref(), Some(&created));
    assert_eq!(*notification_operations.lock(), [GoalOperation::Create]);

    harness
        .agent
        .session()
        .append(
            "goal/change",
            json!({
                "kind": "goal/change", "version": 1, "operation": "edit",
                "goal": {
                    "id": created.id.as_str(), "revision": 2, "objective": "external append",
                    "phase": "active", "maxGoalRounds": 4,
                },
                "roundsStarted": 0, "createdAt": 12, "updatedAt": 13,
            }),
            AppendOptions::default(),
        )
        .expect("external valid append");
    assert_eq!(
        harness
            .goals
            .get(&harness.agent)
            .unwrap()
            .unwrap()
            .objective,
        "external append"
    );
    harness
        .agent
        .session()
        .append(
            "goal/change",
            json!({"kind": "goal/change", "version": 999}),
            AppendOptions::default(),
        )
        .expect("unvalidated corruption without invariant");
    let first = harness.goals.get(&harness.agent).unwrap_err().to_string();
    let second = harness.goals.get(&harness.agent).unwrap_err().to_string();
    assert_eq!(first, second);
}

#[test]
fn session_store_fork_inherits_the_closed_goal_prefix_disarmed() {
    let harness = Harness::new("goal-fork-parent", Config::default(), [10], ["goal-forked"]);
    let created = harness
        .goals
        .create(
            &harness.agent,
            &CreateGoalRequest {
                objective: "inherit through fork".to_owned(),
                max_goal_rounds: Some(5),
            },
        )
        .expect("create");
    append_round(harness.agent.session(), &reference(&created), 1);
    let child_session = harness
        .sessions
        .fork(
            &harness.context,
            harness.agent.session(),
            None,
            Some(SessionId::new("goal-fork-child")),
        )
        .expect("fork");
    let child = agent_for(&harness.context, child_session.clone());
    harness
        .registry
        .register(&harness.context, &child, None)
        .expect("child live");
    let inherited = harness.goals.get(&child).unwrap().unwrap();
    assert_eq!(inherited.id, created.id);
    assert_eq!(inherited.objective, created.objective);
    assert_eq!(inherited.rounds_started, 1);
    assert_eq!(inherited.activation, GoalActivation::Disarmed);
    assert_eq!(
        child_session.header().parent_session,
        Some(harness.agent.id().clone())
    );
    assert_eq!(
        child_session.header().seed_length,
        Some(harness.agent.session().seq())
    );
}

#[tokio::test]
async fn providing_fiber_owns_the_service_and_session_start_listener() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let registry = Arc::new(AgentRegistry::new(context.clone()));
    registry.provide(&context).expect("agents");
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("goal-hmr")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    let agent = agent_for(&context, session);
    registry.register(&context, &agent, None).expect("agent");

    let first_fiber = Fiber::active_child("goal generation one");
    let first_context = context.with_fiber(first_fiber.clone());
    let first = GoalService::new_with_environment(
        &first_context,
        Config::default(),
        ScriptedEnvironment::new([10], ["goal-hmr-id"]),
    )
    .unwrap();
    first.provide(&first_context).unwrap();
    let created = first
        .create(
            &agent,
            &CreateGoalRequest {
                objective: "survive service reload".to_owned(),
                max_goal_rounds: None,
            },
        )
        .unwrap();
    first_fiber.dispose().await.unwrap();
    assert!(context.get(GOAL).is_none());
    AgentEvents::new(context.clone(), agent.clone()).emit(
        "agent/session-start",
        SessionStartEvent {
            source: SessionStartSource::Resume,
        },
    );
    assert_eq!(
        first.get(&agent).unwrap().unwrap().activation,
        GoalActivation::Armed
    );

    let second_fiber = Fiber::active_child("goal generation two");
    let second_context = context.with_fiber(second_fiber.clone());
    let second = GoalService::new_with_environment(
        &second_context,
        Config::default(),
        ScriptedEnvironment::new([], []),
    )
    .unwrap();
    second.provide(&second_context).unwrap();
    let restored = second.get(&agent).unwrap().unwrap();
    assert_eq!(restored.id, created.id);
    assert_eq!(restored.activation, GoalActivation::Disarmed);
    second_fiber.dispose().await.unwrap();
}
