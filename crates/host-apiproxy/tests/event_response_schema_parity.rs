//! Executable parity specifications for stream frames and response payloads.

use seekdeep_host_apiproxy::api::{
    approvals::ApprovalResponsePayload,
    events::{HostFrame, MuxFrame},
    jobs::JobView,
    questions::{QuestionResponsePayload, parse_question_answer},
};
use serde_json::{Value, json};

fn workspace() -> Value {
    json!({
        "workspaceId": "w",
        "path": "/w",
        "title": "w",
        "sessionIds": [],
        "createdAt": "0",
        "updatedAt": "0"
    })
}

#[test]
fn every_mux_frame_branch_parses_and_round_trips_its_discriminant() {
    let frames = vec![
        json!({"type": "session/event", "sessionId": "s", "event": {"type": "t", "seq": 0, "time": 1, "data": null}}),
        json!({"type": "session/subscribed", "sessionId": "s", "lastSeq": -1}),
        json!({"type": "approval/requested", "sessionId": "s", "approvalId": "a", "toolName": "bash", "callId": "c", "reason": "r"}),
        json!({"type": "approval/resolved", "sessionId": "s", "approvalId": "a", "outcome": "allowed-once"}),
        json!({"type": "question/requested", "sessionId": "s", "questions": [{"id": "q", "question": "Q?", "options": [{"label": "L"}], "multiSelect": true}]}),
        json!({"type": "question/resolved", "sessionId": "s", "questionRpcId": "r", "outcome": "answered"}),
        json!({"type": "session/queue", "sessionId": "s", "items": [{
            "id": "m1",
            "placement": "queued",
            "message": {"id": "m1", "role": "user", "content": [{"type": "future", "x": 1}], "source": {"kind": "user", "rpcId": "r9"}}
        }]}),
        json!({"type": "session/projection", "sessionId": "s", "key": "todos", "value": [{"content": "x", "status": "pending"}], "seq": 7}),
        json!({"type": "session/jobs", "sessionId": "s", "jobs": []}),
        json!({"type": "session/jobs", "sessionId": "s", "jobs": [
            {"id": "bash-1", "kind": "bash", "label": "pnpm run build", "status": "running", "startedAt": 5},
            {"id": "pty-send-2", "kind": "pty-send", "label": "send keys", "status": "failed", "detail": "exit code: 3", "startedAt": 5, "finishedAt": 9}
        ]}),
        json!({"type": "stream/error", "error": {"code": "internal", "message": "m", "details": {}}}),
    ];
    for frame in frames {
        let parsed =
            MuxFrame::parse(&frame).unwrap_or_else(|error| panic!("{}: {error}", frame["type"]));
        assert_eq!(serde_json::to_value(parsed).unwrap()["type"], frame["type"]);
    }
}

#[test]
fn mux_frames_reject_invalid_projection_job_and_answerable_shapes() {
    let invalid = vec![
        json!({"type": "unknown/frame"}),
        json!({"type": "question/requested", "sessionId": "s", "questions": []}),
        json!({"type": "session/projection", "sessionId": "s", "key": "", "value": null, "seq": 0}),
        json!({"type": "session/projection", "sessionId": "s", "key": "todos", "value": null, "seq": -1}),
        json!({"type": "session/projection", "sessionId": "s", "key": "todos", "value": null, "seq": 0.5}),
        json!({"type": "session/jobs", "sessionId": "s", "jobs": [{"id": "", "kind": "bash", "label": "l", "status": "running", "startedAt": 0}]}),
        json!({"type": "session/jobs", "sessionId": "s", "jobs": [{"id": "bash-1", "kind": "", "label": "l", "status": "running", "startedAt": 0}]}),
        json!({"type": "session/jobs", "sessionId": "s", "jobs": [{"id": "bash-1", "kind": "bash", "label": "", "status": "running", "startedAt": 0}]}),
        json!({"type": "session/jobs", "sessionId": "s", "jobs": [{"id": "bash-1", "kind": "bash", "label": "l", "status": "pending", "startedAt": 0}]}),
        json!({"type": "session/jobs", "sessionId": "s", "jobs": [{"id": "bash-1", "kind": "bash", "label": "l", "status": "running", "startedAt": -1}]}),
        json!({"type": "session/jobs", "sessionId": "s", "jobs": [{"id": "bash-1", "kind": "bash", "label": "l", "status": "completed", "startedAt": 0, "finishedAt": 0.5}]}),
    ];
    for frame in invalid {
        assert!(MuxFrame::parse(&frame).is_err(), "accepted {frame}");
    }
}

