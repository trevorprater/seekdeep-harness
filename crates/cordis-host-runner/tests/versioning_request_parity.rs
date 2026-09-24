//! Published approval ownership and provisional Host retraction parity.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_cordis_host_runner::{
    CordisDynamicPluginRunId, DynamicCordisCode, DynamicCordisDefineRequest,
    DynamicCordisPluginSelector, DynamicCordisRequestResolved, DynamicCordisRetracted,
    DynamicCordisRunFailureReason, DynamicCordisRunMode, DynamicCordisRunResolution,
    DynamicCordisRunner, RequestRunOutcome,
};
use seekdeep_llm::{AbortSignal, SessionId};

struct ObservedEvents {
    retracted: Arc<Mutex<Vec<CordisDynamicPluginRunId>>>,
    resolved: Arc<Mutex<Vec<DynamicCordisRequestResolved>>>,
}

fn observe(context: &Context) -> ObservedEvents {
    let retracted = Arc::new(Mutex::new(Vec::new()));
    let observed = retracted.clone();
    context
        .events()
        .on_sync(
            context,
            "cordis/dynamic-retract",
            move |_, args| {
                observed.lock().push(
                    args.get::<DynamicCordisRetracted>(0)
                        .unwrap()
                        .plugin_run_id
                        .clone(),
                );
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )
        .unwrap();
    let resolved = Arc::new(Mutex::new(Vec::new()));
    let observed = resolved.clone();
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
    ObservedEvents {
        retracted,
        resolved,
    }
}

fn define(
    runner: &DynamicCordisRunner,
    session: &SessionId,
) -> seekdeep_cordis_host_runner::DynamicCordisDefineReceipt {
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

#[tokio::test]
async fn caller_abort_after_publication_keeps_provisional_host_until_stop_retracts_and_cancels() {
    let context = Context::new();
    let observed = observe(&context);
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let defined = define(&runner, &session);
    let signal = AbortSignal::default();
    runner
        .run_with_signal(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
            Some(&signal),
        )
        .await;
    let request_id = runner
        .registry()
        .pending_request_for(&defined.plugin_id)
        .unwrap();
    let started = runner
        .run_host_half_for_request(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
            &request_id,
            false,
        )
        .await;
    let run_id = match started {
        seekdeep_cordis_host_runner::DynamicCordisHostHalfResult::Success {
            plugin_run_id, ..
        } => plugin_run_id,
        failure @ seekdeep_cordis_host_runner::DynamicCordisHostHalfResult::Failure(_) => {
            panic!("unexpected Host failure: {failure:?}")
        }
    };

    signal.abort();
    assert_eq!(
        runner.inventory()[0]
            .active_run
            .as_ref()
            .unwrap()
            .plugin_run_id,
        run_id
    );
    runner.stop(&session, &defined.plugin_id).await;
    assert!(runner.inventory()[0].active_run.is_none());
    assert_eq!(
        observed.retracted.lock().as_slice(),
        std::slice::from_ref(&run_id)
    );
    assert_eq!(
        observed.resolved.lock()[0].outcome,
        RequestRunOutcome::Cancelled
    );
    assert!(
        !runner
            .resolve_request_run(
                &request_id,
                &DynamicCordisRunResolution::Failure {
                    reason: DynamicCordisRunFailureReason::Rejected,
                    plugin_run_id: None,
                    started_here: None,
                    message: None,
                    stack: None,
                },
            )
            .await
            .accepted
    );
    context.fiber().dispose().await.unwrap();
}
