//! Agent steering, non-waking panel context, and runtime-failure deduplication parity.

use std::sync::Arc;

use futures::future::BoxFuture;
use parking_lot::Mutex;
use seekdeep_agent::{
    Agent, AgentControlError, AgentController, AgentOptions, AgentRegistry, CancelOptions, Inbox,
    InboxTarget, MaintenanceReservation, NoopInboxNotifications,
};
use seekdeep_cordis::Context;
use seekdeep_cordis_host_runner::{
    CordisErrorDetails, DynamicCordisCode, DynamicCordisDefineRequest, DynamicCordisHostHalfResult,
    DynamicCordisPluginSelector, DynamicCordisRenderFailure, DynamicCordisRunFailureReason,
    DynamicCordisRunMode, DynamicCordisRunResolution, DynamicCordisRunResponse,
    DynamicCordisRunner,
};
use seekdeep_core::session::{AgentCancelCause, Session};
use seekdeep_llm::{AbortSignal, ContentBlock, SessionId, UserMessage};
use seekdeep_scope::ScopeKey;
use serde_json::json;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Sent {
    text: String,
    target: InboxTarget,
    wakeup: bool,
}

#[derive(Default)]
struct RecordingController {
    sent: Mutex<Vec<Sent>>,
}

impl AgentController for RecordingController {
    fn send(
        &self,
        message: UserMessage,
        target: InboxTarget,
        wakeup: bool,
    ) -> Result<(), AgentControlError> {
        let text = match message.content() {
            [ContentBlock::Text { text }] => text.clone(),
            content => format!("{content:?}"),
        };
        self.sent.lock().push(Sent {
            text,
            target,
            wakeup,
        });
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
    runner: Arc<DynamicCordisRunner>,
    controller: Arc<RecordingController>,
    session: SessionId,
}

fn harness() -> Harness {
    let context = Context::new();
    let agents = Arc::new(AgentRegistry::new(context.clone()));
    agents.provide(&context).unwrap();
    let session = SessionId::new("session-a");
    let durable = Session::create(&session, None, None).unwrap();
    let inbox = Arc::new(Inbox::new(durable.clone(), Arc::new(NoopInboxNotifications)).unwrap());
    let agent = Arc::new(Agent::new(
        session.clone(),
        AgentOptions::default(),
        durable,
        inbox,
        context.clone(),
        ScopeKey::new(),
    ));
    let controller = Arc::new(RecordingController::default());
    agent.install_controller(controller.clone()).unwrap();
    agents.register(&context, &agent, None).unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    Harness {
        context,
        runner,
        controller,
        session,
    }
}

fn define(
    harness: &Harness,
    prefix: &str,
    host: &str,
    client: Option<&str>,
) -> seekdeep_cordis_host_runner::DynamicCordisDefineReceipt {
    harness
        .runner
        .define(DynamicCordisDefineRequest {
            session_id: harness.session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: prefix.to_owned(),
            },
            name: prefix.to_owned(),
            purpose: "feedback".to_owned(),
            code: DynamicCordisCode {
                host: Some(host.to_owned()),
                client: client.map(str::to_owned),
            },
        })
        .unwrap()
}

#[tokio::test]
async fn model_resolution_steers_but_manual_run_stop_and_remove_only_inject() {
    let harness = harness();
    let rejected = define(
        &harness,
        "panel",
        "return { apply() {} };",
        Some("return { apply() {} }"),
    );
    harness
        .runner
        .run(
            &harness.session,
            &rejected.plugin_id,
            &rejected.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    let request = harness
        .runner
        .registry()
        .pending_request_for(&rejected.plugin_id)
        .unwrap();
    harness
        .runner
        .resolve_request_run(
            &request,
            &DynamicCordisRunResolution::Failure {
                reason: DynamicCordisRunFailureReason::Rejected,
                plugin_run_id: None,
                started_here: None,
                message: None,
                stack: None,
            },
        )
        .await;
    assert!(harness.controller.sent.lock()[0].wakeup);
    assert!(
        harness.controller.sent.lock()[0]
            .text
            .contains("user rejected")
    );

    let manual = define(
        &harness,
        "clock",
        "return { apply() {} };",
        Some("return { apply() {} }"),
    );
    let started = harness
        .runner
        .run_host_half(
            &harness.session,
            &manual.plugin_id,
            &manual.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    let run_id = match started {
        DynamicCordisHostHalfResult::Success { plugin_run_id, .. } => plugin_run_id,
        failure @ DynamicCordisHostHalfResult::Failure(_) => panic!("{failure:?}"),
    };
    harness
        .runner
        .settle_user_run(
            &harness.session,
            &manual.plugin_id,
            &DynamicCordisRunResolution::Success {
                plugin_run_id: run_id,
                waiting_for: None,
            },
        )
        .await;
    harness
        .runner
        .stop_from_panel(&harness.session, &manual.plugin_id)
        .await;
    harness
        .runner
        .undefine_from_panel(&harness.session, &manual.plugin_id)
        .await;
    let sent = harness.controller.sent.lock().clone();
    assert_eq!(sent.len(), 4);
    assert!(sent[1..].iter().all(|message| !message.wakeup));
    assert!(sent[1].text.contains("manually ran"));
    assert!(sent[2].text.contains("user stopped"));
    assert!(sent[3].text.contains("user removed"));
    harness.context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn repeated_handler_render_and_guard_failures_steer_once_per_error_key() {
    let harness = harness();
    let defined = define(
        &harness,
        "guard",
        concat!(
            "harness.handle('boom', async () => { throw new Error('handler broke'); });",
            "return { apply(ctx) { ctx.on('late-guard', () => { const root = ctx.root; }); } };",
        ),
        None,
    );
    let run = harness
        .runner
        .run(
            &harness.session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    let run_id = match run {
        DynamicCordisRunResponse::Success { plugin_run_id, .. } => plugin_run_id,
        failure @ DynamicCordisRunResponse::Failure { .. } => panic!("{failure:?}"),
    };
    for _ in 0..2 {
        harness
            .runner
            .invoke(&defined.plugin_id, &run_id, "boom", json!(null))
            .await;
    }
    for _ in 0..2 {
        assert!(
            harness
                .context
                .events()
                .parallel(
                    &harness.context,
                    "late-guard",
                    &seekdeep_cordis::EventArgs::new(),
                )
                .await
                .is_err()
        );
    }
    let guard = CordisErrorDetails {
        message: "guard broke".to_owned(),
        stack: None,
    };
    harness.runner.report_client_guard_failure(
        &harness.session,
        &defined.plugin_id,
        &run_id,
        &guard,
    );
    let render = DynamicCordisRenderFailure {
        slot: "settings".to_owned(),
        message: "render broke".to_owned(),
        stack: None,
        abdicated: true,
    };
    harness
        .runner
        .report_render_failure(&harness.session, &defined.plugin_id, &run_id, &render);
    harness
        .runner
        .report_render_failure(&harness.session, &defined.plugin_id, &run_id, &render);
    harness.runner.report_client_guard_failure(
        &harness.session,
        &defined.plugin_id,
        &run_id,
        &guard,
    );
    let sent = harness.controller.sent.lock().clone();
    assert_eq!(sent.len(), 4);
    assert!(sent.iter().all(|message| message.wakeup));
    assert!(sent[0].text.contains("handler broke"));
    assert!(sent[1].text.contains("sandbox ctx does not expose"));
    assert!(sent[2].text.contains("guard broke"));
    assert!(sent[3].text.contains("render broke"));
    harness.context.fiber().dispose().await.unwrap();
}
