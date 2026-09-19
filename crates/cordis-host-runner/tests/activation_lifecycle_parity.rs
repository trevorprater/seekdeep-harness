//! Model-run replacement, direct attachment, mode validation, and lifecycle announcements.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_cordis_host_runner::{
    CordisDynamicPackageId, DynamicCordisCode, DynamicCordisDefineReceipt,
    DynamicCordisDefineRequest, DynamicCordisHostHalfResult, DynamicCordisPackage,
    DynamicCordisPluginSelector, DynamicCordisRetracted, DynamicCordisRunFailureReason,
    DynamicCordisRunMode, DynamicCordisRunResponse, DynamicCordisRunner,
};
use seekdeep_llm::SessionId;

fn define(
    runner: &DynamicCordisRunner,
    session: &SessionId,
    existing: Option<&DynamicCordisDefineReceipt>,
    name: &str,
) -> DynamicCordisDefineReceipt {
    runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: existing.map_or_else(
                || DynamicCordisPluginSelector::New {
                    id_prefix: "clock".to_owned(),
                },
                |receipt| DynamicCordisPluginSelector::Existing {
                    plugin_id: receipt.plugin_id.clone(),
                },
            ),
            name: name.to_owned(),
            purpose: "show time".to_owned(),
            code: DynamicCordisCode {
                host: Some("return { apply() {} };".to_owned()),
                client: None,
            },
        })
        .unwrap()
}

fn failure(response: DynamicCordisRunResponse) -> (DynamicCordisRunFailureReason, String) {
    match response {
        DynamicCordisRunResponse::Failure {
            reason, message, ..
        } => (reason, message),
        success @ DynamicCordisRunResponse::Success { .. } => {
            panic!("expected failure, got {success:?}")
        }
    }
}

#[tokio::test]
async fn model_run_replaces_but_direct_page_run_attaches_and_announces_exact_generations() {
    let context = Context::new();
    let lifecycle = Arc::new(Mutex::new(Vec::<String>::new()));
    let packages = lifecycle.clone();
    context
        .events()
        .on_sync(
            &context,
            "cordis/dynamic-package",
            move |_, args| {
                let event = args.get::<DynamicCordisPackage>(0).unwrap();
                packages.lock().push(format!(
                    "package:{}/{}/{}",
                    event.plugin_id, event.package_id, event.plugin_run_id
                ));
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let retractions = lifecycle.clone();
    context
        .events()
        .on_sync(
            &context,
            "cordis/dynamic-retract",
            move |_, args| {
                let event = args.get::<DynamicCordisRetracted>(0).unwrap();
                retractions.lock().push(format!(
                    "retract:{}/{}/{}",
                    event.plugin_id, event.package_id, event.plugin_run_id
                ));
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let defined = define(&runner, &session, None, "clock");

    let first = runner
        .run(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    let second = runner
        .run(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    let attached = runner
        .run_host_half(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;

    assert!(matches!(
        first,
        DynamicCordisRunResponse::Success { ref plugin_run_id, .. }
            if plugin_run_id.as_str() == "run-1"
    ));
    assert!(matches!(
        second,
        DynamicCordisRunResponse::Success { ref plugin_run_id, .. }
            if plugin_run_id.as_str() == "run-2"
    ));
    assert!(matches!(
        attached,
        DynamicCordisHostHalfResult::Success {
            ref plugin_run_id,
            started_here: false,
            ..
        } if plugin_run_id.as_str() == "run-2"
    ));
    assert_eq!(
        *lifecycle.lock(),
        [
            "package:clock-1/pkg-1/run-1",
            "retract:clock-1/pkg-1/run-1",
            "package:clock-1/pkg-1/run-2",
        ]
    );
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn mode_errors_match_the_source_and_do_not_mint_or_mutate_attempts() {
    let context = Context::new();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let first = define(&runner, &session, None, "clock v1");
    let second = define(&runner, &session, Some(&first), "clock v2");

    assert_eq!(
        failure(
            runner
                .run(
                    &session,
                    &first.plugin_id,
                    &second.package_id,
                    DynamicCordisRunMode::Update,
                )
                .await
        ),
        (
            DynamicCordisRunFailureReason::InvalidMode,
            format!(
                "plugin \"{}\" has no successful version yet; start \"{}\" with mode \"run\"",
                first.plugin_id, second.package_id
            )
        )
    );
    assert!(runner.inventory()[0].latest_run.is_none());

    let started = runner
        .run(
            &session,
            &first.plugin_id,
            &first.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    assert!(matches!(
        started,
        DynamicCordisRunResponse::Success { ref plugin_run_id, .. }
            if plugin_run_id.as_str() == "run-1"
    ));
    assert_eq!(
        failure(
            runner
                .run(
                    &session,
                    &first.plugin_id,
                    &first.package_id,
                    DynamicCordisRunMode::Update,
                )
                .await
        ),
        (
            DynamicCordisRunFailureReason::InvalidMode,
            format!(
                "package \"{}\" is already current; use mode \"run\"",
                first.package_id
            )
        )
    );
    assert_eq!(
        failure(
            runner
                .run(
                    &session,
                    &first.plugin_id,
                    &second.package_id,
                    DynamicCordisRunMode::Run,
                )
                .await
        ),
        (
            DynamicCordisRunFailureReason::InvalidMode,
            format!(
                "package \"{}\" differs from current \"{}\"; use mode \"update\"",
                second.package_id, first.package_id
            )
        )
    );
    assert!(
        runner
            .registry()
            .pending_request_for(&first.plugin_id)
            .is_none()
    );
    assert_eq!(
        runner.inventory()[0].next_package_id,
        None::<CordisDynamicPackageId>
    );
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn concurrent_pages_share_the_same_in_flight_host_activation_result() {
    let context = Context::new();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let defined = define(&runner, &session, None, "clock");

    let (first, second) = tokio::join!(
        runner.run_host_half(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
        ),
        runner.run_host_half(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
        ),
    );
    assert_eq!(first, second);
    assert!(matches!(
        first,
        DynamicCordisHostHalfResult::Success {
            ref plugin_run_id,
            started_here: true,
            ..
        } if plugin_run_id.as_str() == "run-1"
    ));
    context.fiber().dispose().await.unwrap();
}
