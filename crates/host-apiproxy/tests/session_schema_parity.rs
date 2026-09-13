//! Executable parity specifications for the Session API wire schemas.

use seekdeep_host_apiproxy::api::sessions::{
    AcceptedValue, ImageLimitsProjection, SESSION_SEARCH_RESULT_LIMIT,
    SESSION_SEARCH_SNIPPET_MAX_CODE_POINTS, SessionAttachmentRequest, SessionAttachmentValue,
    SessionCreateRequest, SessionCreateValue, SessionEvent, SessionForkRequest, SessionForkValue,
    SessionHistoryRequest, SessionHistoryValue, SessionListMetadata, SessionListRequest,
    SessionListValue, SessionModelsRequest, SessionModelsValue, SessionPromptRequest,
    SessionPromptValue, SessionRenameRequest, SessionRenameValue, SessionSearchRequest,
    SessionSearchValue, SessionSelectModelRequest, SessionSelectModelValue, SessionSummary,
    SessionUpdateQueueRequest, truncate_unicode_code_points,
};
use serde_json::{Value, json};

#[test]
fn unicode_truncation_counts_scalar_values_without_splitting_them() {
    assert_eq!(truncate_unicode_code_points("a😀b", 0), "");
    assert_eq!(truncate_unicode_code_points("a😀b", 1), "a");
    assert_eq!(truncate_unicode_code_points("a😀b", 2), "a😀");
    assert_eq!(truncate_unicode_code_points("a😀b", 3), "a😀b");
    assert_eq!(truncate_unicode_code_points("a😀b", 99), "a😀b");
}

#[test]
fn session_ids_summaries_and_open_event_envelopes_validate() {
    let summary = SessionSummary::parse(&json!({
        "sessionId": "s1",
        "updatedAt": 1,
        "running": true,
        "blank": false,
        "parentSessionId": "p",
        "origin": "subagent",
        "cwd": "/x",
        "agentPreset": "standard",
        "projections": {"asOfSeq": -1, "values": {"title": "T"}},
        "ignored": true
    }))
    .unwrap();
    assert_eq!(summary.session_id.as_str(), "s1");
    assert_eq!(summary.parent_session_id.as_ref().unwrap().as_str(), "p");
    assert!(
        !serde_json::to_value(summary)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("ignored")
    );

    for invalid in [
        json!({"sessionId": "", "updatedAt": 1, "running": false, "blank": true}),
        json!({"sessionId": "s", "updatedAt": 1, "running": false}),
        json!({"sessionId": "s", "updatedAt": 1, "running": false, "blank": true, "origin": "root"}),
        json!({"sessionId": "s", "updatedAt": 1, "running": false, "blank": true, "parentSessionId": null}),
        json!({"sessionId": "s", "updatedAt": 1, "running": false, "blank": true, "projections": {"asOfSeq": -2, "values": {}}}),
    ] {
        assert!(
            SessionSummary::parse(&invalid).is_err(),
            "accepted {invalid}"
        );
    }

    let event = SessionEvent::parse(&json!({
        "type": "future/event",
        "seq": 0,
        "time": 1,
        "data": {"any": true},
        "sourceEventSeqs": [0, 0.5],
        "surfaceOp": null,
        "ignorable": true,
        "extra": "stripped"
    }))
    .unwrap();
    assert_eq!(event.kind, "future/event");
    assert!(
        !serde_json::to_value(event)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("extra")
    );
    assert!(
        SessionEvent::parse(&json!({
            "type": "user/message", "seq": -1, "time": 1, "data": {}
        }))
        .is_err()
    );
    assert!(
        SessionEvent::parse(&json!({
            "type": "user/message", "seq": 0.5, "time": 1, "data": {}
        }))
        .is_err()
    );
    assert!(
        SessionEvent::parse(&json!({
            "type": "future/event", "seq": 0, "time": 1, "data": {}, "ignorable": false
        }))
        .is_err()
    );
}

