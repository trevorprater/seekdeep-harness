//! Dynamic definition, inventory, ownership, and inspection parity.

use seekdeep_cordis_host_runner::{
    ApprovalRequestId, CordisDynamicPluginId, DYNAMIC_CORDIS_RUNNER, DynamicCordisCode,
    DynamicCordisDefineRequest, DynamicCordisPluginSelector, DynamicCordisResolveAck,
    DynamicCordisRunFailureReason, DynamicCordisRunMode, DynamicCordisRunResolution,
    DynamicCordisRunResponse, DynamicCordisRunSuccessStatus, DynamicCordisRunner,
    DynamicCordisRunnerConfig, DynamicCordisStopResponse,
};
use seekdeep_llm::SessionId;

fn define(
    session: &SessionId,
    prefix: &str,
    host: Option<&str>,
    client: Option<&str>,
) -> DynamicCordisDefineRequest {
    DynamicCordisDefineRequest {
        session_id: session.clone(),
        plugin: DynamicCordisPluginSelector::New {
            id_prefix: prefix.to_owned(),
        },
        name: " clock ".to_owned(),
        purpose: " show time ".to_owned(),
        code: DynamicCordisCode {
            host: host.map(str::to_owned),
            client: client.map(str::to_owned),
        },
    }
}

#[test]
fn runner_config_defaults_rejects_unknown_fields_and_fails_before_zero_timeout_installation() {
    let plugin = seekdeep_cordis_host_runner::plugin();
    assert_eq!(plugin.name(), "cordis-host-runner");
    assert_eq!(plugin.inject(), ["tools"]);
    assert_eq!(
        serde_json::from_value::<DynamicCordisRunnerConfig>(serde_json::json!({})).unwrap(),
        DynamicCordisRunnerConfig::default()
    );
    assert!(
        serde_json::from_value::<DynamicCordisRunnerConfig>(serde_json::json!({"future": true}))
            .is_err()
    );
    let context = seekdeep_cordis::Context::new();
    assert!(
        DynamicCordisRunner::try_install(&context, DynamicCordisRunnerConfig { vm_timeout_ms: 0 },)
            .unwrap_err()
            .to_string()
            .contains("at least 1")
    );
    assert!(context.get(DYNAMIC_CORDIS_RUNNER).is_none());
}

#[test]
fn define_trims_metadata_keeps_source_and_mints_non_reused_versions() {
    let runner = DynamicCordisRunner::new();
    let session = SessionId::new("session-a");
    let first = runner
        .define(define(
            &session,
            "clock",
            Some("return { apply() {} }"),
            None,
        ))
        .unwrap();
    assert_eq!(first.plugin_id.as_str(), "clock-1");
    assert_eq!(first.package_id.as_str(), "pkg-1");
    assert_eq!(first.name, "clock");
    assert_eq!(first.purpose, "show time");
    assert!(first.has_host_half);
    assert!(!first.has_client_half);

    let second = runner
        .define(DynamicCordisDefineRequest {
            session_id: session.clone(),
            plugin: DynamicCordisPluginSelector::Existing {
                plugin_id: first.plugin_id.clone(),
            },
            name: "clock v2".to_owned(),
            purpose: "show seconds".to_owned(),
            code: DynamicCordisCode {
                host: None,
                client: Some("return { apply() {} }".to_owned()),
            },
        })
        .unwrap();
    assert_eq!(second.package_id.as_str(), "pkg-2");
    let package = runner
        .inspect_package(&session, &first.plugin_id, &second.package_id)
        .unwrap();
    assert_eq!(
        package.code.client.as_deref(),
        Some("return { apply() {} }")
    );
    assert_eq!(runner.inventory()[0].packages.len(), 2);
}

