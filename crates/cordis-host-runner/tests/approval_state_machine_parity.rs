//! Approval, automatic authorization, rejection, and cancellation state-machine parity.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_cordis_host_runner::{
    ApprovalRequestId, CordisDiagnosticPhase, CordisHalfStatus, CordisRunStatus, DynamicCordisCode,
    DynamicCordisDefineReceipt, DynamicCordisDefineRequest, DynamicCordisHostHalfResult,
    DynamicCordisPluginSelector, DynamicCordisRequestResolved, DynamicCordisRunFailureReason,
    DynamicCordisRunMode, DynamicCordisRunResolution, DynamicCordisRunResponse,
    DynamicCordisRunSuccessStatus, DynamicCordisRunner, RequestRunOutcome,
};
use seekdeep_llm::{AbortSignal, SessionId};

fn define_panel(runner: &DynamicCordisRunner, session: &SessionId) -> DynamicCordisDefineReceipt {
    runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "panel".to_owned(),
            },
            name: "panel".to_owned(),
            purpose: "render panel".to_owned(),
            code: DynamicCordisCode {
                host: Some("return { apply() {} };".to_owned()),
                client: Some("return { apply() {} };".to_owned()),
            },
        })
        .unwrap()
}

fn successful_run(
    response: DynamicCordisRunResponse,
) -> seekdeep_cordis_host_runner::CordisDynamicPluginRunId {
    match response {
        DynamicCordisRunResponse::Success { plugin_run_id, .. } => plugin_run_id,
        failed @ DynamicCordisRunResponse::Failure { .. } => {
            panic!("unexpected run failure: {failed:?}")
        }
    }
}

