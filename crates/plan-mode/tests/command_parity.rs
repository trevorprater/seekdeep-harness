//! `/plan` command semantics with a recording Agent controller.

use std::sync::Arc;

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentControlError, AgentController, AgentOptions, CancelOptions, Inbox,
    MaintenanceReservation, NoopInboxNotifications,
};
use seekdeep_cordis::{Context, Fiber};
use seekdeep_core::session::{AgentCancelCause, AppendOptions, Session, SessionId};
use seekdeep_llm::{AbortSignal, ContentBlock, UserMessage};
use seekdeep_plan_mode::{PlanModeConfig, PlanModeController, fold_plan_mode};
use seekdeep_scope::ScopeKey;
use seekdeep_system_prompt::{SystemPrompt, SystemPromptConfig};
use seekdeep_tools::ToolRuntimeConfig;
use serde_json::json;

#[derive(Default)]
struct RecordingController {
    sent: Mutex<Vec<(UserMessage, seekdeep_agent::InboxTarget, bool)>>,
}

impl AgentController for RecordingController {
    fn send(
        &self,
        message: UserMessage,
        target: seekdeep_agent::InboxTarget,
        wakeup: bool,
    ) -> Result<(), AgentControlError> {
        self.sent.lock().push((message, target, wakeup));
        Ok(())
    }

    fn cancel(
        &self,
        _cause: AgentCancelCause,
        _options: CancelOptions,
    ) -> Result<(), AgentControlError> {
        Ok(())
    }

    fn when_idle(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn begin_maintenance(&self) -> Result<MaintenanceReservation, AgentControlError> {
        Ok(MaintenanceReservation::new(
            AbortSignal::default(),
            Arc::new(|| {}),
        ))
    }
}

struct Harness {
    context: Context,
    commands: Arc<seekdeep_commands::CommandRuntime>,
    controller: Arc<PlanModeController>,
    plan_fiber: Arc<Fiber>,
    agent: Arc<Agent>,
    recording: Arc<RecordingController>,
}

impl Harness {
    fn new() -> Self {
        let context = Context::new();
        let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
        prompt.provide(&context).unwrap();
        seekdeep_tools::install(&context, &prompt, ToolRuntimeConfig::default()).unwrap();
        let commands = seekdeep_commands::install(&context).unwrap();
        let plan_fiber = Fiber::active_child("plan-command");
        let plan_context = context.with_fiber(plan_fiber.clone());
        let controller = PlanModeController::install(
            &plan_context,
            &PlanModeConfig {
                section: "plan policy".to_owned(),
            },
        )
        .unwrap();
        let id = SessionId::new("plan-command-agent");
        let session = Session::create(&id, None, None).unwrap();
        let inbox =
            Arc::new(Inbox::new(session.clone(), Arc::new(NoopInboxNotifications)).unwrap());
        let agent = Arc::new(Agent::new(
            id,
            AgentOptions::default(),
            session,
            inbox,
            context.clone(),
            ScopeKey::new(),
        ));
        let recording = Arc::new(RecordingController::default());
        agent.install_controller(recording.clone()).unwrap();
        Self {
            context,
            commands,
            controller,
            plan_fiber,
            agent,
            recording,
        }
    }
}

fn open_turn(agent: &Agent, turn: u64) {
    agent
        .session()
        .append(
            "turn/start",
            json!({"turn": turn}),
            AppendOptions::default(),
        )
        .unwrap();
}

fn close_turn(agent: &Agent, turn: u64) {
    agent
        .session()
        .append(
            "turn/end",
            json!({"turn": turn, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .unwrap();
}

#[tokio::test]
async fn idle_entry_and_exit_use_immediate_copy_and_unknown_commands_do_nothing() {
    let harness = Harness::new();
    assert_eq!(
        harness
            .commands
            .list(&harness.agent)
            .iter()
            .map(|command| command.name.as_str())
            .collect::<Vec<_>>(),
        ["plan"]
    );
    let signal = AbortSignal::default();
    for unknown in ["/mode", "/review"] {
        assert!(
            harness
                .commands
                .execute(harness.agent.clone(), unknown, signal.clone())
                .await
                .unwrap()
                .is_none()
        );
    }
    let entered = harness
        .commands
        .execute(harness.agent.clone(), "/plan", signal.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        entered.result.text(),
        Some("Plan mode on. Use /plan off to leave.")
    );
    assert!(fold_plan_mode(
        &harness.agent.session().events(),
        harness.agent.session().events().len()
    ));
    let exited = harness
        .commands
        .execute(harness.agent.clone(), "/plan off", signal.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(exited.result.text(), Some("Plan mode off."));
    let repeated = harness
        .commands
        .execute(harness.agent.clone(), "/plan off", signal)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        repeated.result.text(),
        Some("Plan mode is already inactive.")
    );
    assert!(harness.recording.sent.lock().is_empty());
    harness.plan_fiber.dispose().await.unwrap();
    harness.context.root_fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn message_entry_steers_trimmed_text_and_off_cancels_pending_without_logging_mode() {
    let harness = Harness::new();
    open_turn(&harness.agent, 1);
    let entered = harness
        .commands
        .execute(
            harness.agent.clone(),
            "/plan   draft the migration  ",
            AbortSignal::default(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        entered
            .result
            .text()
            .unwrap()
            .starts_with("Entering plan mode")
    );
    assert_eq!(harness.controller.get(&harness.agent).pending, Some(true));
    {
        let sent = harness.recording.sent.lock();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].1, seekdeep_agent::InboxTarget::NextStep);
        assert!(sent[0].2);
        assert_eq!(
            sent[0]
                .0
                .content()
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>(),
            "draft the migration"
        );
    }
    let cancelled = harness
        .commands
        .execute(harness.agent.clone(), "/plan off", AbortSignal::default())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.result.text(), Some("Plan mode entry cancelled."));
    close_turn(&harness.agent, 1);
    assert!(
        !harness
            .agent
            .session()
            .events()
            .iter()
            .any(|event| event.event_type == "plan/mode")
    );
    harness.plan_fiber.dispose().await.unwrap();
    harness.context.root_fiber().dispose().await.unwrap();
}
