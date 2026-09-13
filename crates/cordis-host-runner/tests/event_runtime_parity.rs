//! Sandboxed event callbacks, once behavior, and stop-to-quiescence parity.

use seekdeep_cordis::{Context, EventArgs};
use seekdeep_cordis_host_runner::{
    DynamicCordisCode, DynamicCordisDefineRequest, DynamicCordisPluginSelector,
    DynamicCordisRunMode, DynamicCordisRunResponse, DynamicCordisRunner,
};
use seekdeep_llm::SessionId;
use serde_json::json;

#[tokio::test]
async fn event_callbacks_stay_in_the_worker_and_stop_removes_them_before_returning() {
    let context = Context::new();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let defined = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "event".to_owned(),
            },
            name: "event counter".to_owned(),
            purpose: "observe events".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    concat!(
                        "let count = 0;",
                        "harness.handle('count', async () => count);",
                        "return { apply(ctx) {",
                        "ctx.on('probe', () => { count += 1; });",
                        "ctx.once('once', () => { count += 10; });",
                        "const dispose = ctx.on('disposed', () => { count += 100; }); dispose();",
                        "} };",
                    )
                    .to_owned(),
                ),
                client: None,
            },
        })
        .unwrap();
    let started = runner
        .run(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    let run_id = match started {
        DynamicCordisRunResponse::Success { plugin_run_id, .. } => plugin_run_id,
        failed @ DynamicCordisRunResponse::Failure { .. } => {
            panic!("unexpected run failure: {failed:?}")
        }
    };
    let runtime = runner
        .registry()
        .get(&defined.plugin_id)
        .unwrap()
        .lock()
        .run
        .as_ref()
        .unwrap()
        .host_runtime
        .clone()
        .unwrap();

    context
        .events()
        .parallel(&context, "probe", &EventArgs::new())
        .await
        .unwrap();
    context
        .events()
        .parallel(&context, "once", &EventArgs::new())
        .await
        .unwrap();
    context
        .events()
        .parallel(&context, "once", &EventArgs::new())
        .await
        .unwrap();
    context
        .events()
        .parallel(&context, "disposed", &EventArgs::new())
        .await
        .unwrap();
    assert_eq!(
        runner
            .invoke(&defined.plugin_id, &run_id, "count", json!(null))
            .await,
        seekdeep_cordis_host_runner::DynamicCordisInvokeResult::Success { value: json!(11) }
    );

    runner.stop(&session, &defined.plugin_id).await;
    context
        .events()
        .parallel(&context, "probe", &EventArgs::new())
        .await
        .unwrap();
    assert_eq!(
        runtime.invoke("count", json!(null)).await.unwrap(),
        json!(11)
    );
    context.fiber().dispose().await.unwrap();
}