#[test]
fn invalid_definition_is_atomic_and_teaches_metadata_and_javascript_fixes() {
    let runner = DynamicCordisRunner::new();
    let session = SessionId::new("session-a");
    for (request, message) in [
        (
            define(&session, "AB", Some("return {}"), None),
            "3–6 lowercase",
        ),
        (
            DynamicCordisDefineRequest {
                session_id: session.clone(),
                plugin: DynamicCordisPluginSelector::New {
                    id_prefix: "clock".to_owned(),
                },
                name: " ".to_owned(),
                purpose: "purpose".to_owned(),
                code: DynamicCordisCode {
                    host: Some("return {}".to_owned()),
                    client: None,
                },
            },
            "non-empty `name`",
        ),
        (
            define(
                &session,
                "clock",
                Some("return { name: 'clock' as const, apply() {} }"),
                None,
            ),
            "plain JavaScript",
        ),
    ] {
        assert!(
            runner
                .define(request)
                .unwrap_err()
                .to_string()
                .contains(message)
        );
        assert!(runner.inventory().is_empty());
    }
    assert_eq!(
        runner
            .define(define(&session, "clock", Some("return {}"), None))
            .unwrap()
            .plugin_id
            .as_str(),
        "clock-1"
    );
}

#[test]
fn session_ownership_hides_plugins_and_source_free_views_preserve_order() {
    let runner = DynamicCordisRunner::new();
    let session_a = SessionId::new("session-a");
    let session_b = SessionId::new("session-b");
    let a = runner
        .define(define(&session_a, "clock", Some("return {}"), None))
        .unwrap();
    let b = runner
        .define(define(&session_b, "panel", None, Some("return {}")))
        .unwrap();
    assert!(runner.reference(&session_b, &a.plugin_id).is_none());
    assert!(runner.inspect_plugin(&session_b, &a.plugin_id).is_err());
    assert_eq!(runner.list_plugins(&session_a).len(), 1);
    assert_eq!(runner.list_plugins(&session_b).len(), 1);
    assert_eq!(
        runner
            .inventory()
            .iter()
            .map(|row| row.plugin_id.clone())
            .collect::<Vec<_>>(),
        [a.plugin_id, b.plugin_id]
    );
    assert!(
        runner
            .inspect_plugin(&session_a, &CordisDynamicPluginId::new("missing-9"))
            .unwrap_err()
            .to_string()
            .contains("lost on SeekDeep restart")
    );
}

#[tokio::test]
async fn client_request_is_answered_once_and_stop_cancels_a_pending_transition() {
    let context = seekdeep_cordis::Context::new();
    let runner = DynamicCordisRunner::install(&context, 5_000);
    let session = SessionId::new("session-a");
    let defined = runner
        .define(define(
            &session,
            "panel",
            None,
            Some("return { apply() {} }"),
        ))
        .unwrap();
    let response = runner
        .run(
            &session,
            &defined.plugin_id,
            &defined.package_id,
            DynamicCordisRunMode::Run,
        )
        .await;
    let request_id = runner
        .registry()
        .pending_request_for(&defined.plugin_id)
        .unwrap();
    assert!(matches!(
        response,
        DynamicCordisRunResponse::Success {
            status: DynamicCordisRunSuccessStatus::AwaitingApproval,
            ..
        }
    ));
    let rejected = DynamicCordisRunResolution::Failure {
        reason: DynamicCordisRunFailureReason::Rejected,
        plugin_run_id: None,
        started_here: None,
        message: None,
        stack: None,
    };
    assert_eq!(
        runner.resolve_request_run(&request_id, &rejected).await,
        DynamicCordisResolveAck { accepted: true }
    );
    assert_eq!(
        runner.resolve_request_run(&request_id, &rejected).await,
        DynamicCordisResolveAck { accepted: false }
    );

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
    assert_eq!(
        runner.stop(&session, &defined.plugin_id).await,
        DynamicCordisStopResponse::Success
    );
    assert!(
        runner
            .registry()
            .pending_request_for(&defined.plugin_id)
            .is_none()
    );
    assert_eq!(
        runner
            .resolve_request_run(&ApprovalRequestId::new("approval-999"), &rejected)
            .await,
        DynamicCordisResolveAck { accepted: false }
    );
    context.fiber().dispose().await.unwrap();
}
