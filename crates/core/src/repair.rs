//! Deterministic crash-tail repair for interrupted turns.

use indexmap::IndexMap;
use seekdeep_llm::{CallId, ContentBlock, Message, MessageId, MessageRole, MessageSource};
use serde_json::{Value, json};

use crate::session::{SessionEvent, SurfaceOp};

/// A requested tool never reached a durable `tool/call` record.
pub const TOOL_NOT_STARTED: &str = "TOOL_NOT_STARTED";
/// A started tool has no durable outcome, so retry safety is unknown.
pub const TOOL_OUTCOME_UNKNOWN: &str = "TOOL_OUTCOME_UNKNOWN";

const NOT_STARTED_TEXT: &str = "The tool call was interrupted before the SeekDeep Harness recorded it as started. Retry it if it is still needed.";
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
    let message = Message::from_existing(
        MessageId::new(format!("interrupted-tool-result-{call_id}-{seq}")),
        MessageRole::User,
        vec![ContentBlock::ToolResult {
            tool_call_id: call_id.clone(),
            is_error: Some(true),
            content: vec![ContentBlock::Text {
                text: if started {
                    OUTCOME_UNKNOWN_TEXT.to_owned()
                } else {
                    NOT_STARTED_TEXT.to_owned()
                },
            }],
        }],
        MessageSource::tool(&call_id),
        serde_json::Map::new(),
    );
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
    fn turn_start(turn: i64, seq: u64) -> SessionEvent {
        event("turn/start", seq, json!({"turn": turn}))
    }

    fn step_start(turn: i64, step: i64, seq: u64) -> SessionEvent {
        event("step/start", seq, json!({"turn": turn, "step": step}))
    }

    fn step_end(turn: i64, step: i64, seq: u64) -> SessionEvent {
        event("step/end", seq, json!({"turn": turn, "step": step}))
    }

    fn turn_end(turn: i64, seq: u64) -> SessionEvent {
        event(
            "turn/end",
            seq,
            json!({"turn": turn, "reason": {"kind": "completed"}}),
        )
    }

    fn assistant(seq: u64, turn: i64, step: i64, calls: &[&str]) -> SessionEvent {
        let content = calls
            .iter()
            .map(|id| json!({"type": "tool-call", "id": id, "name": "bash", "arguments": "{}"}))
            .collect::<Vec<_>>();
        event(
            "assistant/message",
            seq,
            json!({
                "turn": turn,
                "step": step,
                "message": {
                    "id": format!("m-{seq}"),
                    "role": "assistant",
                    "source": {"kind": "model", "provider": "mock", "model": "mock"},
                    "content": content,
                }
            }),
        )
    }

    fn tool_call(seq: u64, turn: i64, step: i64, call_id: &str) -> SessionEvent {
        event(
            "tool/call",
            seq,
            json!({"turn": turn, "step": step, "callId": call_id, "name": "bash", "arguments": "{}"}),
        )
    }

    fn tool_result(seq: u64, turn: i64, step: i64, call_id: &str) -> SessionEvent {
        event(
            "tool/result",
            seq,
            json!({
                "turn": turn,
                "step": step,
                "message": {
                    "id": format!("r-{seq}"),
                    "role": "user",
                    "source": {"kind": "tool", "callId": call_id},
                    "content": [{"type": "tool-result", "toolCallId": call_id, "isError": false, "content": [{"type": "text", "text": "ok"}]}],
                }
            }),
        )
    }

    fn closer_types(events: &[SessionEvent]) -> Vec<&str> {
        events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect()
    }

    #[test]
    fn closes_an_open_turn_with_no_open_step() {
        let closers = interrupted_turn_closers(&[turn_start(1, 0)]);
        assert_eq!(closer_types(&closers), ["turn/end"]);
        assert_eq!(closers[0].seq, 1);
        assert_eq!(closers[0].data["reason"], json!({"kind": "interrupted"}));
    }

    #[test]
    fn not_started_result_carries_message_text_and_contiguous_sequences() {
        let events = vec![
            turn_start(2, 0),
            step_start(2, 1, 1),
            assistant(2, 2, 1, &["call-1"]),
        ];
        let closers = interrupted_turn_closers(&events);
        assert_eq!(
            closer_types(&closers),
            ["tool/result", "step/end", "turn/end"]
        );
        assert_eq!(
            closers.iter().map(|event| event.seq).collect::<Vec<_>>(),
            [3, 4, 5]
        );
        let result = &closers[0];
        assert_eq!(result.data["turn"], json!(2));
        assert_eq!(result.data["step"], json!(1));
        assert_eq!(result.data["error"]["code"], TOOL_NOT_STARTED);
        assert!(matches!(
            &result.surface_op,
            Some(SurfaceOp::Marker(marker)) if marker == "append"
        ));
        let text = &result.data["message"]["content"][0]["content"][0]["text"];
        assert_eq!(
            text,
            "The tool call was interrupted before the SeekDeep Harness recorded it as started. Retry it if it is still needed."
        );
    }

    #[test]
    fn does_not_synthesize_a_result_for_an_already_answered_call() {
        let events = vec![
            turn_start(2, 0),
            step_start(2, 1, 1),
            assistant(2, 2, 1, &["call-1"]),
            tool_result(3, 2, 1, "call-1"),
        ];
        let closers = interrupted_turn_closers(&events);
        assert_eq!(closer_types(&closers), ["step/end", "turn/end"]);
    }

    #[test]
    fn does_not_synthesize_a_result_after_the_owning_step_closed() {
        let events = vec![
            turn_start(2, 0),
            step_start(2, 1, 1),
            assistant(2, 2, 1, &["call-1"]),
            step_end(2, 1, 3),
        ];
        let closers = interrupted_turn_closers(&events);
        assert_eq!(closer_types(&closers), ["turn/end"]);
        assert_eq!(closers[0].seq, 4);
    }

    #[test]
    fn synthesizes_results_only_for_the_still_open_turn() {
        let events = vec![
            turn_start(1, 0),
            step_start(1, 1, 1),
            assistant(2, 1, 1, &["old-call"]),
            tool_result(3, 1, 1, "old-call"),
            step_end(1, 1, 4),
            turn_end(1, 5),
            turn_start(2, 6),
            step_start(2, 1, 7),
            assistant(8, 2, 1, &["new-call"]),
        ];
        let closers = interrupted_turn_closers(&events);
        assert_eq!(
            closer_types(&closers),
            ["tool/result", "step/end", "turn/end"]
        );
        assert_eq!(closers[0].data["message"]["source"]["callId"], "new-call");
    }

    #[test]
    fn synthesizes_one_result_per_unanswered_call_in_log_order() {
        let events = vec![
            turn_start(1, 0),
            step_start(1, 1, 1),
            assistant(2, 1, 1, &["call-a", "call-b"]),
            tool_result(3, 1, 1, "call-a"),
        ];
        let closers = interrupted_turn_closers(&events);
        assert_eq!(
            closer_types(&closers),
            ["tool/result", "step/end", "turn/end"]
        );
        assert_eq!(closers[0].data["message"]["source"]["callId"], "call-b");
    }

    #[test]
    fn unknown_outcome_result_carries_surface_op_source_seqs_and_text() {
        let events = vec![
            turn_start(1, 0),
            step_start(1, 1, 1),
            assistant(2, 1, 1, &["call-1"]),
            tool_call(3, 1, 1, "call-1"),
        ];
        let closers = interrupted_turn_closers(&events);
        assert_eq!(
            closer_types(&closers),
            ["tool/result", "step/end", "turn/end"]
        );
        let result = &closers[0];
        assert!(matches!(
            &result.surface_op,
            Some(SurfaceOp::Marker(marker)) if marker == "append"
        ));
        assert_eq!(result.source_event_seqs, Some(vec![3]));
        assert_eq!(
            result.data["error"],
            json!({"name": "ToolOutcomeUnknownError", "code": TOOL_OUTCOME_UNKNOWN})
        );
        let text = result.data["message"]["content"][0]["content"][0]["text"]
            .as_str()
            .expect("text");
        assert!(text.contains("retry only if the operation is read-only or idempotent"));
        assert!(text.contains("first verify external state or ask the user"));
    }

    #[test]
    fn handles_an_orphan_tool_call_without_a_matching_assistant_entry() {
        let events = vec![
            turn_start(1, 0),
            step_start(1, 1, 1),
            tool_call(2, 1, 1, "orphan"),
        ];
        let closers = interrupted_turn_closers(&events);
        assert_eq!(closer_types(&closers), ["step/end", "turn/end"]);
    }
}
