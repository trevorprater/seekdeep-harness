//! Concrete two-level parsing installed over Connection's physical carrier.

use seekdeep_client_connection::{RpcResult, WebApiContract, WebApiDownlink};
use seekdeep_host_apiproxy::ApiProxyContract;
use serde_json::{Map, json};

#[test]
fn first_level_response_normalizes_closed_errors_before_observation() {
    let contract = ApiProxyContract;
    let response = contract
        .parse_server_response(&json!({
            "type": "server-response",
            "rpcId": "r1",
            "result": {
                "ok": false,
                "error": {
                    "code": "cancelled",
                    "message": "stopped",
                    "details": { "ignored": true },
                    "ignored": true
                },
                "ignored": true
            },
            "ignored": true
        }))
        .unwrap();
    assert_eq!(response.rpc_id.as_str(), "r1");
    assert_eq!(
        response.result,
        RpcResult::Failure {
            error: seekdeep_host_apiproxy::RpcError {
                code: "cancelled".to_owned(),
                message: "stopped".to_owned(),
                details: Map::new(),
            }
        }
    );
    assert!(
        contract
            .parse_server_response(&json!({
                "type": "server-response",
                "rpcId": "r2",
                "result": {
                    "ok": false,
                    "error": { "code": "future", "message": "x", "details": {} }
                }
            }))
            .is_err()
    );
}

#[test]
fn method_success_parsing_requires_a_value_and_applies_the_exact_schema() {
    let contract = ApiProxyContract;
    let normalized = contract
        .parse_unary_success_value(
            "host.describe",
            Some(&json!({
                "version": "1",
                "cwd": "/tmp",
                "attachedSessions": 0,
                "canOpenPath": true,
                "ignored": true
            })),
        )
        .unwrap();
    assert_eq!(
        normalized,
        Some(json!({
            "version": "1",
            "cwd": "/tmp",
            "attachedSessions": 0,
            "canOpenPath": true
        }))
    );
    assert!(
        contract
            .parse_unary_success_value("host.describe", None)
            .is_err()
    );
    assert!(
        contract
            .parse_unary_success_value("future.method", Some(&json!({})))
            .is_err()
    );
}

#[test]
fn mux_and_host_streams_use_distinct_closed_frame_unions() {
    let contract = ApiProxyContract;
    let mux = contract
        .parse_downlink_payload(
            WebApiDownlink::Mux,
            &json!({
                "type": "session/subscribed",
                "sessionId": "s1",
                "lastSeq": 4,
                "ignored": true
            }),
        )
        .unwrap();
    assert_eq!(
        mux,
        json!({ "type": "session/subscribed", "sessionId": "s1", "lastSeq": 4 })
    );

    let host = contract
        .parse_downlink_payload(
            WebApiDownlink::Host,
            &json!({
                "type": "host/remote-event",
                "event": "commands/change",
                "args": [],
                "ignored": true
            }),
        )
        .unwrap();
    assert_eq!(
        host,
        json!({ "type": "host/remote-event", "event": "commands/change", "args": [] })
    );
    assert!(
        contract
            .parse_downlink_payload(WebApiDownlink::Host, &mux)
            .is_err()
    );
    assert!(
        contract
            .parse_downlink_payload(WebApiDownlink::Mux, &host)
            .is_err()
    );
}
