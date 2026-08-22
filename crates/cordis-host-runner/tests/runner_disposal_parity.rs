//! Runner-tree disposal unwinds every active Host contribution.

use seekdeep_cordis::{Context, ServiceKey};
use seekdeep_cordis_host_runner::{
    DynamicCordisCode, DynamicCordisDefineRequest, DynamicCordisPluginSelector,
    DynamicCordisRunMode, DynamicCordisRunResponse, DynamicCordisRunner,
};
use seekdeep_llm::SessionId;
use serde_json::Value;

const LIVE: ServiceKey<Value> = ServiceKey::new("runnerLive");

#[tokio::test]
async fn disposing_the_runner_context_unwinds_a_still_running_host_half() {
    let context = Context::new();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let defined = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "alive".to_owned(),
            },
            name: "alive".to_owned(),
            purpose: "prove disposal".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    "return { apply(ctx) { ctx.provide('runnerLive', true); } };".to_owned(),
                ),
                client: None,
            },
        })
        .unwrap();
    assert!(matches!(
        runner
            .run(
                &session,
                &defined.plugin_id,
                &defined.package_id,
                DynamicCordisRunMode::Run,
            )
            .await,
        DynamicCordisRunResponse::Success { .. }
    ));
    assert!(context.get(LIVE).is_some());

    context.fiber().dispose().await.unwrap();
    assert!(context.get(LIVE).is_none());
}
