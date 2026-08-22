//! Dynamic Cordis Host/Client wire-shape parity.

use seekdeep_cordis_host_runner::*;
use seekdeep_llm::SessionId;
use serde_json::json;

fn plugin() -> CordisDynamicPluginId {
    CordisDynamicPluginId::new("clock-1")
}

fn package() -> CordisDynamicPackageId {
    CordisDynamicPackageId::new("pkg-2")
}

fn run() -> CordisDynamicPluginRunId {
    CordisDynamicPluginRunId::new("run-3")
}

#[test]
fn run_success_and_failure_preserve_boolean_discriminants_and_optional_fields() {
    let success = DynamicCordisRunResponse::Success {
        status: DynamicCordisRunSuccessStatus::Running,
        plugin_id: plugin(),
        package_id: package(),
        plugin_run_id: run(),
        waiting_for: vec!["timer".to_owned()],
        client_waiting_for: None,
        current_package_id: Some(CordisDynamicPackageId::new("pkg-1")),
        next_package_id: None,
        mode: DynamicCordisRunMode::Update,
    };
    assert_eq!(
        serde_json::to_value(success).unwrap(),
        json!({
            "ok": true,
            "status": "running",
            "pluginId": "clock-1",
            "packageId": "pkg-2",
            "pluginRunId": "run-3",
            "waitingFor": ["timer"],
            "currentPackageId": "pkg-1",
            "mode": "update",
        })
    );
    assert_eq!(
        serde_json::to_value(DynamicCordisRunResponse::Failure {
            reason: DynamicCordisRunFailureReason::HostHalfFailed,
            message: "broken".to_owned(),
            stack: None,
        })
        .unwrap(),
        json!({"ok": false, "reason": "host-half-failed", "message": "broken"})
    );
}

#[test]
fn approval_inspect_stop_undefine_and_invoke_results_keep_exact_shapes() {
    assert_eq!(
        serde_json::to_value(CordisInspectQueryResolution::Failure {
            reason: CordisInspectFailureReason::ProviderMissing,
            message: "gone".to_owned(),
        })
        .unwrap(),
        json!({"ok": false, "reason": "provider-missing", "message": "gone"})
    );
    assert_eq!(
        serde_json::to_value(DynamicCordisUndefineReceipt::Success { was_running: true }).unwrap(),
        json!({"ok": true, "wasRunning": true})
    );
    assert_eq!(
        serde_json::to_value(DynamicCordisStopResponse::Failure {
            reason: DynamicCordisStopFailureReason::NotRunning,
            message: "stopped".to_owned(),
        })
        .unwrap(),
        json!({"ok": false, "reason": "not-running", "message": "stopped"})
    );
    assert_eq!(
        serde_json::to_value(DynamicCordisInvokeResult::Failure {
            code: DynamicCordisInvokeErrorCode::StaleRun,
            error: CordisErrorDetails {
                message: "stale".to_owned(),
                stack: Some("stack".to_owned()),
            },
        })
        .unwrap(),
        json!({"ok": false, "code": "stale-run", "message": "stale", "stack": "stack"})
    );
}

#[test]
fn inventory_and_client_events_use_camel_case_and_reject_unknown_closed_enums() {
    let row = DynamicCordisInventoryRow {
        plugin_id: plugin(),
        agent_id: SessionId::new("session-a"),
        packages: vec![DynamicCordisInventoryPackage {
            package_id: package(),
            name: "clock".to_owned(),
            purpose: "show time".to_owned(),
            has_host_half: true,
            has_client_half: false,
        }],
        current_package_id: Some(package()),
        next_package_id: None,
        active_run: Some(DynamicCordisActiveRun {
            plugin_run_id: run(),
            package_id: package(),
        }),
        latest_run: None,
    };
    assert_eq!(
        serde_json::to_value(row).unwrap(),
        json!({
            "pluginId": "clock-1",
            "agentId": "session-a",
            "packages": [{
                "packageId": "pkg-2",
                "name": "clock",
                "purpose": "show time",
                "hasHostHalf": true,
                "hasClientHalf": false,
            }],
            "currentPackageId": "pkg-2",
            "activeRun": {"pluginRunId": "run-3", "packageId": "pkg-2"},
        })
    );
    assert!(serde_json::from_value::<CordisRunStatus>(json!("future")).is_err());
    assert!(serde_json::from_value::<DynamicCordisInvokeErrorCode>(json!("future")).is_err());
}
