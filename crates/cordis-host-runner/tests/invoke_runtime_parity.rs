//! Exact-generation Host handler registration, invocation, failure, and teardown parity.

use seekdeep_cordis::Context;
use seekdeep_cordis_host_runner::{
    CordisDynamicPluginRunId, DynamicCordisCode, DynamicCordisDefineRequest,
    DynamicCordisInvokeErrorCode, DynamicCordisInvokeResult, DynamicCordisPluginSelector,
    DynamicCordisRunMode, DynamicCordisRunResponse, DynamicCordisRunner,
};
use seekdeep_llm::SessionId;
use serde_json::json;

fn run_id(response: DynamicCordisRunResponse) -> CordisDynamicPluginRunId {
    match response {
        DynamicCordisRunResponse::Success { plugin_run_id, .. } => plugin_run_id,
        failure @ DynamicCordisRunResponse::Failure { .. } => {
            panic!("expected successful run, got {failure:?}")
        }
    }
}

#[tokio::test]
async fn handler_calls_are_json_only_and_bound_to_the_exact_active_run() {
    let context = Context::new();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let defined = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "math".to_owned(),
            },
            name: "math".to_owned(),
            purpose: "double numbers".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    "harness.handle('double', async args => ({ value: args.value * 2 }));\nreturn { apply() {} };"
                        .to_owned(),
                ),
                client: None,
            },
        })
        .unwrap();
    let first = run_id(
        runner
            .run(
                &session,
                &defined.plugin_id,
                &defined.package_id,
                DynamicCordisRunMode::Run,
            )
            .await,
    );
    assert_eq!(
        runner
            .invoke(&defined.plugin_id, &first, "double", json!({"value": 21}))
            .await,
        DynamicCordisInvokeResult::Success {
            value: json!({"value": 42})
        }
    );
    assert!(matches!(
        runner
            .invoke(&defined.plugin_id, &first, "missing", json!(null))
            .await,
        DynamicCordisInvokeResult::Failure {
            code: DynamicCordisInvokeErrorCode::MethodNotFound,
            ..
        }
    ));

    let second = run_id(
        runner
            .run(
                &session,
                &defined.plugin_id,
                &defined.package_id,
                DynamicCordisRunMode::Run,
            )
            .await,
    );
    assert_ne!(first, second);
    assert!(matches!(
        runner
            .invoke(&defined.plugin_id, &first, "double", json!({"value": 1}))
            .await,
        DynamicCordisInvokeResult::Failure {
            code: DynamicCordisInvokeErrorCode::StaleRun,
            ..
        }
    ));
    runner.stop(&session, &defined.plugin_id).await;
    assert!(matches!(
        runner
            .invoke(&defined.plugin_id, &second, "double", json!({"value": 1}))
            .await,
        DynamicCordisInvokeResult::Failure {
            code: DynamicCordisInvokeErrorCode::PluginNotRunning,
            ..
        }
    ));
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn handler_exceptions_and_non_json_results_are_contained_as_handler_errors() {
    let context = Context::new();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let defined = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "guard".to_owned(),
            },
            name: "guard".to_owned(),
            purpose: "guard handler results".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    concat!(
                        "harness.handle('throws', async () => { throw new Error('boom') });\n",
                        "harness.handle('undefined', async () => undefined);\n",
                        "harness.handle('instance', async () => new Date());\n",
                        "return { apply() {} };",
                    )
                    .to_owned(),
                ),
                client: None,
            },
        })
        .unwrap();
    let run = run_id(
        runner
            .run(
                &session,
                &defined.plugin_id,
                &defined.package_id,
                DynamicCordisRunMode::Run,
            )
            .await,
    );

    for (method, expected) in [
        ("throws", "boom"),
        ("undefined", "must be lossless JSON data"),
        ("instance", "not a class instance"),
    ] {
        let result = runner
            .invoke(&defined.plugin_id, &run, method, json!(null))
            .await;
        assert!(
            matches!(
                &result,
                DynamicCordisInvokeResult::Failure {
                    code: DynamicCordisInvokeErrorCode::HandlerError,
                    error,
                } if error.message.contains(expected)
            ),
            "{method}: {result:?}"
        );
    }
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn disposed_and_invalid_handler_registrations_never_escape_a_failed_host_load() {
    let context = Context::new();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let disposed = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "clean".to_owned(),
            },
            name: "clean".to_owned(),
            purpose: "dispose handler".to_owned(),
            code: DynamicCordisCode {
                host: Some(
                    "const dispose = harness.handle('gone', async () => 1); dispose(); return { apply() {} };"
                        .to_owned(),
                ),
                client: None,
            },
        })
        .unwrap();
    let run = run_id(
        runner
            .run(
                &session,
                &disposed.plugin_id,
                &disposed.package_id,
                DynamicCordisRunMode::Run,
            )
            .await,
    );
    assert!(matches!(
        runner
            .invoke(&disposed.plugin_id, &run, "gone", json!(null))
            .await,
        DynamicCordisInvokeResult::Failure {
            code: DynamicCordisInvokeErrorCode::MethodNotFound,
            ..
        }
    ));

    let invalid = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::New {
                id_prefix: "badfn".to_owned(),
            },
            name: "bad handler".to_owned(),
            purpose: "fail load".to_owned(),
            code: DynamicCordisCode {
                host: Some("harness.handle('', () => 1); return { apply() {} };".to_owned()),
                client: None,
            },
        })
        .unwrap();
    let failed = runner
        .run(
            &session,
            &invalid.plugin_id,
            &invalid.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    assert!(matches!(
        failed,
        DynamicCordisRunResponse::Failure { ref message, .. }
            if message.contains("needs a non-empty string method name")
    ));
    assert!(runner.inventory()[1].active_run.is_none());
    context.fiber().dispose().await.unwrap();
}
