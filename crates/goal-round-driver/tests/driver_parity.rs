//! Real-loop race and outcome mirror of the source goal-round-driver suite.

use std::{collections::VecDeque, sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::{StreamExt, stream};
use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentCancelCause, AgentControlError, AgentController, AgentEvent, AgentEvents,
    AgentHandle, AgentOptions, CancelOptions, CreateAgentOptions, Inbox, MaintenanceReservation,
    NoopInboxNotifications, PreStepDecision, RequestErrorAction, SessionStartSource,
};
use seekdeep_agent_loop::{
    AgentErrorEvent, AgentInboxClaimed, AgentInboxMessage, AgentLoop, AgentLoopServices,
    AgentPreStepEvent, DEFAULT_MAX_PARALLEL_TOOL_CALLS, SessionStartEvent,
};
use seekdeep_agent_loop_testkit::{
    AgentLoopTestDependencies, AgentLoopTestDependenciesOptions, mount_agent_loop_test_dependencies,
};
use seekdeep_cordis::{Context, EventOptions, EventReply, PluginFiber};
use seekdeep_core::{
    session::SessionId,
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_goal::{
    Config as GoalConfig, CreateGoalRequest, EditGoalRequest, GoalActivation, GoalEnvironment,
    GoalId, GoalPhase, GoalRef, GoalService, GoalView,
};
use seekdeep_goal_round_driver::{plugin as driver_plugin, render_goal_round_prompt};
use seekdeep_llm::{
    AbortSignal, AdapterStream, ContentBlock, FinishReason, GenerateOptions, LlmAdapter,
    LlmFailure, ModelId, ProviderId, StreamChunk, UserMessage,
};
use seekdeep_scope::ScopeKey;
use serde_json::{Map, Value, json};

#[derive(Debug)]
enum ScriptEntry {
    Text(String),
    MaxTokens(String),
    FinishError(String),
    Error(String),
    Hang,
}

#[derive(Debug)]
struct ScriptedAdapter {
    script: Mutex<VecDeque<ScriptEntry>>,
    requests: Mutex<Vec<GenerateOptions>>,
}

impl ScriptedAdapter {
    fn new(script: impl IntoIterator<Item = ScriptEntry>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(script.into_iter().collect()),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<GenerateOptions> {
        self.requests.lock().clone()
    }
}

#[async_trait]
impl LlmAdapter for ScriptedAdapter {
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        self.requests.lock().push(options.clone());
        match self
            .script
            .lock()
            .pop_front()
            .expect("scripted adapter exhausted")
        {
            ScriptEntry::Text(text) => AdapterStream::new(stream::iter([
                Ok(StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::Text { text },
                }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::Stop,
                    replay_state: None,
                }),
            ])),
            ScriptEntry::MaxTokens(text) => AdapterStream::new(stream::iter([
                Ok(StreamChunk::BlockEnd {
                    index: 0,
                    block: ContentBlock::Text { text },
                }),
                Ok(StreamChunk::Finish {
                    reason: FinishReason::MaxTokens,
                    replay_state: None,
                }),
            ])),
            ScriptEntry::FinishError(message) => {
                AdapterStream::new(stream::iter([Ok(StreamChunk::Finish {
                    reason: FinishReason::Error {
                        failure: LlmFailure {
                            message,
                            code: "SERVER".to_owned(),
                            status: None,
                            provider_retry_after_ms: None,
                            request_id: None,
                        },
                    },
                    replay_state: None,
                })]))
            }
            ScriptEntry::Error(message) => {
                AdapterStream::new(stream::once(async move { Err(anyhow::anyhow!(message)) }))
            }
            ScriptEntry::Hang => {
                let signal = options.signal.unwrap_or_default();
                let partial = stream::iter([Ok(StreamChunk::TextDelta {
                    index: 0,
                    text: "partial".to_owned(),
                })]);
                let cancelled = stream::once(async move {
                    signal.cancelled().await;
                    Err(anyhow::anyhow!("aborted"))
                });
                AdapterStream::new(partial.chain(cancelled))
            }
        }
    }
}

#[derive(Debug)]
struct StableGoalEnvironment;

impl GoalEnvironment for StableGoalEnvironment {
    fn now_millis(&self) -> u64 {
        100
    }

    fn goal_id(&self, session: &seekdeep_core::session::Session, _now: u64) -> GoalId {
        GoalId::new(format!("goal-{}-{}", session.id(), session.seq()))
    }
}

struct Harness {
    context: Context,
    dependencies: AgentLoopTestDependencies,
    goals: Arc<GoalService>,
    adapter: Arc<ScriptedAdapter>,
    agent_loop: AgentLoop,
    factory: seekdeep_agent::AgentFactoryRegistration,
    handle: AgentHandle,
    driver: Option<Arc<PluginFiber>>,
}

impl Harness {
    async fn new(script: impl IntoIterator<Item = ScriptEntry>, install_driver: bool) -> Self {
        let context = Context::new();
        let dependencies = mount_agent_loop_test_dependencies(
            &context,
            AgentLoopTestDependenciesOptions::default(),
        )
        .expect("loop dependencies");
        let goals = GoalService::new_with_environment(
            &context,
            GoalConfig::default(),
            Arc::new(StableGoalEnvironment),
        )
        .expect("goals");
        goals.provide(&context).expect("provide goals");
        let driver = if install_driver {
            Some(mount_driver(&context).await)
        } else {
            None
        };
        let adapter = ScriptedAdapter::new(script);
        dependencies
            .llm
            .register_adapter(&["mock".to_owned()], adapter.clone())
            .expect("adapter");
        let agent_loop = AgentLoop::new(
            context.clone(),
            dependencies.sessions.clone(),
            (*dependencies.agents).clone(),
            AgentLoopServices {
                llm: dependencies.llm.clone(),
                system_prompt: dependencies.system_prompt.clone(),
                tools: dependencies.tools.clone(),
                max_parallel_tool_calls: DEFAULT_MAX_PARALLEL_TOOL_CALLS,
            },
        )
        .expect("agent loop");
        let factory = dependencies
            .agents
            .set_factory(Arc::new(agent_loop.clone()))
            .expect("factory");
        let mut options = CreateAgentOptions::new(SessionId::new("goal-round-driver-test"));
        options.agent_options = AgentOptions {
            provider: Some(ProviderId::new("mock")),
            model: Some(ModelId::new("mock")),
            max_tokens: None,
            subagent_depth: None,
        };
        let handle = dependencies.agents.create(options).await.expect("agent");
        Self {
            context,
            dependencies,
            goals,
            adapter,
            agent_loop,
            factory,
            handle,
            driver,
        }
    }

    fn agent(&self) -> &Arc<Agent> {
        &self.handle.agent
    }

    async fn install_driver(&mut self) {
        assert!(self.driver.is_none());
        self.driver = Some(mount_driver(&self.context).await);
    }

    async fn shutdown(self) {
        if let Some(driver) = self.driver {
            let _ = driver.dispose().await;
        }
        let _ = self.handle.dispose().await;
        let _ = self.agent_loop.dispose().await;
        let _ = self.factory.dispose().await;
        self.dependencies.agents.dispose_initiators().await;
        let _ = self.context.fiber().dispose().await;
    }
}

async fn mount_driver(context: &Context) -> Arc<PluginFiber> {
    let driver = context
        .plugin(driver_plugin(), Value::Null)
        .expect("driver plugin");
    driver.await_settled().await.expect("driver active");
    driver
}

