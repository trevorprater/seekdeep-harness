//! Immutable version pointers, failed updates, rerun, and undefine parity.

use seekdeep_cordis::Context;
use seekdeep_cordis_host_runner::{
    DynamicCordisCode, DynamicCordisDefineRequest, DynamicCordisPluginSelector,
    DynamicCordisRunMode, DynamicCordisRunResponse, DynamicCordisRunner,
    DynamicCordisUndefineReceipt,
};
use seekdeep_llm::SessionId;

#[tokio::test]
async fn failed_update_keeps_current_selects_next_and_stops_physical_run() {
    let context = Context::new();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let first = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "clock".to_owned(),
            },
            name: "clock v1".to_owned(),
            purpose: "show time".to_owned(),
            code: DynamicCordisCode {
                host: Some("return { apply() {} };".to_owned()),
                client: None,
            },
        })
        .unwrap();
    assert!(matches!(
        runner
            .run_host_only(
                &session,
                &first.plugin_id,
                &first.package_id,
                DynamicCordisRunMode::Run,
            )
            .await,
        DynamicCordisRunResponse::Success { .. }
    ));
    let second = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::Existing {
                plugin_id: first.plugin_id.clone(),
            },
            name: "clock v2".to_owned(),
            purpose: "break".to_owned(),
            code: DynamicCordisCode {
                host: Some("throw new Error('broken update');".to_owned()),
                client: None,
            },
        })
        .unwrap();
    assert!(matches!(
        runner
            .run_host_only(
                &session,
                &first.plugin_id,
                &second.package_id,
                DynamicCordisRunMode::Update,
            )
            .await,
        DynamicCordisRunResponse::Failure { .. }
    ));
    let row = &runner.inventory()[0];
    assert_eq!(row.current_package_id.as_ref(), Some(&first.package_id));
    assert_eq!(row.next_package_id.as_ref(), Some(&second.package_id));
    assert!(row.active_run.is_none());

    assert!(matches!(
        runner
            .run_host_only(
                &session,
                &first.plugin_id,
                &first.package_id,
                DynamicCordisRunMode::Run,
            )
            .await,
        DynamicCordisRunResponse::Success { .. }
    ));
    let row = &runner.inventory()[0];
    assert_eq!(row.current_package_id.as_ref(), Some(&first.package_id));
    assert!(row.next_package_id.is_none());
    assert!(row.active_run.is_some());

    assert_eq!(
        runner.undefine(&session, &first.plugin_id).await,
        DynamicCordisUndefineReceipt::Success { was_running: true }
    );
    assert!(runner.inventory().is_empty());
    assert!(matches!(
        runner.undefine(&session, &first.plugin_id).await,
        DynamicCordisUndefineReceipt::PluginMissing { .. }
    ));
    context.fiber().dispose().await.unwrap();
}
