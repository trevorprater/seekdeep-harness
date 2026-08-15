//! Deterministic crash-tail repair for interrupted turns.

use indexmap::IndexMap;
use seekdeep_llm::{CallId, ContentBlock, Message, MessageId, MessageRole, MessageSource};
use serde_json::{Value, json};

use crate::session::{SessionEvent, SurfaceOp};

/// A requested tool never reached a durable `tool/call` record.
pub const TOOL_NOT_STARTED: &str = "TOOL_NOT_STARTED";
/// A started tool has no durable outcome, so retry safety is unknown.
pub const TOOL_OUTCOME_UNKNOWN: &str = "TOOL_OUTCOME_UNKNOWN";

const NOT_STARTED_TEXT: &str = "The tool call was interrupted before Seekdeep recorded it as started. Retry it if it is still needed.";
const OUTCOME_UNKNOWN_TEXT: &str = "The tool call was interrupted after it was recorded, but no result was durably recorded. Its outcome is unknown. Decide whether to retry from the tool semantics: retry only if the operation is read-only or idempotent; if it may have side effects, first verify external state or ask the user. Do not retry blindly.";

#[derive(Clone, Debug)]
struct PendingCall {
    step: i64,
    call_seq: Option<u64>,
}

/// Returns deterministic events that close an open crash-tail turn.
#[must_use]
pub fn interrupted_turn_closers(events: &[SessionEvent]) -> Vec<SessionEvent> {
    let mut open_turn = None;
    let mut open_step = None;
    let mut pending = IndexMap::<String, PendingCall>::new();
    for event in events {
        match event.event_type.as_str() {
            "turn/start" => {
                open_turn = integer_field(&event.data, "turn");
                open_step = None;
                pending.clear();
            }
            "turn/end" => {
                open_turn = None;
                open_step = None;
                pending.clear();
            }
            "step/start" => open_step = integer_field(&event.data, "step"),
            "step/end" => {
                pending.clear();
                open_step = None;
            }
            "assistant/message" => register_assistant_calls(event, &mut pending),
            "tool/call" => {
                if let Some(call_id) = event.data.get("callId").and_then(Value::as_str)
                    && let Some(call) = pending.get_mut(call_id)
                {
                    call.call_seq = Some(event.seq);
                }
            }
            "tool/result" => {
                if let Some(call_id) = event
                    .data
                    .pointer("/message/source/callId")
                    .and_then(Value::as_str)
                {
                    pending.shift_remove(call_id);
                }
            }
            _ => {}
        }
    }
    let (Some(turn), Some(last)) = (open_turn, events.last()) else {
        return Vec::new();
    };
    synthesize_closers(turn, open_step, pending, last)
}

fn register_assistant_calls(event: &SessionEvent, pending: &mut IndexMap<String, PendingCall>) {
    let Some(step) = integer_field(&event.data, "step") else {
        return;
    };
    let Some(content) = event
        .data
        .pointer("/message/content")
        .and_then(Value::as_array)
    else {
        return;
    };
    for block in content {
        if block.get("type").and_then(Value::as_str) == Some("tool-call")
            && let Some(id) = block.get("id").and_then(Value::as_str)
        {
            pending.insert(
                id.to_owned(),
                PendingCall {
                    step,
                    call_seq: None,
                },
            );
        }
    }
}

fn synthesize_closers(
    turn: i64,
    open_step: Option<i64>,
    pending: IndexMap<String, PendingCall>,
    last: &SessionEvent,
) -> Vec<SessionEvent> {
    let mut seq = last.seq.saturating_add(1);
    let mut closers = Vec::new();
    for (call_id, call) in pending {
        closers.push(tool_result_closer(turn, &call_id, &call, seq, last.time));
        seq = seq.saturating_add(1);
    }
    if let Some(step) = open_step {
        closers.push(plain_closer(
            "step/end",
            seq,
            last.time,
            json!({"turn": turn, "step": step}),
        ));
        seq = seq.saturating_add(1);
    }
    closers.push(plain_closer(
        "turn/end",
        seq,
        last.time,
        json!({"turn": turn, "reason": {"kind": "interrupted"}}),
    ));
    closers
}