fn create_goal(goals: &GoalService, agent: &Arc<Agent>, objective: &str, cap: u64) -> GoalView {
    goals
        .create(
            agent,
            &CreateGoalRequest {
                objective: objective.to_owned(),
                max_goal_rounds: Some(cap),
            },
        )
        .expect("create goal")
}

fn reference(goal: &GoalView) -> GoalRef {
    GoalRef {
        id: goal.id.clone(),
        revision: goal.revision,
    }
}

fn user(text: &str) -> UserMessage {
    UserMessage::new(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        seekdeep_llm::MessageSource::user(),
    )
}

fn goal_message(text: &str, round: u64) -> UserMessage {
    UserMessage::new(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        seekdeep_llm::MessageSource {
            kind: "goal".to_owned(),
            fields: Map::from_iter([
                ("goalId".to_owned(), json!("forged-goal")),
                ("revision".to_owned(), json!(1)),
                ("round".to_owned(), json!(round)),
            ]),
        },
    )
}

fn request_text(request: &GenerateOptions) -> String {
    request
        .messages
        .iter()
        .flat_map(|message| message.content())
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn wait_until(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if predicate() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("condition timed out");
}

async fn wait_goal(
    goals: &GoalService,
    agent: &Arc<Agent>,
    mut predicate: impl FnMut(&GoalView) -> bool,
) -> GoalView {
    wait_until(|| {
        goals
            .get(agent)
            .ok()
            .flatten()
            .as_ref()
            .is_some_and(&mut predicate)
    })
    .await;
    goals.get(agent).unwrap().unwrap()
}

#[derive(Clone, Copy, Debug)]
enum QueueFailureMode {
    Reject,
    DisarmThenReject,
}

struct RejectingController {
    mode: QueueFailureMode,
    side_effect: Mutex<Option<(Arc<GoalService>, std::sync::Weak<Agent>)>>,
}

impl std::fmt::Debug for RejectingController {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RejectingController")
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl AgentController for RejectingController {
    fn send(
        &self,
        message: UserMessage,
        _target: seekdeep_agent::InboxTarget,
        _wakeup: bool,
    ) -> Result<(), AgentControlError> {
        if message.source().kind == "goal" {
            if matches!(self.mode, QueueFailureMode::DisarmThenReject)
                && let Some((goals, agent)) = self.side_effect.lock().as_ref()
                && let Some(agent) = agent.upgrade()
            {
                goals
                    .disarm(&agent)
                    .map_err(|error| AgentControlError::Inbox(error.to_string()))?;
            }
            return Err(AgentControlError::Inbox(match self.mode {
                QueueFailureMode::Reject => "queue rejected".to_owned(),
                QueueFailureMode::DisarmThenReject => "queue rejected after disarm".to_owned(),
            }));
        }
        Ok(())
    }

    fn cancel(
        &self,
        _cause: AgentCancelCause,
        _options: CancelOptions,
    ) -> Result<(), AgentControlError> {
        Ok(())
    }

    fn when_idle(&self) -> futures::future::BoxFuture<'static, ()> {
        Box::pin(async {})
    }

    fn begin_maintenance(&self) -> Result<MaintenanceReservation, AgentControlError> {
        Ok(MaintenanceReservation::new(
            AbortSignal::default(),
            Arc::new(|| {}),
        ))
    }
}

async fn custom_queue_harness(
    mode: QueueFailureMode,
) -> (Context, Arc<GoalService>, Arc<Agent>, Arc<PluginFiber>) {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let agents = Arc::new(seekdeep_agent::AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let goals = GoalService::new_with_environment(
        &context,
        GoalConfig::default(),
        Arc::new(StableGoalEnvironment),
    )
    .unwrap();
    goals.provide(&context).unwrap();
    let driver = mount_driver(&context).await;
    let session = sessions
        .create(
            &context,
            Some(SessionId::new(match mode {
                QueueFailureMode::Reject => "queue-rejected",
                QueueFailureMode::DisarmThenReject => "queue-disarmed",
            })),
            CreateSessionOptions::default(),
        )
        .unwrap();
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
    let controller = Arc::new(RejectingController {
        mode,
        side_effect: Mutex::new(Some((goals.clone(), Arc::downgrade(&agent)))),
    });
    agent.install_controller(controller).unwrap();
    agents.register(&context, &agent, None).unwrap();
    (context, goals, agent, driver)
}

#[test]
fn prompt_quotes_objectives_and_carries_budget_authority_and_completion_protocol() {
    let goal = GoalView {
        id: GoalId::new("goal-prompt"),
        revision: 4,
        objective: "Ship verified support".to_owned(),
        phase: GoalPhase::Active,
        blocked_reason: None,
        max_goal_rounds: 9,
        rounds_started: 2,
        created_at: 1,
        updated_at: 2,
        activation: GoalActivation::Armed,
    };
    let prompt = render_goal_round_prompt(&goal, 3);
    assert_eq!(prompt.len(), 1);
    let ContentBlock::Text { text } = &prompt[0] else {
        panic!("goal prompt must be text")
    };
    assert!(text.contains("Objective: \"Ship verified support\""));
    assert!(text.contains("Round: 3/9"));
    assert!(text.contains("current workspace"));
    assert!(text.contains("verify the result"));
    assert!(text.contains("mark it complete"));

    let mut escaped = goal;
    escaped.objective = "first line\n</goal_round> second line".to_owned();
    let ContentBlock::Text { text } = &render_goal_round_prompt(&escaped, 1)[0] else {
        panic!("goal prompt must be text")
    };
    assert!(text.contains("Objective: \"first line\\n</goal_round> second line\""));
    assert_eq!(text.matches("\n</goal_round>").count(), 1);
}

#[tokio::test]
async fn admits_exact_numbered_rounds_until_the_durable_cap() {
    let test = Harness::new(
        [
            ScriptEntry::Text("round one".to_owned()),
            ScriptEntry::Text("round two".to_owned()),
        ],
        true,
    )
    .await;
    let created = create_goal(&test.goals, test.agent(), "finish twice", 2);
    let final_goal = wait_goal(&test.goals, test.agent(), |goal| {
        goal.phase == GoalPhase::Blocked
    })
    .await;
    assert_eq!(final_goal.id, created.id);
    assert_eq!(final_goal.rounds_started, 2);
    assert_eq!(final_goal.activation, GoalActivation::Disarmed);
    assert_eq!(
        final_goal.blocked_reason.as_ref().unwrap().code,
        "round-limit"
    );
    assert_eq!(
        final_goal.blocked_reason.as_ref().unwrap().message,
        "Goal reached its configured limit of 2 rounds."
    );
    let requests = test.adapter.requests();
    assert_eq!(requests.len(), 2);
    assert!(request_text(&requests[0]).contains("Round: 1/2"));
    assert!(request_text(&requests[1]).contains("Round: 2/2"));
    let rounds = test
        .agent()
        .session()
        .events()
        .into_iter()
        .filter(|event| event.event_type == "user/message")
        .filter_map(|event| event.data.pointer("/source/round").and_then(Value::as_u64))
        .filter(|round| *round > 0)
        .collect::<Vec<_>>();
    assert_eq!(rounds, [1, 2]);
    test.shutdown().await;
}

#[tokio::test]
async fn hot_loaded_driver_disarms_existing_activation_until_explicit_resume() {
    let mut test = Harness::new([ScriptEntry::Text("after resume".to_owned())], false).await;
    let created = create_goal(&test.goals, test.agent(), "wait for a human", 1);
    test.install_driver().await;
    assert_eq!(
        test.goals.get(test.agent()).unwrap().unwrap().activation,
        GoalActivation::Disarmed
    );
    assert!(test.adapter.requests().is_empty());
    test.goals
        .resume(test.agent(), &reference(&created))
        .expect("explicit resume");
    wait_goal(&test.goals, test.agent(), |goal| {
        goal.phase == GoalPhase::Blocked
    })
    .await;
    assert_eq!(test.adapter.requests().len(), 1);
    test.shutdown().await;
}

#[tokio::test]
async fn model_failure_and_max_tokens_disarm_after_the_admitted_round() {
    for entry in [
        ScriptEntry::Error("provider broke".to_owned()),
        ScriptEntry::MaxTokens("unfinished".to_owned()),
    ] {
        let test = Harness::new([entry], true).await;
        create_goal(&test.goals, test.agent(), "stop safely", 8);
        let goal = wait_goal(&test.goals, test.agent(), |goal| {
            goal.phase == GoalPhase::Active && goal.activation == GoalActivation::Disarmed
        })
        .await;
        assert_eq!(goal.rounds_started, 1);
        assert_eq!(test.adapter.requests().len(), 1);
        test.shutdown().await;
    }
}

#[tokio::test]
async fn downstream_step_rejection_blocks_without_admitting_the_round() {
    let test = Harness::new([], true).await;
    test.context
        .events()
        .on_waterfall(
            &test.context,
            "agent/pre-step",
            |_, args, next| {
                Box::pin(async move {
                    let event = args.get::<AgentEvent<AgentPreStepEvent>>(0).unwrap();
                    if event
                        .payload
                        .messages
                        .first()
                        .is_some_and(|message| message.source().kind == "goal")
                    {
                        Ok(EventReply::Value(Arc::new(PreStepDecision::Reject)))
                    } else {
                        next.run().await
                    }
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "respect policy", 8);
    let goal = wait_goal(&test.goals, test.agent(), |goal| {
        goal.phase == GoalPhase::Blocked
    })
    .await;
    assert_eq!(goal.rounds_started, 0);
    assert_eq!(
        goal.blocked_reason.as_ref().unwrap().code,
        "prompt-rejected"
    );
    assert!(test.adapter.requests().is_empty());
    assert!(
        test.agent()
            .session()
            .events()
            .iter()
            .any(|event| event.event_type == "turn/start")
    );
    test.shutdown().await;
}

#[tokio::test]
async fn queued_human_work_runs_before_the_automatic_round() {
    let test = Harness::new(
        [
            ScriptEntry::Text("human answer".to_owned()),
            ScriptEntry::Text("goal answer".to_owned()),
        ],
        true,
    )
    .await;
    create_goal(&test.goals, test.agent(), "continue after the human", 1);
    test.agent()
        .followup(user("human goes first"))
        .expect("human followup");
    wait_goal(&test.goals, test.agent(), |goal| {
        goal.phase == GoalPhase::Blocked
    })
    .await;
    let requests = test.adapter.requests();
    assert_eq!(requests.len(), 2);
    assert!(request_text(&requests[0]).contains("human goes first"));
    assert!(!request_text(&requests[0]).contains("<goal_round>"));
    assert!(request_text(&requests[1]).contains("<goal_round>"));
    test.shutdown().await;
}

#[tokio::test]
async fn nested_human_insertion_makes_a_queued_reservation_stale() {
    let test = Harness::new(
        [
            ScriptEntry::Text("human batch".to_owned()),
            ScriptEntry::Text("later goal".to_owned()),
        ],
        true,
    )
    .await;
    let inserted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = inserted.clone();
    let agent = test.agent().clone();
    test.context
        .events()
        .on_sync(
            &test.context,
            "agent/inbox/inserted",
            move |_, args| {
                let event = args
                    .get::<AgentEvent<AgentInboxMessage>>(0)
                    .expect("inbox event");
                if Arc::ptr_eq(&event.agent, &agent)
                    && event.payload.message.source().kind == "goal"
                    && !once.swap(true, std::sync::atomic::Ordering::AcqRel)
                {
                    agent.followup(user("human joined the pending batch"))?;
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "yield to nested human input", 1);
    wait_goal(&test.goals, test.agent(), |goal| {
        goal.phase == GoalPhase::Blocked
    })
    .await;
    let requests = test.adapter.requests();
    assert_eq!(requests.len(), 2);
    assert!(request_text(&requests[0]).contains("human joined the pending batch"));
    assert!(!request_text(&requests[0]).contains("<goal_round>"));
    assert!(request_text(&requests[1]).contains("<goal_round>"));
    test.shutdown().await;
}

#[tokio::test]
async fn downstream_goal_edit_is_rechecked_before_admission() {
    let test = Harness::new([ScriptEntry::Text("new revision".to_owned())], true).await;
    let edited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = edited.clone();
    let goals = test.goals.clone();
    test.context
        .events()
        .on_waterfall(
            &test.context,
            "agent/pre-step",
            move |_, args, next| {
                let goals = goals.clone();
                let once = once.clone();
                Box::pin(async move {
                    let event = args.get::<AgentEvent<AgentPreStepEvent>>(0).unwrap();
                    if event
                        .payload
                        .messages
                        .first()
                        .is_some_and(|message| message.source().kind == "goal")
                        && !once.swap(true, std::sync::atomic::Ordering::AcqRel)
                    {
                        let current = goals.get(&event.agent)?.expect("goal");
                        goals.edit(
                            &event.agent,
                            &reference(&current),
                            &EditGoalRequest {
                                objective: Some("edited downstream".to_owned()),
                                max_goal_rounds: None,
                            },
                        )?;
                    }
                    next.run().await
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "edit during pre-step", 1);
    let goal = wait_goal(&test.goals, test.agent(), |goal| {
        goal.phase == GoalPhase::Blocked
    })
    .await;
    assert_eq!(goal.objective, "edited downstream");
    assert_eq!(goal.rounds_started, 1);
    assert_eq!(test.adapter.requests().len(), 1);
    test.shutdown().await;
}

#[tokio::test]
async fn durability_checkpoint_failure_disarms_without_dispatch() {
    let test = Harness::new([], true).await;
    test.context
        .events()
        .on_sync(
            &test.context,
            "session/flush",
            |_, _| anyhow::bail!("disk unavailable"),
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "do not outrun storage", 8);
    let goal = wait_goal(&test.goals, test.agent(), |goal| {
        goal.activation == GoalActivation::Disarmed
    })
    .await;
    assert_eq!(goal.phase, GoalPhase::Active);
    assert_eq!(goal.rounds_started, 0);
    assert!(test.adapter.requests().is_empty());
    test.shutdown().await;
}

#[tokio::test]
async fn next_round_waits_for_the_successful_settled_round_checkpoint() {
    let test = Harness::new(
        [
            ScriptEntry::Text("round one".to_owned()),
            ScriptEntry::Text("round two".to_owned()),
        ],
        true,
    )
    .await;
    let observations = Arc::new(Mutex::new(Vec::new()));
    let seen = observations.clone();
    let adapter = test.adapter.clone();
    test.context
        .events()
        .on_sync(
            &test.context,
            "session/flush",
            move |_, _| {
                seen.lock().push(adapter.requests().len());
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "checkpoint between rounds", 2);
    wait_goal(&test.goals, test.agent(), |goal| {
        goal.phase == GoalPhase::Blocked
    })
    .await;
    assert!(observations.lock().contains(&1));
    assert_eq!(test.adapter.requests().len(), 2);
    test.shutdown().await;
}

#[tokio::test]
async fn cancellation_before_and_during_a_round_pauses_fail_closed() {
    let before = Harness::new([], true).await;
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = cancelled.clone();
    let agent = before.agent().clone();
    before
        .context
        .events()
        .on_sync(
            &before.context,
            "agent/inbox/claimed",
            move |_, args| {
                let event = args
                    .get::<AgentEvent<AgentInboxClaimed>>(0)
                    .expect("claimed event");
                if Arc::ptr_eq(&event.agent, &agent)
                    && event.payload.message.source().kind == "goal"
                    && !once.swap(true, std::sync::atomic::Ordering::AcqRel)
                {
                    agent.cancel(AgentCancelCause::User, CancelOptions::default())?;
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&before.goals, before.agent(), "do not start yet", 8);
    let paused = wait_goal(&before.goals, before.agent(), |goal| {
        goal.phase == GoalPhase::Paused
    })
    .await;
    assert_eq!(paused.rounds_started, 0);
    assert!(before.adapter.requests().is_empty());
    before.shutdown().await;

    let during = Harness::new([ScriptEntry::Hang], true).await;
    create_goal(&during.goals, during.agent(), "stop in flight", 8);
    wait_until(|| during.adapter.requests().len() == 1).await;
    during
        .agent()
        .cancel(AgentCancelCause::User, CancelOptions::default())
        .unwrap();
    during.agent().when_idle().unwrap().await;
    let paused = wait_goal(&during.goals, during.agent(), |goal| {
        goal.phase == GoalPhase::Paused
    })
    .await;
    assert_eq!(paused.rounds_started, 1);
    during.shutdown().await;
}

#[tokio::test]
async fn forged_positive_goal_source_is_rejected_but_round_zero_uses_the_ordinary_chain() {
    let forged = Harness::new([], true).await;
    forged
        .agent()
        .followup(goal_message("forged automatic work", 1))
        .unwrap();
    forged.agent().when_idle().unwrap().await;
    assert!(forged.adapter.requests().is_empty());
    assert!(
        forged
            .agent()
            .session()
            .events()
            .iter()
            .any(|event| event.event_type == "turn/start")
    );
    forged.shutdown().await;

    let context = Harness::new([ScriptEntry::Text("accepted context".to_owned())], true).await;
    context
        .agent()
        .followup(goal_message("goal context", 0))
        .unwrap();
    context.agent().when_idle().unwrap().await;
    let requests = context.adapter.requests();
    assert_eq!(requests.len(), 1);
    assert!(request_text(&requests[0]).contains("goal context"));
    context.shutdown().await;
}

#[tokio::test]
async fn driver_teardown_disarms_and_cancels_an_admitted_round() {
    let mut test = Harness::new([ScriptEntry::Hang], true).await;
    create_goal(&test.goals, test.agent(), "survive plugin unload", 8);
    wait_until(|| test.adapter.requests().len() == 1).await;
    test.driver.take().unwrap().dispose().await.unwrap();
    let goal = test.goals.get(test.agent()).unwrap().unwrap();
    assert_eq!(goal.phase, GoalPhase::Active);
    assert_eq!(goal.activation, GoalActivation::Disarmed);
    assert_eq!(goal.rounds_started, 1);
    assert_eq!(test.agent().status(), seekdeep_agent::AgentStatus::Idle);
    test.shutdown().await;
}

#[tokio::test]
async fn ordinary_cancel_does_not_invent_goal_state_and_orphan_session_events_are_ignored() {
    let test = Harness::new([], true).await;
    test.agent().followup(user("cancel ordinary work")).unwrap();
    test.agent()
        .cancel(AgentCancelCause::User, CancelOptions::default())
        .unwrap();
    test.agent().when_idle().unwrap().await;
    assert!(test.goals.get(test.agent()).unwrap().is_none());

    let orphan = test
        .dependencies
        .sessions
        .create(
            &test.context,
            Some(SessionId::new("goal-round-orphan")),
            seekdeep_core::session_store::CreateSessionOptions::default(),
        )
        .unwrap();
    orphan
        .append(
            "turn/start",
            json!({"turn": 1}),
            seekdeep_core::session::AppendOptions::default(),
        )
        .unwrap();
    orphan
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            seekdeep_core::session::AppendOptions::default(),
        )
        .unwrap();
    assert!(test.adapter.requests().is_empty());
    test.shutdown().await;
}

#[tokio::test]
async fn downstream_pause_before_rejection_is_not_overwritten_with_blocked() {
    let test = Harness::new([], true).await;
    let goals = test.goals.clone();
    test.context
        .events()
        .on_waterfall(
            &test.context,
            "agent/pre-step",
            move |_, args, next| {
                let goals = goals.clone();
                Box::pin(async move {
                    let event = args.get::<AgentEvent<AgentPreStepEvent>>(0).unwrap();
                    if event.payload.messages.iter().any(|message| {
                        message.source().kind == "goal"
                            && message
                                .source()
                                .fields
                                .get("round")
                                .and_then(Value::as_u64)
                                .is_some_and(|round| round > 0)
                    }) {
                        let goal = goals.get(&event.agent)?.expect("current goal");
                        goals.pause(&event.agent, &reference(&goal))?;
                        Ok(EventReply::Value(Arc::new(PreStepDecision::Reject)))
                    } else {
                        next.run().await
                    }
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "pause before rejection", 8);
    let goal = wait_goal(&test.goals, test.agent(), |goal| {
        goal.phase == GoalPhase::Paused
    })
    .await;
    assert!(goal.blocked_reason.is_none());
    assert!(test.adapter.requests().is_empty());
    test.shutdown().await;
}

#[tokio::test]
async fn failed_round_checkpoint_disarms_before_reserving_another_round() {
    let test = Harness::new([ScriptEntry::Text("round one ran".to_owned())], true).await;
    let flushes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = flushes.clone();
    test.context
        .events()
        .on_sync(
            &test.context,
            "session/flush",
            move |_, _| {
                if count.fetch_add(1, std::sync::atomic::Ordering::AcqRel) >= 1 {
                    anyhow::bail!("round checkpoint failed");
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(
        &test.goals,
        test.agent(),
        "no autonomous rounds without durability",
        5,
    );
    let goal = wait_goal(&test.goals, test.agent(), |goal| {
        goal.activation == GoalActivation::Disarmed
    })
    .await;
    assert_eq!(goal.phase, GoalPhase::Active);
    assert_eq!(goal.rounds_started, 1);
    assert_eq!(test.adapter.requests().len(), 1);
    test.shutdown().await;
}

#[tokio::test]
async fn successful_retry_turn_settles_the_goal_round() {
    let test = Harness::new(
        [
            ScriptEntry::FinishError("transient".to_owned()),
            ScriptEntry::Text("retry succeeded".to_owned()),
        ],
        true,
    )
    .await;
    let retried = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = retried.clone();
    test.context
        .events()
        .on_waterfall(
            &test.context,
            "agent/request-error",
            move |_, _, next| {
                let once = once.clone();
                Box::pin(async move {
                    if !once.swap(true, std::sync::atomic::Ordering::AcqRel) {
                        Ok(EventReply::Value(Arc::new(RequestErrorAction::Retry)))
                    } else {
                        next.run().await
                    }
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "survive a transient failure", 1);
    let goal = wait_goal(&test.goals, test.agent(), |goal| {
        goal.phase == GoalPhase::Blocked
    })
    .await;
    assert_eq!(goal.rounds_started, 1);
    assert_eq!(goal.blocked_reason.as_ref().unwrap().code, "round-limit");
    assert_eq!(test.adapter.requests().len(), 2);
    test.shutdown().await;
}

#[tokio::test]
async fn closed_scheduler_fails_goal_activation_closed() {
    let test = Harness::new([], true).await;
    test.dependencies.agents.close_initiators();
    create_goal(&test.goals, test.agent(), "fail scheduler closed", 8);
    let goal = wait_goal(&test.goals, test.agent(), |goal| {
        goal.activation == GoalActivation::Disarmed
    })
    .await;
    assert_eq!(goal.phase, GoalPhase::Active);
    assert!(test.adapter.requests().is_empty());
    test.shutdown().await;
}

#[tokio::test]
async fn session_start_resets_reservation_and_requires_explicit_resume() {
    let test = Harness::new(
        [ScriptEntry::Text("after explicit resume".to_owned())],
        true,
    )
    .await;
    let created = create_goal(&test.goals, test.agent(), "restart safely", 1);
    AgentEvents::new(test.context.clone(), test.agent().clone()).emit(
        "agent/session-start",
        SessionStartEvent {
            source: SessionStartSource::Resume,
        },
    );
    tokio::task::yield_now().await;
    let stopped = test.goals.get(test.agent()).unwrap().unwrap();
    assert_eq!(stopped.activation, GoalActivation::Disarmed);
    assert_eq!(stopped.rounds_started, 0);
    assert!(test.adapter.requests().is_empty());
    test.goals
        .resume(test.agent(), &reference(&created))
        .unwrap();
    wait_goal(&test.goals, test.agent(), |goal| {
        goal.phase == GoalPhase::Blocked
    })
    .await;
    assert_eq!(test.adapter.requests().len(), 1);
    test.shutdown().await;
}

#[tokio::test]
async fn rejected_turn_end_and_post_turn_error_disarm_without_continuing() {
    let rejected = Harness::new([ScriptEntry::Text("round ran".to_owned())], true).await;
    rejected
        .context
        .events()
        .on_sync(
            &rejected.context,
            "internal/dispatch",
            |_, args| {
                let name = args.get::<String>(1).expect("event name");
                if name.as_str() == "session/event" {
                    let event_args = args
                        .get::<seekdeep_cordis::EventArgs>(2)
                        .expect("event args");
                    let event = event_args
                        .get::<seekdeep_core::session::SessionEvent>(1)
                        .expect("session event");
                    if event.event_type == "turn/end" {
                        anyhow::bail!("turn close permanently rejected");
                    }
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(
        &rejected.goals,
        rejected.agent(),
        "survive a lost turn end",
        8,
    );
    wait_until(|| rejected.adapter.requests().len() == 1).await;
    rejected.agent().when_idle().unwrap().await;
    let goal = wait_goal(&rejected.goals, rejected.agent(), |goal| {
        goal.activation == GoalActivation::Disarmed
    })
    .await;
    assert_eq!(goal.phase, GoalPhase::Active);
    rejected.shutdown().await;

    let reported = Harness::new([ScriptEntry::Text("round one".to_owned())], true).await;
    let agent = reported.agent().clone();
    let context = reported.context.clone();
    reported
        .context
        .events()
        .on_sync(
            &reported.context,
            "session/event",
            move |_, args| {
                let session = args
                    .get::<seekdeep_core::session::Session>(0)
                    .expect("session");
                let event = args
                    .get::<seekdeep_core::session::SessionEvent>(1)
                    .expect("event");
                if Arc::ptr_eq(&session, agent.session()) && event.event_type == "turn/end" {
                    AgentEvents::new(context.clone(), agent.clone()).emit(
                        "agent/error",
                        AgentErrorEvent {
                            turn: event.data["turn"].as_u64().unwrap(),
                            step: 1,
                            error: "post-turn flush failed".to_owned(),
                        },
                    );
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(
        &reported.goals,
        reported.agent(),
        "stop when durability is lost",
        8,
    );
    let goal = wait_goal(&reported.goals, reported.agent(), |goal| {
        goal.activation == GoalActivation::Disarmed
    })
    .await;
    assert_eq!(goal.rounds_started, 1);
    assert_eq!(reported.adapter.requests().len(), 1);
    reported.shutdown().await;
}

#[tokio::test]
async fn pause_observer_work_runs_after_cancelled_round_before_any_new_drive() {
    let test = Harness::new(
        [
            ScriptEntry::Hang,
            ScriptEntry::Text("inspection answer".to_owned()),
        ],
        true,
    )
    .await;
    let agent = test.agent().clone();
    test.context
        .events()
        .on_sync(
            &test.context,
            "goal/changed",
            move |_, args| {
                let event = args
                    .get::<AgentEvent<seekdeep_goal::GoalChangedEvent>>(0)
                    .expect("goal change");
                if Arc::ptr_eq(&event.agent, &agent)
                    && event.payload.change.operation == seekdeep_goal::GoalOperation::Pause
                {
                    agent.followup(user("inspect the pause"))?;
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "pause then inspect", 8);
    wait_until(|| test.adapter.requests().len() == 1).await;
    test.agent()
        .cancel(AgentCancelCause::User, CancelOptions::default())
        .unwrap();
    wait_until(|| test.adapter.requests().len() == 2).await;
    test.agent().when_idle().unwrap().await;
    let goal = test.goals.get(test.agent()).unwrap().unwrap();
    assert_eq!(goal.phase, GoalPhase::Paused);
    assert_eq!(goal.rounds_started, 1);
    assert!(request_text(&test.adapter.requests()[1]).contains("inspect the pause"));
    test.shutdown().await;
}

#[tokio::test]
async fn teardown_waits_for_claimed_pre_step_work_to_release() {
    let mut test = Harness::new([], true).await;
    let release = Arc::new(tokio::sync::Notify::new());
    let did_enter = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let release_listener = release.clone();
    let did_enter_listener = did_enter.clone();
    test.context
        .events()
        .on_waterfall(
            &test.context,
            "agent/pre-step",
            move |_, args, next| {
                let release = release_listener.clone();
                let did_enter = did_enter_listener.clone();
                Box::pin(async move {
                    let event = args.get::<AgentEvent<AgentPreStepEvent>>(0).unwrap();
                    if event
                        .payload
                        .messages
                        .first()
                        .is_some_and(|message| message.source().kind == "goal")
                        && !did_enter.swap(true, std::sync::atomic::Ordering::AcqRel)
                    {
                        release.notified().await;
                    }
                    next.run().await
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "unload during pre-step", 8);
    wait_until(|| did_enter.load(std::sync::atomic::Ordering::Acquire)).await;
    let driver = test.driver.take().unwrap();
    let disposal = tokio::spawn(async move { driver.dispose().await });
    wait_goal(&test.goals, test.agent(), |goal| {
        goal.activation == GoalActivation::Disarmed
    })
    .await;
    assert!(!disposal.is_finished());
    release.notify_waiters();
    disposal.await.unwrap().unwrap();
    assert!(test.adapter.requests().is_empty());
    assert_eq!(
        test.goals
            .get(test.agent())
            .unwrap()
            .unwrap()
            .rounds_started,
        0
    );
    test.shutdown().await;
}

#[tokio::test]
async fn rejected_custom_followup_blocks_with_queue_failure() {
    let (context, goals, agent, driver) = custom_queue_harness(QueueFailureMode::Reject).await;
    create_goal(&goals, &agent, "handle queue failure", 8);
    let goal = wait_goal(&goals, &agent, |goal| goal.phase == GoalPhase::Blocked).await;
    assert_eq!(goal.rounds_started, 0);
    assert_eq!(goal.activation, GoalActivation::Disarmed);
    assert_eq!(goal.blocked_reason.as_ref().unwrap().code, "queue-failed");
    assert_eq!(
        goal.blocked_reason.as_ref().unwrap().message,
        "Could not queue goal round 1: queue rejected"
    );
    driver.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn custom_followup_disarm_side_effect_is_preserved_before_failure() {
    let (context, goals, agent, driver) =
        custom_queue_harness(QueueFailureMode::DisarmThenReject).await;
    create_goal(&goals, &agent, "preserve newer activation", 8);
    let goal = wait_goal(&goals, &agent, |goal| {
        goal.activation == GoalActivation::Disarmed
    })
    .await;
    assert_eq!(goal.phase, GoalPhase::Active);
    assert_eq!(goal.rounds_started, 0);
    assert!(goal.blocked_reason.is_none());
    driver.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn blocked_goal_observer_work_remains_queued_without_reserving_again() {
    let test = Harness::new([ScriptEntry::Text("human follow-up".to_owned())], true).await;
    test.context
        .events()
        .on_waterfall(
            &test.context,
            "agent/pre-step",
            |_, args, next| {
                Box::pin(async move {
                    let event = args.get::<AgentEvent<AgentPreStepEvent>>(0).unwrap();
                    if event
                        .payload
                        .messages
                        .first()
                        .is_some_and(|message| message.source().kind == "goal")
                    {
                        Ok(EventReply::Value(Arc::new(PreStepDecision::Reject)))
                    } else {
                        next.run().await
                    }
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    let agent = test.agent().clone();
    test.context
        .events()
        .on_sync(
            &test.context,
            "goal/changed",
            move |_, args| {
                let event = args
                    .get::<AgentEvent<seekdeep_goal::GoalChangedEvent>>(0)
                    .expect("goal changed");
                if Arc::ptr_eq(&event.agent, &agent)
                    && event.payload.change.operation == seekdeep_goal::GoalOperation::Block
                {
                    agent.followup(user("inspect the blocker"))?;
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "stop and inspect", 8);
    wait_goal(&test.goals, test.agent(), |goal| {
        goal.phase == GoalPhase::Blocked
    })
    .await;
    test.agent().when_idle().unwrap().await;
    assert!(test.adapter.requests().is_empty());
    assert_eq!(test.agent().inbox().next_turn().len(), 1);
    assert_eq!(
        test.agent().inbox().next_turn()[0].content()[0],
        ContentBlock::Text {
            text: "inspect the blocker".to_owned()
        }
    );
    test.shutdown().await;
}

#[tokio::test]
async fn queued_goal_edit_stales_old_revision_and_continues_the_new_one() {
    let test = Harness::new([ScriptEntry::Text("new revision".to_owned())], true).await;
    let edited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = edited.clone();
    let goals = test.goals.clone();
    test.context
        .events()
        .on_sync(
            &test.context,
            "agent/inbox/inserted",
            move |_, args| {
                let event = args
                    .get::<AgentEvent<AgentInboxMessage>>(0)
                    .expect("inserted");
                if event.payload.message.source().kind == "goal"
                    && !once.swap(true, std::sync::atomic::Ordering::AcqRel)
                {
                    let goal = goals.get(&event.agent)?.expect("goal");
                    goals.edit(
                        &event.agent,
                        &reference(&goal),
                        &EditGoalRequest {
                            objective: Some("new objective".to_owned()),
                            max_goal_rounds: None,
                        },
                    )?;
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "old objective", 1);
    let goal = wait_goal(&test.goals, test.agent(), |goal| {
        goal.phase == GoalPhase::Blocked
    })
    .await;
    assert_eq!(goal.objective, "new objective");
    assert_eq!(goal.revision, 3);
    assert_eq!(goal.rounds_started, 1);
    let admitted_revision = test
        .agent()
        .session()
        .events()
        .into_iter()
        .find(|event| {
            event.event_type == "user/message"
                && event.data.pointer("/source/kind").and_then(Value::as_str) == Some("goal")
                && event
                    .data
                    .pointer("/source/round")
                    .and_then(Value::as_u64)
                    .is_some_and(|round| round > 0)
        })
        .and_then(|event| {
            event
                .data
                .pointer("/source/revision")
                .and_then(Value::as_u64)
        });
    assert_eq!(admitted_revision, Some(2));
    test.shutdown().await;
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn stale_claim_restores_non_goal_context_without_reviving_round_zero() {
    let test = Harness::new(
        [
            ScriptEntry::Text("side contexts".to_owned()),
            ScriptEntry::Text("revised goal".to_owned()),
        ],
        true,
    )
    .await;
    let claimed = UserMessage::new(
        vec![ContentBlock::Text {
            text: "claimed context to restore".to_owned(),
        }],
        seekdeep_llm::MessageSource::plugin("test"),
    );
    let round_zero = UserMessage::new(
        vec![ContentBlock::Text {
            text: "obsolete goal context".to_owned(),
        }],
        seekdeep_llm::MessageSource {
            kind: "goal".to_owned(),
            fields: Map::from_iter([
                ("goalId".to_owned(), json!("old-goal")),
                ("revision".to_owned(), json!(1)),
                ("round".to_owned(), json!(0)),
            ]),
        },
    );
    let queued_step = UserMessage::new(
        vec![ContentBlock::Text {
            text: "context already queued for the next step".to_owned(),
        }],
        seekdeep_llm::MessageSource::plugin("test"),
    );
    let queued_turn = UserMessage::new(
        vec![ContentBlock::Text {
            text: "context already queued for the next turn".to_owned(),
        }],
        seekdeep_llm::MessageSource::plugin("test"),
    );
    let staged = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let staged_once = staged.clone();
    let claimed_for_listener = claimed.clone();
    let round_zero_for_listener = round_zero.clone();
    test.context
        .events()
        .on_sync(
            &test.context,
            "agent/inbox/inserted",
            move |_, args| {
                let event = args
                    .get::<AgentEvent<AgentInboxMessage>>(0)
                    .expect("inserted");
                if event.payload.message.source().kind == "goal"
                    && !staged_once.swap(true, std::sync::atomic::Ordering::AcqRel)
                {
                    event.agent.inbox().prepend(
                        seekdeep_agent::InboxTarget::NextStep,
                        claimed_for_listener.clone(),
                    )?;
                    event.agent.inbox().prepend(
                        seekdeep_agent::InboxTarget::NextStep,
                        round_zero_for_listener.clone(),
                    )?;
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let edited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let edited_once = edited.clone();
    let goals = test.goals.clone();
    let queued_step_for_listener = queued_step.clone();
    let queued_turn_for_listener = queued_turn.clone();
    test.context
        .events()
        .on_waterfall(
            &test.context,
            "agent/pre-step",
            move |_, args, next| {
                let goals = goals.clone();
                let edited = edited_once.clone();
                let queued_step = queued_step_for_listener.clone();
                let queued_turn = queued_turn_for_listener.clone();
                Box::pin(async move {
                    let event = args.get::<AgentEvent<AgentPreStepEvent>>(0).unwrap();
                    let decision = next.run().await?;
                    if event.payload.messages.iter().any(|message| {
                        message.source().kind == "goal"
                            && message
                                .source()
                                .fields
                                .get("round")
                                .and_then(Value::as_u64)
                                .is_some_and(|round| round > 0)
                    }) && !edited.swap(true, std::sync::atomic::Ordering::AcqRel)
                    {
                        event
                            .agent
                            .inbox()
                            .prepend(seekdeep_agent::InboxTarget::NextStep, queued_step.clone())?;
                        event
                            .agent
                            .inbox()
                            .append(seekdeep_agent::InboxTarget::NextTurn, queued_turn.clone())?;
                        let goal = goals.get(&event.agent)?.expect("goal");
                        goals.edit(
                            &event.agent,
                            &reference(&goal),
                            &EditGoalRequest {
                                objective: Some("revised after claim".to_owned()),
                                max_goal_rounds: None,
                            },
                        )?;
                        let entered = decision
                            .downcast::<PreStepDecision>()
                            .ok_or_else(|| anyhow::anyhow!("invalid pre-step decision"))?;
                        let PreStepDecision::Enter { mut messages } = (*entered).clone() else {
                            return Ok(EventReply::Value(entered));
                        };
                        messages.push(queued_step);
                        messages.push(queued_turn);
                        return Ok(EventReply::Value(Arc::new(PreStepDecision::Enter {
                            messages,
                        })));
                    }
                    Ok(decision)
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "stale before admission", 1);
    let goal = wait_goal(&test.goals, test.agent(), |goal| {
        goal.phase == GoalPhase::Blocked
    })
    .await;
    assert_eq!(goal.objective, "revised after claim");
    assert_eq!(goal.rounds_started, 1);
    let requests = test.adapter.requests();
    assert_eq!(requests.len(), 2);
    let first = request_text(&requests[0]);
    assert!(first.contains("claimed context to restore"));
    assert!(first.contains("context already queued for the next step"));
    assert!(first.contains("context already queued for the next turn"));
    assert!(!first.contains("obsolete goal context"));
    assert!(!first.contains("<goal_round>"));
    let second = request_text(&requests[1]);
    assert!(second.contains("revised after claim"));
    assert!(!second.contains("stale before admission"));
    test.shutdown().await;
}

#[tokio::test]
async fn clear_checkpoint_failure_is_contained_without_a_current_goal() {
    let test = Harness::new([], true).await;
    let flushes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = flushes.clone();
    test.context
        .events()
        .on_sync(
            &test.context,
            "session/flush",
            move |_, _| {
                seen.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                anyhow::bail!("clear checkpoint failed")
            },
            EventOptions::default(),
        )
        .unwrap();
    AgentEvents::new(test.context.clone(), test.agent().clone()).emit(
        "goal/changed",
        seekdeep_goal::GoalChangedEvent {
            change: seekdeep_goal::GoalChanged {
                operation: seekdeep_goal::GoalOperation::Clear,
                goal_ref: GoalRef {
                    id: GoalId::new("cleared-goal"),
                    revision: 2,
                },
                goal: None,
            },
        },
    );
    wait_until(|| flushes.load(std::sync::atomic::Ordering::Acquire) == 1).await;
    assert!(test.goals.get(test.agent()).unwrap().is_none());
    assert!(test.adapter.requests().is_empty());
    test.shutdown().await;
}

#[tokio::test]
async fn throwing_downstream_hook_fails_closed_and_does_not_admit_the_prompt() {
    let test = Harness::new([], true).await;
    let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = fired.clone();
    test.context
        .events()
        .on_waterfall(
            &test.context,
            "agent/pre-step",
            move |_, args, next| {
                let once = once.clone();
                Box::pin(async move {
                    let event = args.get::<AgentEvent<AgentPreStepEvent>>(0).unwrap();
                    if event
                        .payload
                        .messages
                        .first()
                        .is_some_and(|message| message.source().kind == "goal")
                        && !once.swap(true, std::sync::atomic::Ordering::AcqRel)
                    {
                        anyhow::bail!("downstream pre-step hook exploded");
                    }
                    next.run().await
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "survive a throwing hook", 1);
    let goal = wait_goal(&test.goals, test.agent(), |goal| {
        goal.activation == GoalActivation::Disarmed
    })
    .await;
    assert_eq!(goal.phase, GoalPhase::Active);
    assert_eq!(goal.rounds_started, 0);
    assert!(test.adapter.requests().is_empty());
    assert!(test.agent().inbox().next_turn().is_empty());
    test.shutdown().await;
}

#[tokio::test]
async fn round_limit_mutation_failure_propagates_to_scheduler_fail_closed() {
    let test = Harness::new([ScriptEntry::Text("the only round".to_owned())], true).await;
    test.context
        .events()
        .on_sync(
            &test.context,
            "internal/dispatch",
            |_, args| {
                if args
                    .get::<String>(1)
                    .is_some_and(|name| name.as_str() == "session/event")
                {
                    let event_args = args
                        .get::<seekdeep_cordis::EventArgs>(2)
                        .expect("event args");
                    let event = event_args
                        .get::<seekdeep_core::session::SessionEvent>(1)
                        .expect("event");
                    if event.event_type == "goal/change" && event.data["operation"] == "block" {
                        anyhow::bail!("round-limit block failed");
                    }
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "contain a driver failure", 1);
    let goal = wait_goal(&test.goals, test.agent(), |goal| {
        goal.activation == GoalActivation::Disarmed
    })
    .await;
    assert_eq!(goal.phase, GoalPhase::Active);
    assert_eq!(goal.rounds_started, 1);
    assert!(goal.blocked_reason.is_none());
    assert_eq!(test.adapter.requests().len(), 1);
    test.shutdown().await;
}

#[tokio::test]
async fn cancellation_of_unrelated_human_work_disarms_without_pausing_goal() {
    let test = Harness::new([ScriptEntry::Hang], true).await;
    test.agent()
        .followup(user("inspect something first"))
        .unwrap();
    wait_until(|| test.adapter.requests().len() == 1).await;
    let created = create_goal(&test.goals, test.agent(), "continue after inspection", 8);
    test.agent()
        .cancel(AgentCancelCause::User, CancelOptions::default())
        .unwrap();
    test.agent().when_idle().unwrap().await;
    let goal = test.goals.get(test.agent()).unwrap().unwrap();
    assert_eq!(goal.id, created.id);
    assert_eq!(goal.revision, created.revision);
    assert_eq!(goal.phase, GoalPhase::Active);
    assert_eq!(goal.activation, GoalActivation::Disarmed);
    assert_eq!(goal.rounds_started, 0);
    test.shutdown().await;
}

#[tokio::test]
async fn downstream_cancellation_rejects_without_admission_or_reblocking() {
    for reject in [false, true] {
        let test = Harness::new([], true).await;
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let once = cancelled.clone();
        test.context
            .events()
            .on_waterfall(
                &test.context,
                "agent/pre-step",
                move |_, args, next| {
                    let once = once.clone();
                    Box::pin(async move {
                        let event = args.get::<AgentEvent<AgentPreStepEvent>>(0).unwrap();
                        if event
                            .payload
                            .messages
                            .first()
                            .is_some_and(|message| message.source().kind == "goal")
                            && !once.swap(true, std::sync::atomic::Ordering::AcqRel)
                        {
                            event
                                .agent
                                .cancel(AgentCancelCause::User, CancelOptions::default())?;
                            if reject {
                                return Ok(EventReply::Value(Arc::new(PreStepDecision::Reject)));
                            }
                        }
                        next.run().await
                    })
                },
                EventOptions::default(),
            )
            .unwrap();
        create_goal(&test.goals, test.agent(), "cancel during pre-step", 8);
        let goal = wait_goal(&test.goals, test.agent(), |goal| {
            goal.phase == GoalPhase::Paused
        })
        .await;
        assert_eq!(goal.rounds_started, 0);
        assert!(goal.blocked_reason.is_none());
        assert!(test.adapter.requests().is_empty());
        test.shutdown().await;
    }
}

#[tokio::test]
async fn downstream_cancel_then_throw_does_not_reschedule_or_double_clear() {
    let test = Harness::new([], true).await;
    let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = fired.clone();
    test.context
        .events()
        .on_waterfall(
            &test.context,
            "agent/pre-step",
            move |_, args, next| {
                let once = once.clone();
                Box::pin(async move {
                    let event = args.get::<AgentEvent<AgentPreStepEvent>>(0).unwrap();
                    if event
                        .payload
                        .messages
                        .first()
                        .is_some_and(|message| message.source().kind == "goal")
                        && !once.swap(true, std::sync::atomic::Ordering::AcqRel)
                    {
                        event
                            .agent
                            .cancel(AgentCancelCause::User, CancelOptions::default())?;
                        anyhow::bail!("hook cancelled then exploded");
                    }
                    next.run().await
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "cancel then throw", 8);
    let goal = wait_goal(&test.goals, test.agent(), |goal| {
        goal.phase == GoalPhase::Paused
    })
    .await;
    test.agent().when_idle().unwrap().await;
    tokio::task::yield_now().await;
    assert_eq!(goal.rounds_started, 0);
    assert!(test.adapter.requests().is_empty());
    assert_eq!(
        test.goals.get(test.agent()).unwrap().unwrap().phase,
        GoalPhase::Paused
    );
    test.shutdown().await;
}

#[tokio::test]
async fn retry_of_human_failure_does_not_adopt_or_clear_goal_reservation() {
    let test = Harness::new(
        [
            ScriptEntry::FinishError("transient on human turn".to_owned()),
            ScriptEntry::Text("human retry succeeded".to_owned()),
            ScriptEntry::Text("goal round ran".to_owned()),
        ],
        true,
    )
    .await;
    let retried = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = retried.clone();
    test.context
        .events()
        .on_waterfall(
            &test.context,
            "agent/request-error",
            move |_, _, next| {
                let once = once.clone();
                Box::pin(async move {
                    if !once.swap(true, std::sync::atomic::Ordering::AcqRel) {
                        Ok(EventReply::Value(Arc::new(RequestErrorAction::Retry)))
                    } else {
                        next.run().await
                    }
                })
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "ignore foreign retries", 1);
    test.agent().followup(user("human work")).unwrap();
    let goal = wait_goal(&test.goals, test.agent(), |goal| {
        goal.phase == GoalPhase::Blocked
    })
    .await;
    assert_eq!(goal.rounds_started, 1);
    assert_eq!(goal.blocked_reason.as_ref().unwrap().code, "round-limit");
    let requests = test.adapter.requests();
    assert_eq!(requests.len(), 3);
    assert!(request_text(&requests[0]).contains("human work"));
    assert!(request_text(&requests[1]).contains("human work"));
    assert!(request_text(&requests[2]).contains("<goal_round>"));
    test.shutdown().await;
}

#[tokio::test]
async fn pause_commit_failure_falls_back_to_process_local_disarm() {
    let test = Harness::new([], true).await;
    test.context
        .events()
        .on_sync(
            &test.context,
            "internal/dispatch",
            |_, args| {
                if args
                    .get::<String>(1)
                    .is_some_and(|name| name.as_str() == "session/event")
                {
                    let event_args = args
                        .get::<seekdeep_cordis::EventArgs>(2)
                        .expect("event args");
                    let event = event_args
                        .get::<seekdeep_core::session::SessionEvent>(1)
                        .expect("event");
                    if event.event_type == "goal/change"
                        && event.data["operation"] == "pause"
                    {
                        anyhow::bail!("pause failed");
                    }
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = cancelled.clone();
    let agent = test.agent().clone();
    test.context
        .events()
        .on_sync(
            &test.context,
            "agent/inbox/inserted",
            move |_, args| {
                let event = args.get::<AgentEvent<AgentInboxMessage>>(0).unwrap();
                if event.payload.message.source().kind == "goal"
                    && !once.swap(true, std::sync::atomic::Ordering::AcqRel)
                {
                    agent.cancel(AgentCancelCause::User, CancelOptions::default())?;
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "fail closed after cancellation", 8);
    let goal = wait_goal(&test.goals, test.agent(), |goal| {
        goal.activation == GoalActivation::Disarmed
    })
    .await;
    assert_eq!(goal.phase, GoalPhase::Active);
    assert_eq!(goal.revision, 1);
    assert_eq!(goal.rounds_started, 0);
    assert!(test.adapter.requests().is_empty());
    test.shutdown().await;
}

#[tokio::test]
async fn teardown_of_accepted_queued_round_disarms_before_dispatch() {
    let test = Harness::new([], true).await;
    let disposal = Arc::new(Mutex::new(None));
    let slot = disposal.clone();
    let driver = test.driver.as_ref().unwrap().clone();
    test.context
        .events()
        .on_sync(
            &test.context,
            "agent/inbox/inserted",
            move |_, args| {
                let event = args.get::<AgentEvent<AgentInboxMessage>>(0).unwrap();
                if event.payload.message.source().kind == "goal" && slot.lock().is_none() {
                    let driver = driver.clone();
                    *slot.lock() = Some(tokio::spawn(async move { driver.dispose().await }));
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "unload while queued", 8);
    wait_until(|| disposal.lock().is_some()).await;
    let handle = disposal.lock().take().unwrap();
    handle.await.unwrap().unwrap();
    let goal = test.goals.get(test.agent()).unwrap().unwrap();
    assert_eq!(goal.phase, GoalPhase::Active);
    assert_eq!(goal.activation, GoalActivation::Disarmed);
    assert_eq!(goal.rounds_started, 0);
    assert!(test.adapter.requests().is_empty());
    test.shutdown().await;
}

#[tokio::test]
async fn terminal_goal_failure_leaves_human_work_queued_until_a_new_wakeup() {
    let test = Harness::new(
        [
            ScriptEntry::Error("round one broke".to_owned()),
            ScriptEntry::Text("human answer".to_owned()),
        ],
        true,
    )
    .await;
    let queued = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let once = queued.clone();
    let agent = test.agent().clone();
    test.context
        .events()
        .on_sync(
            &test.context,
            "session/event",
            move |_, args| {
                let event = args
                    .get::<seekdeep_core::session::SessionEvent>(1)
                    .expect("event");
                if event.event_type == "user/message"
                    && event.data.pointer("/source/kind").and_then(Value::as_str) == Some("goal")
                    && !once.swap(true, std::sync::atomic::Ordering::AcqRel)
                {
                    let agent = agent.clone();
                    tokio::spawn(async move {
                        let _ = agent.followup(user("human interleaved"));
                    });
                }
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    create_goal(&test.goals, test.agent(), "survive a stale failure", 1);
    wait_goal(&test.goals, test.agent(), |goal| {
        goal.phase == GoalPhase::Active && goal.activation == GoalActivation::Disarmed
    })
    .await;
    wait_until(|| test.agent().inbox().next_turn().len() == 1).await;
    assert_eq!(test.adapter.requests().len(), 1);
    test.agent()
        .steer(user("resume after failure"))
        .expect("new wakeup");
    test.agent().when_idle().unwrap().await;
    let requests = test.adapter.requests();
    assert_eq!(requests.len(), 2);
    assert!(request_text(&requests[1]).contains("human interleaved"));
    assert!(request_text(&requests[1]).contains("resume after failure"));
    test.shutdown().await;
}
