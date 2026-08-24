//! Behavioral mirror of the plan projection source suite.

use std::sync::Arc;

use seekdeep_cordis::{Context, Fiber};
use seekdeep_core::{
    session::{AppendOptions, Session, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_plan_mode::{PlanModeConfig, PlanModeController};
use seekdeep_session_projection::{ProjectionSnapshot, SessionProjectionRegistry};
use seekdeep_system_prompt::{SystemPrompt, SystemPromptConfig};
use seekdeep_tools::ToolRuntimeConfig;
use serde_json::{Value, json};

struct Harness {
    context: Context,
    projections: Arc<SessionProjectionRegistry>,
    session: Arc<Session>,
    _plan_fiber: Option<Arc<Fiber>>,
}

impl Harness {
    fn new(with_plan_mode: bool) -> Self {
        let context = Context::new();
        let sessions = SessionStore::install(&context).expect("sessions");
        let prompt = SystemPrompt::new(
            &context,
            SystemPromptConfig {
                persona: String::new(),
                ..SystemPromptConfig::default()
            },
        )
        .expect("prompt");
        prompt.provide(&context).expect("provide prompt");
        seekdeep_tools::install(&context, &prompt, ToolRuntimeConfig::default()).expect("tools");
        let projections = SessionProjectionRegistry::install(&context).expect("projections");
        let plan_fiber = with_plan_mode.then(|| {
            let fiber = Fiber::active_child("plan-mode-projection");
            let child = context.with_fiber(fiber.clone());
            PlanModeController::new(
                &child,
                &PlanModeConfig {
                    section: "plan policy".to_owned(),
                },
            )
            .expect("plan mode");
            fiber
        });
        let session = sessions
            .create(
                &context,
                Some(SessionId::new("plan-projection")),
                CreateSessionOptions::default(),
            )
            .expect("session");
        Self {
            context,
            projections,
            session,
            _plan_fiber: plan_fiber,
        }
    }

    fn values(&self) -> ProjectionSnapshot {
        self.projections.snapshot(&self.session).expect("snapshot")
    }
}

fn run_plan_command(session: &Session, args: Option<&str>, index: u64, name: &str) {
    let mut data = serde_json::Map::from_iter([
        ("commandId".to_owned(), json!(format!("plan-proj-{index}"))),
        ("name".to_owned(), json!(name)),
        ("source".to_owned(), json!({"kind": "user"})),
    ]);
    if let Some(args) = args {
        data.insert("args".to_owned(), json!(args));
    }
    session
        .append("command/run", Value::Object(data), AppendOptions::default())
        .expect("command/run");
}

fn commit_plan_mode(session: &Session, active: bool, turn: u64) {
    session
        .append(
            "turn/start",
            json!({"turn": turn}),
            AppendOptions::default(),
        )
        .unwrap();
    session
        .append(
            "plan/mode",
            json!({"active": active}),
            AppendOptions::default(),
        )
        .unwrap();
    session
        .append(
            "turn/end",
            json!({"turn": turn, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .unwrap();
}

fn plan(snapshot: &ProjectionSnapshot) -> &Value {
    snapshot.values.get("plan").expect("plan value")
}

#[test]
fn empty_log_is_inactive_and_not_pending() {
    let harness = Harness::new(true);
    assert_eq!(
        plan(&harness.values()),
        &json!({"active": false, "pending": false})
    );
}

#[test]
fn logged_selection_is_pending_until_mode_commit_and_repeats_are_stable() {
    let harness = Harness::new(true);
    run_plan_command(&harness.session, Some(""), 0, "plan");
    assert_eq!(
        plan(&harness.values()),
        &json!({"active": false, "pending": true})
    );
    run_plan_command(&harness.session, Some(""), 1, "plan");
    assert_eq!(
        plan(&harness.values()),
        &json!({"active": false, "pending": true})
    );
    commit_plan_mode(&harness.session, true, 0);
    assert_eq!(
        plan(&harness.values()),
        &json!({"active": true, "pending": false})
    );
}

#[test]
fn off_missing_input_non_plan_and_matching_selection_fold_exactly() {
    let harness = Harness::new(true);
    commit_plan_mode(&harness.session, true, 0);
    run_plan_command(&harness.session, Some(""), 0, "compact");
    assert_eq!(
        plan(&harness.values()),
        &json!({"active": true, "pending": false})
    );
    run_plan_command(&harness.session, None, 1, "plan");
    assert_eq!(
        plan(&harness.values()),
        &json!({"active": true, "pending": false})
    );
    run_plan_command(&harness.session, Some(" off"), 2, "plan");
    assert_eq!(
        plan(&harness.values()),
        &json!({"active": true, "pending": true})
    );
    commit_plan_mode(&harness.session, false, 1);
    assert_eq!(
        plan(&harness.values()),
        &json!({"active": false, "pending": false})
    );
    run_plan_command(&harness.session, Some("off"), 3, "plan");
    assert_eq!(
        plan(&harness.values()),
        &json!({"active": false, "pending": false})
    );
}

#[test]
fn message_argument_targets_plan_mode() {
    let harness = Harness::new(true);
    run_plan_command(
        &harness.session,
        Some(" sketch the refactor first"),
        0,
        "plan",
    );
    assert_eq!(
        plan(&harness.values()),
        &json!({"active": false, "pending": true})
    );
}

#[tokio::test]
async fn composition_and_hmr_control_the_projection_key() {
    let absent = Harness::new(false);
    assert!(!absent.values().values.contains_key("plan"));

    let fiber = Fiber::active_child("late-plan-mode-projection");
    let child = absent.context.with_fiber(fiber.clone());
    PlanModeController::new(
        &child,
        &PlanModeConfig {
            section: "plan policy".to_owned(),
        },
    )
    .unwrap();
    assert_eq!(
        plan(&absent.values()),
        &json!({"active": false, "pending": false})
    );
    fiber.dispose().await.unwrap();
    assert!(!absent.values().values.contains_key("plan"));
}

#[tokio::test]
async fn optional_projection_service_mounts_unmounts_and_rebinds_after_plan_mode() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let prompt = SystemPrompt::new(&context, SystemPromptConfig::default()).unwrap();
    prompt.provide(&context).unwrap();
    seekdeep_tools::install(&context, &prompt, ToolRuntimeConfig::default()).unwrap();
    let controller_fiber = Fiber::active_child("plan-before-projections");
    let controller_context = context.with_fiber(controller_fiber.clone());
    PlanModeController::new(
        &controller_context,
        &PlanModeConfig {
            section: "plan policy".to_owned(),
        },
    )
    .unwrap();
    let session = sessions
        .create(
            &context,
            Some(SessionId::new("late-projections")),
            CreateSessionOptions::default(),
        )
        .unwrap();

    let first_fiber = Fiber::active_child("projections-first");
    let first_context = context.with_fiber(first_fiber.clone());
    let first = SessionProjectionRegistry::install(&first_context).unwrap();
    assert_eq!(
        first.snapshot(&session).unwrap().values["plan"],
        json!({"active": false, "pending": false})
    );
    first_fiber.dispose().await.unwrap();
    assert!(
        !first
            .snapshot(&session)
            .unwrap()
            .values
            .contains_key("plan")
    );

    let second_fiber = Fiber::active_child("projections-second");
    let second_context = context.with_fiber(second_fiber.clone());
    let second = SessionProjectionRegistry::install(&second_context).unwrap();
    assert_eq!(
        second.snapshot(&session).unwrap().values["plan"],
        json!({"active": false, "pending": false})
    );
    second_fiber.dispose().await.unwrap();
    controller_fiber.dispose().await.unwrap();
}

#[test]
fn cold_replay_recovers_pending_from_the_log_alone() {
    let hot = Harness::new(true);
    run_plan_command(&hot.session, Some(""), 0, "plan");
    let cold = Harness::new(true);
    for event in hot.session.events() {
        if matches!(event.event_type.as_str(), "command/run" | "plan/mode") {
            cold.session
                .append(&event.event_type, event.data, AppendOptions::default())
                .unwrap();
        }
    }
    assert_eq!(
        plan(&cold.values()),
        &json!({"active": false, "pending": true})
    );
}