fn resolved_events(context: &Context) -> Arc<Mutex<Vec<DynamicCordisRequestResolved>>> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = events.clone();
    context
        .events()
        .on_sync(
            context,
            "cordis/request-run-resolved",
            move |_, args| {
                observed.lock().push(
                    args.get::<DynamicCordisRequestResolved>(0)
                        .unwrap()
                        .as_ref()
                        .clone(),
                );
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    events
}

#[tokio::test]
async fn explicit_approval_moves_through_client_pending_and_clears_transient_fields_on_commit() {
    let context = Context::new();
    let resolved = resolved_events(&context);
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let defined = define_panel(&runner, &session);
    let requested = runner
        .run(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    let run_id = successful_run(requested);
    let request_id = runner
        .registry()
        .pending_request_for(&defined.plugin_id)
        .unwrap();
    let host = runner
        .run_host_half_for_request(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
            &request_id,
            false,
        )
        .await;
    assert!(matches!(host, DynamicCordisHostHalfResult::Success { .. }));
    let pending = runner.inventory()[0].latest_run.clone().unwrap();
    assert_eq!(pending.status, CordisRunStatus::ClientPending);
    assert_eq!(pending.host.status, CordisHalfStatus::Running);
    assert_eq!(pending.approval_request_id.as_ref(), Some(&request_id));
    assert_eq!(pending.requires_approval, Some(true));

    assert!(
        runner
            .resolve_request_run(
                &request_id,
                &DynamicCordisRunResolution::Success {
                    plugin_run_id: run_id.clone(),
                    waiting_for: None,
                },
            )
            .await
            .accepted
    );
    let committed = runner.inventory()[0].latest_run.clone().unwrap();
    assert_eq!(committed.status, CordisRunStatus::Running);
    assert_eq!(committed.client.status, CordisHalfStatus::Running);
    assert!(committed.approval_request_id.is_none());
    assert!(committed.requires_approval.is_none());
    assert!(committed.error.is_none());
    assert!(
        runner
            .registry()
            .get(&defined.plugin_id)
            .unwrap()
            .lock()
            .run
            .as_ref()
            .unwrap()
            .started_for_request
            .is_none()
    );
    assert_eq!(resolved.lock()[0].outcome, RequestRunOutcome::Approved);
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn pre_aborted_call_is_atomic_but_a_published_request_outlives_later_abort() {
    let context = Context::new();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let defined = define_panel(&runner, &session);
    let pre_aborted = AbortSignal::default();
    pre_aborted.abort();
    let cancelled = runner
        .run_with_signal(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
            Some(&pre_aborted),
        )
        .await;
    assert!(matches!(
        cancelled,
        DynamicCordisRunResponse::Failure {
            reason: DynamicCordisRunFailureReason::Cancelled,
            ref message,
            ..
        } if message.contains("cancelled before activation")
    ));
    assert!(runner.inventory()[0].latest_run.is_none());

    let live = AbortSignal::default();
    let requested = runner
        .run_with_signal(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
            Some(&live),
        )
        .await;
    assert!(matches!(
        requested,
        DynamicCordisRunResponse::Success {
            status: DynamicCordisRunSuccessStatus::AwaitingApproval,
            ref plugin_run_id,
            ..
        } if plugin_run_id.as_str() == "run-1"
    ));
    let request_id = runner
        .registry()
        .pending_request_for(&defined.plugin_id)
        .unwrap();
    assert_eq!(request_id, ApprovalRequestId::new("approval-1"));
    live.abort();
    assert_eq!(
        runner
            .registry()
            .pending_request_for(&defined.plugin_id)
            .as_ref(),
        Some(&request_id)
    );
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn rejection_uses_the_source_outcome_and_approval_diagnostic() {
    let context = Context::new();
    let resolved = resolved_events(&context);
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let rejected = define_panel(&runner, &session);
    runner
        .run(
            &session,
            &rejected.plugin_id,
            &rejected.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    let rejection_id = runner
        .registry()
        .pending_request_for(&rejected.plugin_id)
        .unwrap();
    runner
        .resolve_request_run(
            &rejection_id,
            &DynamicCordisRunResolution::Failure {
                reason: DynamicCordisRunFailureReason::Rejected,
                plugin_run_id: None,
                started_here: None,
                message: None,
                stack: None,
            },
        )
        .await;
    let attempt = runner.inventory()[0].latest_run.clone().unwrap();
    assert_eq!(attempt.status, CordisRunStatus::Rejected);
    assert_eq!(attempt.client.status, CordisHalfStatus::Stopped);
    assert_eq!(
        attempt.error.as_ref().unwrap().phase,
        CordisDiagnosticPhase::Approval
    );
    assert_eq!(
        attempt.error.as_ref().unwrap().message,
        "the run request was declined"
    );
    assert_eq!(resolved.lock()[0].outcome, RequestRunOutcome::Rejected);
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn automatically_authorized_failure_announces_completion_and_retracts_its_run() {
    let context = Context::new();
    let resolved = resolved_events(&context);
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let approved = define_panel(&runner, &session);
    let direct = runner
        .run_host_half(
            &session,
            &approved.plugin_id,
            &approved.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    let direct_run = match direct {
        DynamicCordisHostHalfResult::Success { plugin_run_id, .. } => plugin_run_id,
        failure @ DynamicCordisHostHalfResult::Failure(_) => {
            panic!("unexpected direct failure: {failure:?}")
        }
    };
    runner
        .settle_user_run(
            &session,
            &approved.plugin_id,
            &DynamicCordisRunResolution::Success {
                plugin_run_id: direct_run,
                waiting_for: None,
            },
        )
        .await;
    let automatic = runner
        .run(
            &session,
            &approved.plugin_id,
            &approved.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    assert!(matches!(
        automatic,
        DynamicCordisRunResponse::Success {
            status: DynamicCordisRunSuccessStatus::Starting,
            ..
        }
    ));
    let automatic_id = runner
        .registry()
        .pending_request_for(&approved.plugin_id)
        .unwrap();
    let host = runner
        .run_host_half_for_request(
            &session,
            &approved.plugin_id,
            &approved.package_id,
            DynamicCordisRunMode::Run,
            &automatic_id,
            false,
        )
        .await;
    let automatic_run = match host {
        DynamicCordisHostHalfResult::Success { plugin_run_id, .. } => plugin_run_id,
        failure @ DynamicCordisHostHalfResult::Failure(_) => {
            panic!("unexpected automatic Host failure: {failure:?}")
        }
    };
    runner
        .resolve_request_run(
            &automatic_id,
            &DynamicCordisRunResolution::Failure {
                reason: DynamicCordisRunFailureReason::ClientHalfFailed,
                plugin_run_id: Some(automatic_run),
                started_here: Some(true),
                message: Some("browser failed".to_owned()),
                stack: None,
            },
        )
        .await;
    assert_eq!(resolved.lock()[0].outcome, RequestRunOutcome::Completed);
    let row = &runner.inventory()[0];
    assert!(row.active_run.is_none());
    assert_eq!(row.current_package_id.as_ref(), Some(&approved.package_id));
    assert_eq!(
        row.latest_run.as_ref().unwrap().status,
        CordisRunStatus::Failed
    );
    context.fiber().dispose().await.unwrap();
}