fn tool_result_closer(
    turn: i64,
    call_id: &str,
    pending: &PendingCall,
    seq: u64,
    time: i64,
) -> SessionEvent {
    let started = pending.call_seq.is_some();
    let call_id = CallId::new(call_id);
    let message = Message {
        id: MessageId::new(format!("interrupted-tool-result-{call_id}-{seq}")),
        role: MessageRole::User,
        source: MessageSource::tool(&call_id),
        content: vec![ContentBlock::ToolResult {
            tool_call_id: call_id,
            is_error: Some(true),
            content: vec![ContentBlock::Text {
                text: if started {
                    OUTCOME_UNKNOWN_TEXT.to_owned()
                } else {
                    NOT_STARTED_TEXT.to_owned()
                },
            }],
        }],
    };
    SessionEvent {
        event_type: "tool/result".to_owned(),
        seq,
        time,
        data: json!({
            "turn": turn,
            "step": pending.step,
            "message": message,
            "error": if started {
                json!({"name": "ToolOutcomeUnknownError", "code": TOOL_OUTCOME_UNKNOWN})
            } else {
                json!({"name": "ToolNotStartedError", "code": TOOL_NOT_STARTED})
            },
        }),
        source_event_seqs: pending.call_seq.map(|call_seq| vec![call_seq]),
        surface_op: Some(SurfaceOp::append()),
        ignorable: None,
    }
}

fn plain_closer(event_type: &str, seq: u64, time: i64, data: Value) -> SessionEvent {
    SessionEvent {
        event_type: event_type.to_owned(),
        seq,
        time,
        data,
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    }
}

fn integer_field(value: &Value, field: &str) -> Option<i64> {
    value.get(field)?.as_i64()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(event_type: &str, seq: u64, data: Value) -> SessionEvent {
        plain_closer(
            event_type,
            seq,
            i64::try_from(seq).expect("small seq"),
            data,
        )
    }

    #[test]
    fn balanced_and_empty_logs_need_no_repair() {
        assert!(interrupted_turn_closers(&[]).is_empty());
        let events = vec![
            event("turn/start", 0, json!({"turn": 1})),
            event(
                "turn/end",
                1,
                json!({"turn": 1, "reason": {"kind": "completed"}}),
            ),
        ];
        assert!(interrupted_turn_closers(&events).is_empty());
    }

    #[test]
    fn closes_step_before_turn() {
        let events = vec![
            event("turn/start", 0, json!({"turn": 1})),
            event("step/start", 1, json!({"turn": 1, "step": 1})),
        ];
        let closers = interrupted_turn_closers(&events);
        assert_eq!(
            closers
                .iter()
                .map(|event| event.event_type.as_str())
                .collect::<Vec<_>>(),
            ["step/end", "turn/end"]
        );
    }

    #[test]
    fn distinguishes_not_started_from_unknown_outcome() {
        let assistant = |seq, id: &str| {
            event(
                "assistant/message",
                seq,
                json!({
                    "turn": 1,
                    "step": 1,
                    "message": {
                        "id": "m",
                        "role": "assistant",
                        "source": {"kind": "model", "provider": "mock", "model": "mock"},
                        "content": [{"type": "tool-call", "id": id, "name": "bash", "arguments": "{}"}],
                    }
                }),
            )
        };
        let base = vec![
            event("turn/start", 0, json!({"turn": 1})),
            event("step/start", 1, json!({"turn": 1, "step": 1})),
            assistant(2, "call-1"),
        ];
        let not_started = interrupted_turn_closers(&base);
        assert_eq!(not_started[0].data["error"]["code"], TOOL_NOT_STARTED);

        let mut started = base;
        started.push(event(
            "tool/call",
            3,
            json!({"turn": 1, "step": 1, "callId": "call-1", "name": "bash", "arguments": "{}"}),
        ));
        let unknown = interrupted_turn_closers(&started);
        assert_eq!(unknown[0].data["error"]["code"], TOOL_OUTCOME_UNKNOWN);
        assert_eq!(unknown[0].source_event_seqs, Some(vec![3]));
    }
}