#[test]
fn session_list_request_and_value_match_optional_cursor_contract() {
    assert_eq!(SessionListRequest::parse(&json!({})).unwrap().cursor, None);
    assert_eq!(
        SessionListRequest::parse(&json!({"cursor": "c", "extra": 1}))
            .unwrap()
            .cursor
            .as_deref(),
        Some("c")
    );
    assert!(SessionListRequest::parse(&json!({"cursor": null})).is_err());
    assert!(SessionListRequest::parse(&json!({"cursor": 1})).is_err());
    assert!(
        SessionListValue::parse(&json!({
            "items": [{"sessionId": "s", "updatedAt": 1, "running": false, "blank": true}]
        }))
        .is_ok()
    );
    assert!(SessionListValue::parse(&json!({"items": "bad"})).is_err());
}

#[test]
fn session_search_trims_with_ecmascript_rules_and_enforces_nul_and_utf16_bound() {
    assert_eq!(
        SessionSearchRequest::parse(&json!({"query": "\u{FEFF}  exact phrase  \u{3000}"}))
            .unwrap()
            .query,
        "exact phrase"
    );
    assert!(SessionSearchRequest::parse(&json!({"query": "   "})).is_err());
    assert!(SessionSearchRequest::parse(&json!({"query": "bad\0query"})).is_err());
    assert!(SessionSearchRequest::parse(&json!({"query": "x".repeat(501)})).is_err());
    assert!(SessionSearchRequest::parse(&json!({"query": "😀".repeat(250)})).is_ok());
    assert!(SessionSearchRequest::parse(&json!({"query": "😀".repeat(251)})).is_err());
    // Rust considers U+0085 whitespace; ECMAScript String.prototype.trim does not.
    assert_eq!(
        SessionSearchRequest::parse(&json!({"query": "\u{0085}x\u{0085}"}))
            .unwrap()
            .query,
        "\u{0085}x\u{0085}"
    );
}

#[test]
fn session_search_value_is_bounded_by_rows_and_unicode_code_points() {
    let fitting = "😀".repeat(SESSION_SEARCH_SNIPPET_MAX_CODE_POINTS);
    let value = SessionSearchValue::parse(&json!({
        "items": [{"sessionId": "s1", "snippet": fitting}],
        "hasMore": true
    }))
    .unwrap();
    assert!(value.has_more);
    assert_eq!(
        value.items[0].snippet.chars().count(),
        SESSION_SEARCH_SNIPPET_MAX_CODE_POINTS
    );

    let overlong = "😀".repeat(SESSION_SEARCH_SNIPPET_MAX_CODE_POINTS + 1);
    assert!(
        SessionSearchValue::parse(&json!({
            "items": [{"sessionId": "s1", "snippet": overlong}], "hasMore": false
        }))
        .is_err()
    );
    assert!(
        SessionSearchValue::parse(&json!({
            "items": [{"sessionId": "", "snippet": "x"}], "hasMore": false
        }))
        .is_err()
    );
    let too_many: Vec<Value> = (0..=SESSION_SEARCH_RESULT_LIMIT)
        .map(|index| json!({"sessionId": format!("s{index}"), "snippet": "x"}))
        .collect();
    assert!(SessionSearchValue::parse(&json!({"items": too_many, "hasMore": true})).is_err());
    assert!(SessionSearchValue::parse(&json!({"items": [], "hasMore": null})).is_err());
}

#[test]
fn create_rename_fork_and_history_request_schemas_enforce_refinements() {
    assert_eq!(
        SessionCreateRequest::parse(&json!({"cwd": "/w"}))
            .unwrap()
            .cwd
            .as_deref(),
        Some("/w")
    );
    let reserved = SessionCreateRequest::parse(&json!({
        "workspaceId": "w1", "sessionId": "s1", "agentPreset": "minimal"
    }))
    .unwrap();
    assert_eq!(reserved.workspace_id.unwrap().as_str(), "w1");
    assert_eq!(reserved.session_id.unwrap().as_str(), "s1");
    assert!(SessionCreateRequest::parse(&json!({"workspaceId": "w1", "cwd": "/w"})).is_err());
    assert!(SessionCreateRequest::parse(&json!({"workspaceId": ""})).is_err());
    assert_eq!(
        SessionCreateValue::parse(&json!({"sessionId": "s1"}))
            .unwrap()
            .session_id
            .as_str(),
        "s1"
    );

    assert!(SessionRenameRequest::parse(&json!({"sessionId": "s", "title": ""})).is_ok());
    assert!(SessionRenameValue::parse(&json!({"title": "", "seq": 0})).is_err());
    assert!(SessionRenameValue::parse(&json!({"title": "T", "seq": 0})).is_ok());
    assert!(SessionForkRequest::parse(&json!({"sessionId": "s", "atSeq": 3})).is_ok());
    assert!(SessionForkRequest::parse(&json!({"sessionId": "s", "atSeq": -1})).is_err());
    assert_eq!(
        SessionForkValue::parse(&json!({"sessionId": "child"}))
            .unwrap()
            .session_id
            .as_str(),
        "child"
    );

    let history =
        SessionHistoryRequest::parse(&json!({"sessionId": "s1", "beforeSeq": 3, "maxMessages": 5}))
            .unwrap();
    assert_eq!(history.before_seq, Some(3));
    assert!(SessionHistoryRequest::parse(&json!({"sessionId": "s1", "maxMessages": 0})).is_err());
    assert!(SessionHistoryRequest::parse(&json!({"sessionId": "s1", "beforeSeq": 0.5})).is_err());
}

