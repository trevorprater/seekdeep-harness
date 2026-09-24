//! Executable parity specifications for the API Proxy RPC wire schemas.

use seekdeep_host_apiproxy::{
    RpcMessage, RpcReceipt, RpcReceiptReason, parse_client_request, parse_client_response,
    parse_rpc_error, parse_rpc_message, parse_rpc_receipt, parse_rpc_result, parse_server_request,
    parse_server_response,
};
use serde_json::{Value, json};

#[test]
fn rpc_id_is_an_unvalidated_string_brand() {
    let id = seekdeep_host_apiproxy::api::rpc::parse_rpc_id(&json!("abc")).unwrap();
    assert_eq!(id.as_str(), "abc");
    assert_eq!(
        seekdeep_host_apiproxy::api::rpc::parse_rpc_id(&json!(""))
            .unwrap()
            .as_str(),
        ""
    );
    assert!(seekdeep_host_apiproxy::api::rpc::parse_rpc_id(&json!(42)).is_err());
}

#[test]
fn transport_error_folds_into_internal_failure() {
    let result = seekdeep_host_apiproxy::transport_error::<Value>(&"wire down");
    assert_eq!(
        serde_json::to_value(result).unwrap(),
        json!({
            "ok": false,
            "error": {"code": "internal", "message": "wire down", "details": {}}
        })
    );
}

#[test]
fn all_closed_rpc_error_branches_accept_their_exact_details() {
    let branches = [
        ("bad-request", json!({"issues": []})),
        ("cancelled", json!({})),
        ("session-not-found", json!({"sessionId": "s"})),
        ("model-unavailable", json!({"provider": "p", "model": "m"})),
        (
            "session-conflict",
            json!({"sessionId": "s", "requestedCwd": "/a", "existingCwd": "/b"}),
        ),
        ("invalid-time-zone", json!({"value": "CST"})),
        (
            "workspace-attach-failed",
            json!({"sessionId": "s", "workspaceId": "w"}),
        ),
        ("workspace-not-found", json!({"workspaceId": "w"})),
        ("workspace-invalid-path", json!({"path": "/x"})),
        ("workspace-name-conflict", json!({"name": "x"})),
        (
            "workspace-move-invalid",
            json!({"workspaceId": "w", "sessionId": "s", "beforeSessionId": "s2"}),
        ),
        ("directory-unreadable", json!({"path": "/x"})),
        ("directory-exists", json!({"path": "/x"})),
        ("directory-create-failed", json!({"path": "/x"})),
        (
            "directory-picker-unavailable",
            json!({"capability": "picker"}),
        ),
        (
            "agent-preset-read-only",
            json!({"agentPreset": "p", "reason": "system"}),
        ),
        (
            "agent-preset-locked",
            json!({"sessionId": "s", "agentPreset": "p"}),
        ),
        (
            "agent-preset-conflict",
            json!({"sessionId": "s", "requestedPreset": "p", "existingPreset": "q"}),
        ),
        (
            "agent-preset-not-found",
            json!({"agentPreset": "p", "available": ["q"]}),
        ),
        (
            "agent-preset-invalid",
            json!({"agentPreset": "p", "reason": "bad"}),
        ),
        ("agent-busy", json!({"reason": "running"})),
        ("attachment-error", json!({"reason": "large"})),
        ("queue-item-not-found", json!({"itemId": "i"})),
        ("steer-unavailable", json!({"itemId": "i"})),
        ("command-error", json!({})),
        ("unknown-command", json!({})),
        ("settings-rejected", json!({"ns": "p"})),
        ("settings-not-exposed", json!({"ns": "p"})),
        (
            "settings-conflict",
            json!({"ns": "p", "expected": 1, "actual": 2}),
        ),
        ("credential-rejected", json!({"ref": "r"})),
        (
            "model-discovery-failed",
            json!({"settingsNs": "p", "baseURL": "https://example.test"}),
        ),
        ("title-invalid", json!({"sessionId": "s"})),
        ("fork-unavailable", json!({"sessionId": "s"})),
        (
            "subagent-parent-unavailable",
            json!({"parentSessionId": "p"}),
        ),
        (
            "subagent-not-found",
            json!({"parentSessionId": "p", "childSessionId": "c"}),
        ),
        (
            "subagent-catalog-diagnostic",
            json!({"parentSessionId": "p", "childSessionId": "c", "reason": "corrupt"}),
        ),
        ("subagent-not-resumable", json!({"childSessionId": "c"})),
        ("subagent-unauthorized", json!({"childSessionId": "c"})),
        (
            "subagent-delivery-unavailable",
            json!({"childSessionId": "c"}),
        ),
        ("internal", json!({})),
    ];

    assert_eq!(branches.len(), 40);
    for (code, details) in branches {
        let parsed = parse_rpc_error(&json!({
            "code": code,
            "message": "m",
            "details": details,
            "ignored": true
        }))
        .unwrap_or_else(|error| panic!("{code}: {error}"));
        assert_eq!(parsed.code, code);
        assert_eq!(parsed.message, "m");
        assert!(!parsed.details.contains_key("ignored"));
    }
}

