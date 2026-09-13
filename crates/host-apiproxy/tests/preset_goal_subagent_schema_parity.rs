//! Executable parity specifications for preset, Goal, and subagent schemas.

use seekdeep_host_apiproxy::api::{
    agent_presets::{
        AgentPresetCopyRequest, AgentPresetEntry, AgentPresetIdValue, AgentPresetListRequest,
        AgentPresetListValue, AgentPresetOpenDocumentValue, AgentPresetReadValue,
        AgentPresetSelectRequest,
    },
    goals::{GoalClearValue, GoalCreateRequest, GoalEditRequest, GoalRefRequest, GoalRefValue},
    subagents::{
        SubagentHistoryRequest, SubagentHistoryValue, SubagentInterruptRequest,
        SubagentInterruptValue, SubagentListValue, SubagentPromptRequest, SubagentPromptValue,
    },
};
use serde_json::json;

#[test]
fn agent_preset_roster_keeps_trust_closed_and_capabilities_required() {
    let entry = AgentPresetEntry::parse(&json!({
        "id": "standard",
        "trust": "system",
        "isDefault": true,
        "name": "Standard",
        "description": "Default",
        "broken": "diagnostic"
    }))
    .unwrap();
    assert_eq!(entry.id, "standard");
    for invalid in [
        json!({"id": "x", "trust": "root", "isDefault": false}),
        json!({"id": "", "trust": "user", "isDefault": false}),
        json!({"id": "x", "trust": "user", "isDefault": false, "broken": ""}),
    ] {
        assert!(
            AgentPresetEntry::parse(&invalid).is_err(),
            "accepted {invalid}"
        );
    }
    assert!(AgentPresetListRequest::parse(&json!({})).is_ok());
    assert_eq!(
        AgentPresetListValue::parse(
            &json!({"presets": [], "authorable": false, "hasDocument": false})
        )
        .unwrap()
        .presets,
        []
    );
    assert!(AgentPresetListValue::parse(&json!({"presets": [], "authorable": false})).is_err());
}

#[test]
fn agent_preset_mutation_schemas_preserve_request_and_response_id_asymmetry() {
    assert!(
        AgentPresetSelectRequest::parse(&json!({"sessionId": "s", "agentPreset": "minimal"}))
            .is_ok()
    );
    assert!(
        AgentPresetSelectRequest::parse(&json!({"sessionId": "s", "agentPreset": ""})).is_err()
    );
    assert!(AgentPresetIdValue::parse_request(&json!({"agentPreset": "minimal"})).is_ok());
    assert!(AgentPresetIdValue::parse_request(&json!({"agentPreset": ""})).is_err());
    // Select/copy values use z.string(), not min(1).
    assert!(AgentPresetIdValue::parse_value(&json!({"agentPreset": ""})).is_ok());

    assert!(
        AgentPresetReadValue::parse(&json!({
            "agentPreset": "minimal", "trust": "user", "content": "export default {}"
        }))
        .is_ok()
    );
    assert!(
        AgentPresetCopyRequest::parse(
            &json!({"from": "standard", "agentPreset": "mine", "name": "Mine"})
        )
        .is_ok()
    );
    assert!(AgentPresetCopyRequest::parse(&json!({"from": "", "agentPreset": "mine"})).is_err());

    assert_eq!(
        serde_json::to_value(
            AgentPresetOpenDocumentValue::parse(&json!({"opened": true})).unwrap()
        )
        .unwrap(),
        json!({"opened": true})
    );
    assert_eq!(
        serde_json::to_value(
            AgentPresetOpenDocumentValue::parse(&json!({"opened": false, "path": "/presets/mine"}))
                .unwrap()
        )
        .unwrap(),
        json!({"opened": false, "path": "/presets/mine"})
    );
    assert!(AgentPresetOpenDocumentValue::parse(&json!({"opened": false})).is_err());
}

