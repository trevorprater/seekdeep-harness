//! Client plugin descriptor and Host invocation teaching parity.

use seekdeep_cordis_client_runner::*;
use seekdeep_cordis_dynamic_types::{
    CordisDynamicPluginId, CordisErrorDetails, DynamicCordisInvokeErrorCode,
    DynamicCordisInvokeResult,
};
use serde_json::json;

#[test]
fn plugin_descriptor_requires_the_exact_loader_slot_and_remote_services() {
    let host = seekdeep_cordis_client_runner::host_plugin();
    assert_eq!(host.name(), "cordis-client-runner");
    assert!(host.inject().is_empty());
    assert_eq!(CLIENT_RUNNER_NAME, "cordis-client-runner");
    assert_eq!(
        CLIENT_RUNNER_INJECT,
        [
            "loader",
            "modules",
            "slots",
            "remote",
            "remote.dynamicCordisRunner",
        ]
    );
}

#[test]
fn invocation_codes_have_distinct_actionable_messages_and_preserve_host_stack() {
    let plugin = CordisDynamicPluginId::new("panel-1");
    assert_eq!(
        unwrap_host_invoke(
            &plugin,
            "ping",
            DynamicCordisInvokeResult::Success {
                value: json!({"ok": 1})
            },
        )
        .unwrap(),
        json!({"ok": 1})
    );
    for (code, expected) in [
        (
            DynamicCordisInvokeErrorCode::PluginNotRunning,
            "stopped or was removed",
        ),
        (
            DynamicCordisInvokeErrorCode::StaleRun,
            "already been replaced",
        ),
        (
            DynamicCordisInvokeErrorCode::MethodNotFound,
            "harness.handle(\"ping\", fn)",
        ),
        (
            DynamicCordisInvokeErrorCode::HandlerError,
            "failed inside the host handler",
        ),
    ] {
        let error = unwrap_host_invoke(
            &plugin,
            "ping",
            DynamicCordisInvokeResult::Failure {
                code,
                error: CordisErrorDetails {
                    message: "host broke".to_owned(),
                    stack: Some("host stack".to_owned()),
                },
            },
        )
        .unwrap_err();
        assert!(error.message.contains(expected), "{}", error.message);
        assert_eq!(error.host_stack.as_deref(), Some("host stack"));
    }
}

#[test]
fn wire_failure_names_the_call_and_both_json_directions() {
    let text = host_wire_failure(
        &CordisDynamicPluginId::new("panel-1"),
        "ping",
        "arguments must be JSON",
    );
    assert!(text.contains("host.call(\"ping\") on panel-1 did not complete"));
    assert!(text.contains("Both directions carry JSON only"));
    assert!(text.contains("omit it, and the handler receives null"));
    assert!(text.contains("`return null`"));
}
