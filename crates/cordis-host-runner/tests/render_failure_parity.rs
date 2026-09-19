//! Exact-run Client render reports and Host-rich snapshot parity.

use seekdeep_cordis::Context;
use seekdeep_cordis_host_runner::{
    CordisDiagnosticPhase, CordisHalfStatus, CordisRunStatus, DynamicCordisCode,
    DynamicCordisDefineRequest, DynamicCordisHostHalfResult, DynamicCordisPluginSelector,
    DynamicCordisRenderFailure, DynamicCordisRunMode, DynamicCordisRunResponse,
    DynamicCordisRunner,
};
use seekdeep_llm::SessionId;

fn failure(slot: &str, message: &str, abdicated: bool) -> DynamicCordisRenderFailure {
    DynamicCordisRenderFailure {
        slot: slot.to_owned(),
        message: message.to_owned(),
        stack: None,
        abdicated,
    }
}

#[tokio::test]
async fn snapshot_keeps_last_exact_run_report_and_structures_the_client_diagnostic() {
    let context = Context::new();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let defined = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "panel".to_owned(),
            },
            name: "panel".to_owned(),
            purpose: "render settings".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    "harness.handle('refresh', async () => null); return { apply() {} };"
                        .to_owned(),
                ),
                client: Some("return { apply() {} };".to_owned()),
            },
        })
        .unwrap();
    let started = runner
        .run_host_half(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    let run_id = match started {
        DynamicCordisHostHalfResult::Success { plugin_run_id, .. } => plugin_run_id,
        failed @ DynamicCordisHostHalfResult::Failure(_) => {
            panic!("unexpected Host failure: {failed:?}")
        }
    };

    runner.report_render_failure(
        &session,
        &defined.plugin_id,
        &run_id,
        &failure("settings.section", "boom", true),
    );
    let later = failure("shell.overlay", "later", false);
    runner.report_render_failure(&session, &defined.plugin_id, &run_id, &later);

    let snapshot = runner.snapshot(&session);
    let active = snapshot[0].active_run.as_ref().unwrap();
    assert_eq!(active.handlers, ["refresh"]);
    assert_eq!(active.render_failure.as_ref(), Some(&later));
    let attempt = snapshot[0].latest_run.as_ref().unwrap();
    assert_eq!(attempt.status, CordisRunStatus::Failed);
    assert_eq!(attempt.client.status, CordisHalfStatus::Failed);
    assert_eq!(attempt.client.error.as_deref(), Some("later"));
    assert_eq!(
        attempt.error.as_ref().unwrap().phase,
        CordisDiagnosticPhase::ClientRender
    );
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn foreign_stale_stopped_and_replaced_render_reports_are_silently_absent() {
    let context = Context::new();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let foreign = SessionId::new("session-b");
    let defined = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "panel".to_owned(),
            },
            name: "panel".to_owned(),
            purpose: "render settings".to_owned(),
            code: DynamicCordisCode {
                host: Some("return { apply() {} };".to_owned()),
                client: None,
            },
        })
        .unwrap();
    let first = runner
        .run(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    let first_run = match first {
        DynamicCordisRunResponse::Success { plugin_run_id, .. } => plugin_run_id,
        failed @ DynamicCordisRunResponse::Failure { .. } => {
            panic!("unexpected run failure: {failed:?}")
        }
    };
    runner.report_render_failure(
        &foreign,
        &defined.plugin_id,
        &first_run,
        &failure("foreign", "ignored", true),
    );
    assert!(
        runner.snapshot(&session)[0]
            .active_run
            .as_ref()
            .unwrap()
            .render_failure
            .is_none()
    );
    runner.report_render_failure(
        &session,
        &defined.plugin_id,
        &first_run,
        &failure("settings", "visible", true),
    );
    assert!(
        runner.snapshot(&session)[0]
            .active_run
            .as_ref()
            .unwrap()
            .render_failure
            .is_some()
    );

    runner.stop(&session, &defined.plugin_id).await;
    assert!(runner.snapshot(&session)[0].active_run.is_none());
    runner.report_render_failure(
        &session,
        &defined.plugin_id,
        &first_run,
        &failure("stale", "ignored", true),
    );
    let second = runner
        .run(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    assert!(matches!(second, DynamicCordisRunResponse::Success { .. }));
    assert!(
        runner.snapshot(&session)[0]
            .active_run
            .as_ref()
            .unwrap()
            .render_failure
            .is_none()
    );
    context.fiber().dispose().await.unwrap();
}