#[test]
fn rpc_error_schema_rejects_unknown_or_malformed_branches() {
    for invalid in [
        json!({"code": "agent-busy", "message": "m", "details": {}}),
        json!({"code": "title-invalid", "message": "m", "details": {}}),
        json!({"code": "command-error", "message": "m"}),
        json!({"code": "nope", "message": "m", "details": {}}),
        json!({"code": "settings-conflict", "message": "m", "details": {"ns": "x", "expected": "1", "actual": 2}}),
        json!({"code": "subagent-catalog-diagnostic", "message": "m", "details": {"parentSessionId": "p", "childSessionId": "c", "reason": "other"}}),
    ] {
        assert!(parse_rpc_error(&invalid).is_err(), "accepted {invalid}");
    }
}

#[test]
fn generic_result_requires_and_validates_the_success_value() {
    let parsed = parse_rpc_result(&json!({"ok": true, "value": {"n": 1}}), |value| {
        value.get("n").and_then(Value::as_i64).ok_or_else(|| {
            seekdeep_host_apiproxy::api::rpc::ContractError::new("$.value.n", "expected number")
        })
    });
    assert!(parsed.is_ok());
    assert!(parse_rpc_result::<Value>(&json!({"ok": true}), |value| Ok(value.clone())).is_err());
    assert!(
        parse_rpc_result::<Value>(&json!({"ok": true, "error": {}}), |value| {
            Ok(value.clone())
        })
        .is_err()
    );
    assert!(
        parse_rpc_result::<Value>(
            &json!({
                "ok": false,
                "error": {"code": "internal", "message": "x", "details": {}}
            }),
            |value| Ok(value.clone())
        )
        .is_ok()
    );
}

#[test]
fn four_wire_quadrants_parse_and_normalize() {
    let messages = [
        json!({"type": "client-request", "rpcId": "r1", "method": "session.list", "payload": {}, "ignored": 1}),
        json!({"type": "server-response", "rpcId": "r1", "result": {"ok": true, "value": 1}}),
        json!({"type": "server-request", "rpcId": "r2", "method": "session/event", "payload": {"a": 1}}),
        json!({"type": "client-response", "rpcId": "r2", "result": {"ok": true, "value": null}}),
    ];

    assert_eq!(
        parse_client_request(&messages[0]).unwrap().method,
        "session.list"
    );
    assert_eq!(
        parse_server_response(&messages[1]).unwrap().rpc_id.as_str(),
        "r1"
    );
    assert_eq!(
        parse_server_request(&messages[2]).unwrap().method,
        "session/event"
    );
    assert_eq!(
        parse_client_response(&messages[3]).unwrap().rpc_id.as_str(),
        "r2"
    );

    for message in messages {
        let parsed = parse_rpc_message(&message).unwrap();
        let round_trip = serde_json::to_value(&parsed).unwrap();
        assert_eq!(round_trip.get("type"), message.get("type"));
        let via_serde: RpcMessage = serde_json::from_value(message).unwrap();
        assert_eq!(via_serde, parsed);
    }
}

#[test]
fn wire_quadrants_require_members_but_allow_valueless_void_success() {
    for invalid in [
        json!({"type": "client-request", "rpcId": "r1"}),
        json!({"type": "server-response", "rpcId": "r1"}),
        json!({"type": "server-response", "rpcId": "r1", "result": {}}),
        json!({"type": "other", "rpcId": "x"}),
    ] {
        assert!(parse_rpc_message(&invalid).is_err(), "accepted {invalid}");
    }
    assert!(
        parse_server_response(
            &json!({"type": "server-response", "rpcId": "r1", "result": {"ok": true}})
        )
        .is_ok()
    );
}

#[test]
fn carrier_receipt_has_a_closed_reason_set() {
    for (value, expected) in [
        (json!({"accepted": true}), RpcReceipt::Accepted),
        (
            json!({"accepted": false, "reason": "not-pending"}),
            RpcReceipt::Rejected {
                reason: RpcReceiptReason::NotPending,
            },
        ),
        (
            json!({"accepted": false, "reason": "bad-response"}),
            RpcReceipt::Rejected {
                reason: RpcReceiptReason::BadResponse,
            },
        ),
    ] {
        assert_eq!(parse_rpc_receipt(&value).unwrap(), expected);
        assert_eq!(
            serde_json::from_value::<RpcReceipt>(value).unwrap(),
            expected
        );
    }
    assert!(parse_rpc_receipt(&json!({"accepted": false, "reason": "other"})).is_err());
}