#[test]
fn question_intent_and_queue_placement_are_closed_while_message_interiors_stay_open() {
    let plan = MuxFrame::parse(&json!({
        "type": "question/requested",
        "sessionId": "s",
        "questions": [{
            "id": "plan-review",
            "question": "Approve?",
            "detail": "# Plan",
            "options": [{"label": "Approve"}],
            "intent": {"kind": "plan-review", "approve": "Approve", "stripped": true}
        }]
    }))
    .unwrap();
    let encoded = serde_json::to_value(plan).unwrap();
    assert_eq!(encoded["questions"][0]["intent"]["approve"], "Approve");
    assert!(encoded["questions"][0]["intent"].get("stripped").is_none());

    for intent in [
        json!({"kind": "plan-review"}),
        json!({"kind": "poll", "approve": "Approve"}),
        json!({"approve": "Approve"}),
    ] {
        assert!(
            MuxFrame::parse(&json!({
                "type": "question/requested", "sessionId": "s",
                "questions": [{"id": "q", "question": "Q?", "intent": intent}]
            }))
            .is_err()
        );
    }

    for placement in ["queued", "steering", "context"] {
        let frame = MuxFrame::parse(&json!({
            "type": "session/queue", "sessionId": "s", "items": [{
                "id": "m", "placement": placement,
                "message": {"id": "m", "role": "user", "content": [], "source": {"kind": "future", "extra": 1}}
            }]
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(frame).unwrap()["items"][0]["message"]["source"]["extra"],
            1
        );
    }
    assert!(
        MuxFrame::parse(&json!({
            "type": "session/queue", "sessionId": "s", "items": [{
                "id": "m", "placement": "bogus",
                "message": {"id": "m", "role": "user", "content": [], "source": {"kind": "user"}}
            }]
        }))
        .is_err()
    );
}

#[test]
fn every_host_frame_branch_parses_including_complete_order_and_archive_snapshots() {
    let frames = vec![
        json!({"type": "host/session-added", "sessionId": "s", "blank": true, "parentSessionId": "p", "origin": "subagent", "cwd": "/x", "agentPreset": "minimal"}),
        json!({"type": "host/session-removed", "sessionId": "s"}),
        json!({"type": "host/session-status", "sessionId": "s", "running": true}),
        json!({"type": "host/agent-error", "sessionId": "s", "message": "boom"}),
        json!({"type": "host/workspace-changed", "workspace": workspace()}),
        json!({"type": "host/workspace-removed", "workspaceId": "w"}),
        json!({"type": "host/workspace-order-changed", "workspaceIds": ["w2", "w1"]}),
        json!({"type": "host/archived-sessions-changed", "archivedSessionIds": ["s1"]}),
        json!({"type": "host/remote-event", "event": "commands/change", "args": []}),
        json!({"type": "host/remote-event", "event": "settings/document-updated", "args": ["ns", 3]}),
        json!({"type": "stream/error", "error": {"code": "internal", "message": "m", "details": {}}}),
    ];
    for frame in frames {
        let parsed =
            HostFrame::parse(&frame).unwrap_or_else(|error| panic!("{}: {error}", frame["type"]));
        assert_eq!(serde_json::to_value(parsed).unwrap()["type"], frame["type"]);
    }
    for invalid in [
        json!({"type": "host/session-added", "sessionId": "s", "blank": true, "origin": "root"}),
        json!({"type": "host/remote-event", "event": "", "args": []}),
        json!({"type": "host/workspace-order-changed", "workspaceIds": [""]}),
        json!({"type": "host/archived-sessions-changed", "archivedSessionIds": [null]}),
    ] {
        assert!(HostFrame::parse(&invalid).is_err(), "accepted {invalid}");
    }
}

#[test]
fn approval_and_question_client_response_payloads_accept_only_client_owned_shapes() {
    let approval = ApprovalResponsePayload::parse(
        &json!({"sessionId": "s", "approvalId": "a", "outcome": "rejected"}),
    )
    .unwrap();
    assert_eq!(approval.approval_id.as_str(), "a");
    for outcome in ["cancelled", "unavailable", "other"] {
        assert!(
            ApprovalResponsePayload::parse(
                &json!({"sessionId": "s", "approvalId": "a", "outcome": outcome})
            )
            .is_err()
        );
    }

    let answer = parse_question_answer(&json!({
        "answers": [{"id": "q", "selected": ["x"], "custom": "c", "stripped": true}]
    }))
    .unwrap();
    assert_eq!(answer.answers[0].selected, ["x"]);
    assert!(parse_question_answer(&json!({"answers": [{"id": "q", "selected": [1]}]})).is_err());
    let payload =
        QuestionResponsePayload::parse(&json!({"sessionId": "s", "answer": {"answers": []}}))
            .unwrap();
    assert_eq!(payload.session_id.as_str(), "s");
}

#[test]
fn standalone_job_view_keeps_kind_open_but_status_and_time_closed() {
    assert!(
        JobView::parse(&json!({
            "id": "future-1", "kind": "future", "label": "Work",
            "status": "stopping", "startedAt": 0
        }))
        .is_ok()
    );
    assert!(
        JobView::parse(&json!({
            "id": "future-1", "kind": "future", "label": "Work",
            "status": "future", "startedAt": 0
        }))
        .is_err()
    );
}
