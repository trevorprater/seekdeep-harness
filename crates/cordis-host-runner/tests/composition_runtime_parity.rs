//! Cross-package provide/inject and stop-to-quiescence parity.

use std::time::Duration;

use seekdeep_cordis::{Context, ServiceKey};
use seekdeep_cordis_host_runner::{
    DynamicCordisCode, DynamicCordisDefineRequest, DynamicCordisPluginSelector,
    DynamicCordisRunMode, DynamicCordisRunResponse, DynamicCordisRunner, DynamicCordisStopResponse,
};
use seekdeep_llm::SessionId;
use serde_json::{Value, json};

const SHARED: ServiceKey<Value> = ServiceKey::new("shared");
const RESULT: ServiceKey<Value> = ServiceKey::new("result");
const PRIMITIVES: ServiceKey<Value> = ServiceKey::new("primitives");

fn request(session: &SessionId, prefix: &str, host: &str) -> DynamicCordisDefineRequest {
    DynamicCordisDefineRequest {
        session_id: session.clone(),
        plugin: DynamicCordisPluginSelector::New {
            id_prefix: prefix.to_owned(),
        },
        name: prefix.to_owned(),
        purpose: "composition test".to_owned(),
        code: DynamicCordisCode {
            host: Some(host.to_owned()),
            client: None,
        },
    }
}

async fn eventually(mut predicate: impl FnMut() -> bool, message: &str) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !predicate() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{message}"));
}

#[tokio::test]
async fn consumer_parks_then_tracks_provider_stop_and_restart() {
    let context = Context::new();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let consumer = runner
        .define(request(
            &session,
            "read",
            "return { inject: ['shared'], apply(ctx) { ctx.provide('result', ctx.shared.value); } };",
        ))
        .unwrap();
    let provider = runner
        .define(request(
            &session,
            "clock",
            "return { apply(ctx) { ctx.provide('shared', { value: 7 }); } };",
        ))
        .unwrap();

    let waiting = runner
        .run_host_only(
            &session,
            &consumer.plugin_id,
            &consumer.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    assert!(matches!(
        waiting,
        DynamicCordisRunResponse::Success { waiting_for, .. }
            if waiting_for == ["shared"]
    ));
    assert!(context.get(RESULT).is_none());

    assert!(matches!(
        runner
            .run_host_only(
                &session,
                &provider.plugin_id,
                &provider.package_id,
                DynamicCordisRunMode::Run,
            )
            .await,
        DynamicCordisRunResponse::Success { .. }
    ));
    eventually(
        || context.get(RESULT).is_some(),
        "consumer did not activate",
    )
    .await;
    assert_eq!(context.get(RESULT).as_deref(), Some(&serde_json::json!(7)));

    assert_eq!(
        runner.stop(&session, &provider.plugin_id).await,
        DynamicCordisStopResponse::Success
    );
    eventually(|| context.get(RESULT).is_none(), "consumer did not park").await;
    assert!(context.get(SHARED).is_none());

    assert!(matches!(
        runner
            .run_host_only(
                &session,
                &provider.plugin_id,
                &provider.package_id,
                DynamicCordisRunMode::Run,
            )
            .await,
        DynamicCordisRunResponse::Success { .. }
    ));
    eventually(
        || context.get(RESULT).is_some(),
        "consumer did not reactivate",
    )
    .await;
    assert_eq!(
        runner.stop(&session, &consumer.plugin_id).await,
        DynamicCordisStopResponse::Success
    );
    assert!(context.get(RESULT).is_none());
    assert!(context.get(SHARED).is_some());
    assert_eq!(
        runner.stop(&session, &provider.plugin_id).await,
        DynamicCordisStopResponse::Success
    );
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn duplicate_provide_fails_without_leaving_second_package_running() {
    let context = Context::new();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let first = runner
        .define(request(
            &session,
            "first",
            "return { apply(ctx) { ctx.provide('shared', 1); } };",
        ))
        .unwrap();
    let second = runner
        .define(request(
            &session,
            "secon",
            "return { apply(ctx) { ctx.provide('shared', 2); } };",
        ))
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
    let failed = runner
        .run_host_only(
            &session,
            &second.plugin_id,
            &second.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    assert!(
        matches!(
            &failed,
            DynamicCordisRunResponse::Failure { message, .. }
                if message.contains("already registered")
        ),
        "{failed:?}"
    );
    assert!(
        runner
            .registry()
            .get(&second.plugin_id)
            .unwrap()
            .lock()
            .run
            .is_none()
    );
    assert_eq!(context.get(SHARED).as_deref(), Some(&serde_json::json!(1)));
    runner.stop(&session, &first.plugin_id).await;
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn primitive_and_null_services_pass_unchanged_through_property_and_optional_reads() {
    let context = Context::new();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let provider = runner
        .define(request(
            &session,
            "value",
            "return { apply(ctx) { ctx.provide('flag', true); ctx.provide('nothing', null); } };",
        ))
        .unwrap();
    let consumer = runner
        .define(request(
            &session,
            "reads",
            concat!(
                "return { inject: ['flag', 'nothing'], apply(ctx) {",
                "ctx.provide('primitives', { directFlag: ctx.flag, optionalFlag: ctx.get('flag'),",
                "directNull: ctx.nothing, optionalNull: ctx.get('nothing') });",
                "} };",
            ),
        ))
        .unwrap();
    assert!(matches!(
        runner
            .run_host_only(
                &session,
                &provider.plugin_id,
                &provider.package_id,
                DynamicCordisRunMode::Run,
            )
            .await,
        DynamicCordisRunResponse::Success { .. }
    ));
    assert!(matches!(
        runner
            .run_host_only(
                &session,
                &consumer.plugin_id,
                &consumer.package_id,
                DynamicCordisRunMode::Run,
            )
            .await,
        DynamicCordisRunResponse::Success { .. }
    ));
    assert_eq!(
        context.get(PRIMITIVES).as_deref(),
        Some(&json!({
            "directFlag": true,
            "optionalFlag": true,
            "directNull": null,
            "optionalNull": null,
        }))
    );
    runner.stop(&session, &consumer.plugin_id).await;
    assert!(context.has_named("flag"));
    assert!(context.has_named("nothing"));
    runner.stop(&session, &provider.plugin_id).await;
    context.fiber().dispose().await.unwrap();
}