#[test]
fn model_directory_contract_requires_routability_and_nonempty_reasoning_metadata() {
    assert_eq!(
        SessionModelsRequest::parse(&json!({"sessionId": "s1"}))
            .unwrap()
            .session_id
            .as_str(),
        "s1"
    );
    let directory = json!({
        "current": {"provider": "deepseek-official", "model": "deepseek-v4-flash", "reasoningEffort": "max"},
        "routable": true,
        "groups": [{
            "id": "deepseek-official",
            "name": "DeepSeek",
            "models": [{
                "id": "deepseek-v4-flash",
                "name": "DeepSeek V4 Flash",
                "description": "fast",
                "reasoning": {
                    "efforts": [
                        {"id": "off", "name": "Off"},
                        {"id": "max", "name": "Max", "description": "Largest budget"}
                    ],
                    "defaultEffort": "off"
                }
            }]
        }],
        "failures": [{"id": "broken", "name": "Broken", "message": "offline"}]
    });
    let parsed = SessionModelsValue::parse(&directory).unwrap();
    assert!(parsed.routable);
    assert_eq!(parsed.groups[0].models[0].id, "deepseek-v4-flash");
    let mut missing_routable = directory.clone();
    missing_routable.as_object_mut().unwrap().remove("routable");
    assert!(SessionModelsValue::parse(&missing_routable).is_err());
    assert!(
        SessionModelsValue::parse(&json!({
            "current": {"provider": "p", "model": "m"},
            "routable": true,
            "groups": [{"id": "p", "name": "P", "models": [{
                "id": "m", "name": "M", "reasoning": {"efforts": []}
            }]}],
            "failures": []
        }))
        .is_err()
    );

    let selection = SessionSelectModelRequest::parse(&json!({
        "sessionId": "s1", "provider": "p", "model": "m", "reasoningEffort": "max"
    }))
    .unwrap();
    assert_eq!(selection.reasoning_effort.as_deref(), Some("max"));
    assert!(
        SessionSelectModelRequest::parse(&json!({
            "sessionId": "s1", "provider": "", "model": "m"
        }))
        .is_err()
    );
    assert!(
        SessionSelectModelRequest::parse(&json!({
            "sessionId": "s1", "provider": "p", "model": "m", "reasoningEffort": ""
        }))
        .is_err()
    );
    assert_eq!(
        SessionSelectModelValue::parse(&json!({"selected": {"provider": "p", "model": "m"}}))
            .unwrap()
            .selected
            .model,
        "m"
    );
}

