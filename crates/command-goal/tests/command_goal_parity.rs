//! Behavioral mirror of `packages/goal/command-goal/tests/command-goal.spec.ts`.

use std::sync::Arc;

use seekdeep_agent::{Agent, AgentOptions, AgentRegistry, Inbox, NoopInboxNotifications};
use seekdeep_commands::CommandResult;
use seekdeep_cordis::Context;
use seekdeep_core::session::{Session, SessionId};
use seekdeep_goal::{Config as GoalConfig, CreateGoalRequest, GoalPhase, GoalRef, GoalService};
use seekdeep_llm::AbortSignal;
use seekdeep_scope::ScopeKey;

struct Harness {
    context: Context,
    commands: Arc<seekdeep_commands::CommandRuntime>,
    goals: Arc<GoalService>,
    agent: Arc<Agent>,
    plugin: Arc<seekdeep_cordis::PluginFiber>,
}

impl Harness {
    async fn new() -> Self {
        let context = Context::new();
        let agents = Arc::new(AgentRegistry::new(context.clone()));
        agents.provide(&context).expect("agents");
        let commands = seekdeep_commands::install(&context).expect("commands");
        let goals = GoalService::install(&context, GoalConfig::default()).expect("goals");
        let plugin = context
            .plugin(seekdeep_command_goal::plugin(), serde_json::json!({}))
            .expect("mount command-goal");
        plugin.await_settled().await.expect("settle command-goal");
        let agent = agent("command-goal");
        agents
            .register(&context, &agent, None)
            .expect("register agent");
        Self {
            context,
            commands,
            goals,
            agent,
            plugin,
        }
    }

    async fn run(&self, suffix: &str) -> anyhow::Result<CommandResult> {
        let execution = self
            .commands
            .execute(
                self.agent.clone(),
                &format!("/goal{suffix}"),
                AbortSignal::default(),
            )
            .await?
            .ok_or_else(|| anyhow::anyhow!("goal command was not registered"))?;
        Ok(execution.result)
    }
}