#[test]
fn goal_schemas_enforce_positive_revisions_and_edit_presence_without_overvalidating_ids() {
    let reference = json!({"id": "g1", "revision": 1});
    assert!(
        GoalCreateRequest::parse(
            &json!({"sessionId": "", "objective": "objective", "maxGoalRounds": 3})
        )
        .is_ok()
    );
    assert!(GoalCreateRequest::parse(&json!({"sessionId": "s", "objective": ""})).is_err());
    assert!(
        GoalEditRequest::parse(
            &json!({"sessionId": "s1", "ref": reference, "objective": "updated"})
        )
        .is_ok()
    );
    assert!(
        GoalEditRequest::parse(&json!({
            "sessionId": "s1", "ref": {"id": "g1", "revision": 1}, "maxGoalRounds": 3
        }))
        .is_ok()
    );
    assert!(
        GoalEditRequest::parse(&json!({"sessionId": "s1", "ref": {"id": "g1", "revision": 1}}))
            .is_err()
    );
    assert!(
        GoalEditRequest::parse(&json!({
            "sessionId": "s1", "ref": {"id": "g1", "revision": 0}, "objective": "x"
        }))
        .is_err()
    );
    assert!(
        GoalRefRequest::parse(&json!({"sessionId": "s", "ref": {"id": "", "revision": 1}})).is_ok()
    );
    assert!(GoalRefValue::parse(&json!({"ref": {"id": "g", "revision": 2}})).is_ok());
    assert!(GoalClearValue::parse(&json!({"cleared": true})).is_ok());
    assert!(GoalClearValue::parse(&json!({"cleared": false})).is_err());
}

#[test]
fn subagent_catalog_union_enforces_mode_specific_label_and_diagnostic_reasons() {
    let catalog = SubagentListValue::parse(&json!({
        "entries": [
            {"kind": "child", "id": "one", "mode": "one-shot", "activity": "inactive", "hasChildren": false},
            {"kind": "child", "id": "cont", "mode": "continuable", "activity": "running", "hasChildren": true, "label": "Worker"},
            {"kind": "diagnostic", "id": "bad", "reason": "corrupt"}
        ],
        "parentAvailable": true
    }))
    .unwrap();
    assert_eq!(catalog.entries.len(), 3);
    assert!(
        SubagentListValue::parse(&json!({
            "entries": [{"kind": "child", "id": "cont", "mode": "continuable", "activity": "running", "hasChildren": false}],
            "parentAvailable": true
        }))
        .is_err()
    );
    assert!(
        SubagentListValue::parse(&json!({
            "entries": [{"kind": "diagnostic", "id": "bad", "reason": "future"}],
            "parentAvailable": true
        }))
        .is_err()
    );
}

#[test]
fn subagent_history_prompt_and_interrupt_keep_address_and_mode_authority() {
    assert!(
        SubagentHistoryRequest::parse(&json!({
            "parentSessionId": "p", "childSessionId": "c", "mode": "one-shot",
            "beforeSeq": 1, "maxMessages": 2
        }))
        .is_ok()
    );
    assert!(
        SubagentHistoryValue::parse(&json!({
            "events": [], "hasMore": false,
            "projections": {"asOfSeq": -1, "values": {}}
        }))
        .is_ok()
    );
    let prompt = SubagentPromptRequest::parse(&json!({
        "parentSessionId": "parent",
        "childSessionId": "child",
        "mode": "continuable",
        "content": [{"type": "future", "x": 1}],
        "clientTimeZone": "Asia/Shanghai"
    }))
    .unwrap();
    assert_eq!(prompt.client_time_zone.as_deref(), Some("Asia/Shanghai"));
    assert_eq!(prompt.content[0]["x"], 1);
    assert!(
        SubagentPromptRequest::parse(&json!({
            "parentSessionId": "parent", "childSessionId": "child",
            "mode": "one-shot", "content": []
        }))
        .is_err()
    );
    assert!(
        SubagentInterruptRequest::parse(&json!({
            "parentSessionId": "p", "childSessionId": "c", "mode": "continuable"
        }))
        .is_ok()
    );
    assert!(
        SubagentInterruptRequest::parse(&json!({
            "parentSessionId": "p", "childSessionId": "c", "mode": "one-shot"
        }))
        .is_err()
    );
    assert!(SubagentInterruptValue::parse(&json!({"accepted": true})).is_ok());
    // Source schema intentionally brands any string, including empty, for this receipt.
    assert_eq!(
        SubagentPromptValue::parse(&json!({"messageId": ""}))
            .unwrap()
            .message_id
            .as_str(),
        ""
    );
}