#[test]
fn history_projection_and_host_computed_view_contracts_remain_open_only_where_declared() {
    let history = SessionHistoryValue::parse(&json!({
        "events": [{
            "event": {"type": "future/event", "seq": 0, "time": 1, "data": null},
            "view": {"for": "call", "view": {"card": "terminal", "future": {"x": 1}}, "stripped": true}
        }],
        "hasMore": false,
        "projections": {"asOfSeq": 0, "values": {"future": {"any": true}}}
    }))
    .unwrap();
    assert!(!history.has_more);
    assert_eq!(
        history.events[0].view.as_ref().unwrap().view["card"],
        "terminal"
    );
    assert_eq!(history.projections.unwrap().values["future"]["any"], true);
    assert!(
        SessionHistoryValue::parse(&json!({
            "events": [{"event": {"type": "t", "seq": 0, "time": 1, "data": {}}, "view": {"for": "other", "view": {"card": "x"}}}],
            "hasMore": false
        }))
        .is_err()
    );

    assert!(SessionListMetadata::parse(&json!({"blank": true, "lastPromptAt": null})).is_ok());
    assert!(SessionListMetadata::parse(&json!({"blank": true})).is_err());
    assert!(
        ImageLimitsProjection::parse(&json!({
            "maxImageBytes": 1,
            "maxImagesPerMessage": 2,
            "maxMessageImageBytes": 3,
            "maxImagePixels": 4,
            "mediaTypes": ["future/type"]
        }))
        .is_ok()
    );
    assert!(
        ImageLimitsProjection::parse(&json!({
            "maxImageBytes": 0,
            "maxImagesPerMessage": 2,
            "maxMessageImageBytes": 3,
            "maxImagePixels": 4,
            "mediaTypes": []
        }))
        .is_err()
    );
}

#[test]
fn prompt_attachment_queue_and_acceptance_schemas_preserve_their_exact_unions() {
    let prompt = SessionPromptRequest::parse(&json!({
        "sessionId": "s1",
        "mode": "queue",
        "content": [
            {"type": "text", "text": "hi", "stripped": true},
            {"type": "image", "mediaType": "image/png", "data": "AA==", "name": "x.png"}
        ],
        "clientTimeZone": "Asia/Shanghai"
    }))
    .unwrap();
    assert_eq!(prompt.client_time_zone.as_deref(), Some("Asia/Shanghai"));
    assert_eq!(
        serde_json::to_value(&prompt.content[0]).unwrap(),
        json!({"type": "text", "text": "hi"})
    );
    assert!(
        SessionPromptRequest::parse(&json!({"sessionId": "s1", "mode": "inject", "content": []}))
            .is_err()
    );
    assert!(
        SessionPromptRequest::parse(&json!({
            "sessionId": "s1", "mode": "queue", "content": [{"type": "image", "mediaType": "image/svg+xml", "data": "x"}]
        }))
        .is_err()
    );
    assert!(SessionPromptValue::parse(&json!({"accepted": true})).is_ok());
    assert!(
        SessionPromptValue::parse(&json!({"accepted": true, "command": {"kind": "success"}}))
            .is_ok()
    );
    assert!(
        SessionPromptValue::parse(&json!({"accepted": true, "command": {"kind": "failure"}}))
            .is_err()
    );

    assert!(
        SessionAttachmentRequest::parse(&json!({"sessionId": "s", "attachmentId": "a"})).is_ok()
    );
    let attachment = SessionAttachmentValue::parse(&json!({
        "attachment": {
            "attachmentId": "a", "mediaType": "image/jpeg", "bytes": 1,
            "width": 2, "height": 3, "extra": "stripped"
        },
        "data": "AA=="
    }))
    .unwrap();
    assert_eq!(attachment.attachment.attachment_id.as_str(), "a");
    assert!(
        SessionAttachmentValue::parse(&json!({
            "attachment": {"attachmentId": "a", "mediaType": "image/jpeg", "bytes": 0, "width": 2, "height": 3},
            "data": ""
        }))
        .is_err()
    );

    let edit = SessionUpdateQueueRequest::parse(&json!({
        "sessionId": "s1", "itemId": "i1",
        "action": {"kind": "edit", "content": [{"type": "future", "extra": 1}]}
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(edit.action).unwrap()["content"][0]["extra"],
        1
    );
    for kind in ["remove", "steer"] {
        assert!(
            SessionUpdateQueueRequest::parse(&json!({
                "sessionId": "s1", "itemId": "i1", "action": {"kind": kind}
            }))
            .is_ok()
        );
    }
    assert!(
        SessionUpdateQueueRequest::parse(&json!({
            "sessionId": "s1", "itemId": "i1", "action": {"kind": "promote"}
        }))
        .is_err()
    );
    assert!(AcceptedValue::parse(&json!({"accepted": true})).is_ok());
    assert!(AcceptedValue::parse(&json!({"accepted": false})).is_err());
}