fn agent(raw_id: &str) -> Arc<Agent> {
    let id = SessionId::new(raw_id);
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

fn domain_event_types(session: &Session) -> Vec<String> {
    session
        .events()
        .into_iter()
        .filter(|event| !matches!(event.event_type.as_str(), "command/run" | "command/done"))
        .map(|event| event.event_type)
        .collect()
}

fn goal_ref(goal: &seekdeep_goal::GoalView) -> GoalRef {
    GoalRef {
        id: goal.id.clone(),
        revision: goal.revision,
    }
}

fn assert_kind_and_text(result: &CommandResult, kind: &str, fragment: &str) {
    assert_eq!(result.kind(), kind);
    assert!(
        result.text().is_some_and(|text| text.contains(fragment)),
        "expected {fragment:?} in {result:?}"
    );
}

#[tokio::test]
async fn loader_metadata_registration_and_disposal_are_exact() {
    let harness = Harness::new().await;
    let plugin = seekdeep_command_goal::plugin();
    assert_eq!(plugin.name(), "command-goal");
    assert_eq!(plugin.inject(), ["commands", "goals"]);
    assert!(harness.commands.find(&harness.agent, "goal").is_some());
    assert!(harness.commands.list(&harness.agent).iter().any(|command| {
        command.name == "goal"
            && command.description == "set or view the goal for a long-running task"
            && command.input.as_ref().map(|input| input.hint.as_str())
                == Some("[<objective>|clear|edit <objective>|pause|resume]")
    }));

    harness.plugin.dispose().await.expect("dispose plugin");
    assert!(harness.commands.find(&harness.agent, "goal").is_none());
    harness
        .context
        .fiber()
        .dispose()
        .await
        .expect("dispose root");
}

#[tokio::test]
async fn empty_status_does_not_mutate_the_goal_domain() {
    let harness = Harness::new().await;
    assert_eq!(
        harness.run("").await.expect("show"),
        CommandResult::success(Some(
            "No goal is currently set.\nUsage: /goal [<objective>|clear|edit <objective>|pause|resume]"
        ))
    );
    assert!(domain_event_types(harness.agent.session()).is_empty());
    harness.context.fiber().dispose().await.expect("dispose");
}

#[tokio::test]
async fn creates_trimmed_objective_and_refuses_silent_replacement() {
    let harness = Harness::new().await;
    let created = harness
        .run("\n  finish the release  ")
        .await
        .expect("create");
    assert_kind_and_text(&created, "success", "Goal created\nStatus: active");
    assert_kind_and_text(&created, "success", "Objective: finish the release");
    assert_kind_and_text(&created, "success", "Rounds: 0/256");
    assert_kind_and_text(&created, "success", "Activation: armed");
    assert_eq!(
        harness
            .goals
            .get(&harness.agent)
            .expect("get")
            .expect("goal")
            .objective,
        "finish the release"
    );
    assert_eq!(domain_event_types(harness.agent.session()), ["goal/change"]);

    let count = domain_event_types(harness.agent.session()).len();
    assert_eq!(
        harness.run(" replacement").await.expect("replacement"),
        CommandResult::error(
            "A goal is already active. Use /goal edit <objective> to change it or /goal clear before replacing it."
        )
    );
    assert_eq!(domain_event_types(harness.agent.session()).len(), count);

    let exact_control = Harness::new().await;
    exact_control
        .run(" pause everything only after verification")
        .await
        .expect("objective");
    assert_eq!(
        exact_control
            .goals
            .get(&exact_control.agent)
            .expect("get")
            .expect("goal")
            .objective,
        "pause everything only after verification"
    );
}

#[tokio::test]
async fn edits_inline_and_replaces_only_a_completed_goal() {
    let empty = Harness::new().await;
    assert_kind_and_text(
        &empty.run(" edit").await.expect("invalid edit"),
        "error",
        "requires a replacement objective",
    );
    assert_kind_and_text(
        &empty.run(" edit replacement").await.expect("missing edit"),
        "error",
        "/goal edit requires one",
    );

    let harness = Harness::new().await;
    harness.run(" first").await.expect("first");
    let first = harness
        .goals
        .get(&harness.agent)
        .expect("get")
        .expect("first");
    assert_kind_and_text(
        &harness.run(" EDIT\n  second  ").await.expect("edit"),
        "success",
        "Goal updated",
    );
    let second = harness
        .goals
        .get(&harness.agent)
        .expect("get")
        .expect("second");
    assert_eq!(second.id, first.id);
    assert_eq!(second.objective, "second");
    assert_eq!(second.revision, 2);
    harness
        .goals
        .complete(&harness.agent, &goal_ref(&second))
        .expect("complete");
    assert_kind_and_text(
        &harness.run(" edit third").await.expect("replace completed"),
        "success",
        "Goal created",
    );
    let third = harness
        .goals
        .get(&harness.agent)
        .expect("get")
        .expect("third");
    assert_eq!(third.objective, "third");
    assert_eq!(third.revision, 1);
    assert_ne!(third.id, first.id);
}

#[tokio::test]
async fn missing_pause_resume_and_clear_have_direct_results() {
    let harness = Harness::new().await;
    assert_kind_and_text(
        &harness.run(" pause").await.expect("pause"),
        "error",
        "/goal pause requires one",
    );
    assert_kind_and_text(
        &harness.run(" resume").await.expect("resume"),
        "error",
        "/goal resume requires one",
    );
    assert_eq!(
        harness.run(" clear").await.expect("clear"),
        CommandResult::success(Some("No goal to clear."))
    );
}

#[tokio::test]
async fn pause_resume_clear_and_expected_domain_rejections_are_stable() -> anyhow::Result<()> {
    let harness = Harness::new().await;
    harness.run(" work").await.expect("create");
    assert_eq!(
        harness.run(" RESUME").await.expect("redundant resume"),
        CommandResult::error(
            "The goal command is not valid for the current state. Run /goal to view available commands."
        )
    );
    assert_kind_and_text(
        &harness.run(" PAUSE").await.expect("pause"),
        "success",
        "Goal paused",
    );
    let paused = harness.goals.get(&harness.agent)?.expect("paused goal");
    assert_eq!(paused.phase, GoalPhase::Paused);
    assert_kind_and_text(
        &harness.run(" resume").await.expect("resume"),
        "success",
        "Goal resumed",
    );
    assert_eq!(
        harness.goals.get(&harness.agent)?.expect("active").phase,
        GoalPhase::Active
    );
    assert_eq!(
        harness.run(" clear").await.expect("clear"),
        CommandResult::success(Some("Goal cleared."))
    );
    assert!(harness.goals.get(&harness.agent)?.is_none());
    Ok::<_, anyhow::Error>(())
}

#[tokio::test]
async fn status_renders_disarmed_paused_blocked_and_complete_states() -> anyhow::Result<()> {
    let harness = Harness::new().await;
    harness.goals.create(
        &harness.agent,
        &CreateGoalRequest {
            objective: "state matrix".to_owned(),
            max_goal_rounds: Some(1),
        },
    )?;
    harness.goals.disarm(&harness.agent)?;
    assert_kind_and_text(
        &harness.run("").await?,
        "success",
        "Status: active\nObjective: state matrix\nRounds: 0/1\nActivation: disarmed",
    );
    assert_kind_and_text(&harness.run("").await?, "success", "/goal resume");

    let mut goal = harness.goals.get(&harness.agent)?.expect("goal");
    goal = harness.goals.resume(&harness.agent, &goal_ref(&goal))?;
    goal = harness.goals.pause(&harness.agent, &goal_ref(&goal))?;
    assert_kind_and_text(&harness.run("").await?, "success", "Status: paused");

    goal = harness.goals.resume(&harness.agent, &goal_ref(&goal))?;
    goal = harness.goals.block(
        &harness.agent,
        &goal_ref(&goal),
        &serde_json::json!({
            "code": "upstream-unavailable",
            "message": "Provider unavailable",
        }),
    )?;
    assert_kind_and_text(&harness.run("").await?, "success", "Status: blocked");
    assert_kind_and_text(
        &harness.run("").await?,
        "success",
        "Blocker: upstream-unavailable: Provider unavailable",
    );

    goal = harness.goals.resume(&harness.agent, &goal_ref(&goal))?;
    harness.goals.complete(&harness.agent, &goal_ref(&goal))?;
    assert_kind_and_text(&harness.run("").await?, "success", "Status: complete");
    assert_kind_and_text(
        &harness.run("").await?,
        "success",
        "Commands: /goal <objective>, /goal clear",
    );
    Ok(())
}

#[tokio::test]
async fn unexpected_missing_service_failure_propagates() {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).expect("agents");
    let commands = seekdeep_commands::install(&context).expect("commands");
    let goal_fiber = seekdeep_cordis::Fiber::active_child("goals");
    let goal_context = context.with_fiber(goal_fiber.clone());
    GoalService::install(&goal_context, GoalConfig::default()).expect("goals");
    seekdeep_command_goal::apply(&context).expect("command");
    let agent = agent("unexpected");
    agents
        .register(&context, &agent, None)
        .expect("register agent");
    goal_fiber.dispose().await.expect("dispose goals");

    let error = commands
        .execute(agent, "/goal", AbortSignal::default())
        .await
        .expect_err("unexpected service failure");
    assert!(error.to_string().contains("command-goal requires goals"));
}
