//! Approval-authorized Host activation, Client source, settlement, and attach parity.

use seekdeep_cordis::Context;
use seekdeep_cordis_host_runner::{
    DynamicCordisCode, DynamicCordisDefineReceipt, DynamicCordisDefineRequest,
    DynamicCordisPluginSelector, DynamicCordisResolveAck, DynamicCordisRunFailureReason,
    DynamicCordisRunMode, DynamicCordisRunResolution, DynamicCordisRunResponse,
    DynamicCordisRunner,
};
use seekdeep_llm::SessionId;

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

fn successful_run_id(
    response: DynamicCordisRunResponse,
) -> seekdeep_cordis_host_runner::CordisDynamicPluginRunId {
    match response {
        DynamicCordisRunResponse::Success { plugin_run_id, .. } => plugin_run_id,
        failure @ DynamicCordisRunResponse::Failure { .. } => {
            panic!("unexpected request failure: {failure:?}")
        }
    }
}

#[tokio::test]
async fn approved_request_preserves_run_identity_through_host_client_and_late_answers() {
    let context = Context::new();
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
    let requested_run = successful_run_id(requested);
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
            true,
        )
        .await;
    assert!(matches!(
        host,
        seekdeep_cordis_host_runner::DynamicCordisHostHalfResult::Success {
            ref plugin_run_id,
            started_here: true,
            ..
        } if plugin_run_id == &requested_run
    ));
    let source = runner
        .get_client_code(&session, &defined.plugin_id, &requested_run)
        .unwrap();
    assert_eq!(source.code, "return { apply() {} };");
    let resolution = DynamicCordisRunResolution::Success {
        plugin_run_id: requested_run.clone(),
        waiting_for: None,
    };
    assert_eq!(
        runner.resolve_request_run(&request_id, &resolution).await,
        DynamicCordisResolveAck { accepted: true }
    );
    assert_eq!(
        runner.resolve_request_run(&request_id, &resolution).await,
        DynamicCordisResolveAck { accepted: false }
    );
    let row = &runner.inventory()[0];
    assert_eq!(row.current_package_id.as_ref(), Some(&defined.package_id));
    assert_eq!(
        row.active_run.as_ref().unwrap().plugin_run_id,
        requested_run
    );

    let attached = runner
        .run_host_half(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    assert!(matches!(
        attached,
        seekdeep_cordis_host_runner::DynamicCordisHostHalfResult::Success {
            started_here: false,
            ..
        }
    ));
    let failed_attach = runner
        .settle_user_run(
            &session,
            &defined.plugin_id,
            &DynamicCordisRunResolution::Failure {
                reason: DynamicCordisRunFailureReason::ClientHalfFailed,
                plugin_run_id: Some(requested_run.clone()),
                started_here: Some(false),
                message: Some("this page cannot load it".to_owned()),
                stack: None,
            },
        )
        .await;
    assert!(matches!(
        failed_attach,
        DynamicCordisRunResponse::Failure { .. }
    ));
    assert_eq!(
        runner.inventory()[0]
            .active_run
            .as_ref()
            .unwrap()
            .plugin_run_id,
        requested_run
    );
    runner.stop(&session, &defined.plugin_id).await;
    context.fiber().dispose().await.unwrap();
}
