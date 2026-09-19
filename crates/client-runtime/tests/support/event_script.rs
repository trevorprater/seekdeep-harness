//! Deterministic `SessionEvent` builders for Client runtime orchestration tests.

use serde_json::{Value, json};

#[allow(clippy::needless_pass_by_value)]
fn at(seq: u64, event_type: &str, data: Value) -> Value {
    json!({
        "seq":seq,
        "time":1_700_000_000_000_u64 + seq,
        "type":event_type,
        "data":data,
    })
}

fn text(value: &str) -> Value {
    json!([{"type":"text","text":value}])
}

fn message_id(seq: u64) -> String {
    format!("fixture-message-{seq}")
}

pub(crate) fn turn_start(seq: u64, turn: u64) -> Value {
    at(seq, "turn/start", json!({"turn":turn}))
}

pub(crate) fn user(seq: u64, body: &str) -> Value {
    let mut event = at(
        seq,
        "user/message",
        json!({
            "content":text(body),
            "source":{"kind":"user"},
            "role":"user",
            "id":message_id(seq),
        }),
    );
    event["surfaceOp"] = json!("append");
    event
}

pub(crate) fn step_start(seq: u64, turn: u64, step: u64) -> Value {
    at(seq, "step/start", json!({"turn":turn,"step":step}))
}

pub(crate) fn chunk_start(seq: u64, turn: u64, step: u64, index: u64) -> Value {
    at(
        seq,
        "assistant/chunk",
        json!({
            "turn":turn,"step":step,
            "chunk":{"type":"block-start","index":index,"blockType":"text"}
        }),
    )
}

pub(crate) fn chunk_text(seq: u64, turn: u64, step: u64, index: u64, piece: &str) -> Value {
    at(
        seq,
        "assistant/chunk",
        json!({
            "turn":turn,"step":step,
            "chunk":{"type":"text-delta","index":index,"text":piece}
        }),
    )
}

pub(crate) fn assistant(seq: u64, turn: u64, step: u64, body: &str) -> Value {
    let mut event = at(
        seq,
        "assistant/message",
        json!({
            "turn":turn,"step":step,
            "message":{
                "role":"assistant","content":text(body),
                "source":{"kind":"model","provider":"fake","model":"fk-1"},
                "id":message_id(seq),
            }
        }),
    );
    event["surfaceOp"] = json!("append");
    event
}

pub(crate) fn tool_call(
    seq: u64,
    turn: u64,
    step: u64,
    call_id: &str,
    name: &str,
    args: &str,
) -> Value {
    at(
        seq,
        "tool/call",
        json!({"turn":turn,"step":step,"callId":call_id,"name":name,"arguments":args}),
    )
}

pub(crate) fn tool_result(seq: u64, turn: u64, step: u64, call_id: &str, body: &str) -> Value {
    let mut event = at(
        seq,
        "tool/result",
        json!({
            "turn":turn,"step":step,
            "message":{
                "role":"user","source":{"kind":"tool","callId":call_id},
                "content":[{"type":"tool-result","toolCallId":call_id,"content":text(body),"isError":false}],
                "id":message_id(seq),
            }
        }),
    );
    event["surfaceOp"] = json!("append");
    event
}

pub(crate) fn code_dispatch_start(
    seq: u64,
    parent_call_id: &str,
    number: u64,
    name: &str,
    arguments: &Value,
) -> Value {
    at(
        seq,
        "tool/code-dispatch-start",
        json!({
            "rootCallId":parent_call_id,"parentCallId":parent_call_id,
            "subCallId":format!("{parent_call_id}:code:{number}"),
            "name":name,"arguments":arguments,
        }),
    )
}

pub(crate) fn code_dispatch(
    seq: u64,
    parent_call_id: &str,
    number: u64,
    name: &str,
    arguments: &Value,
    body: &str,
    is_error: bool,
) -> Value {
    at(
        seq,
        "tool/code-dispatch",
        json!({
            "rootCallId":parent_call_id,"parentCallId":parent_call_id,
            "subCallId":format!("{parent_call_id}:code:{number}"),
            "name":name,"arguments":arguments,"isError":is_error,"content":text(body),
        }),
    )
}

pub(crate) fn step_end(seq: u64, turn: u64, step: u64) -> Value {
    at(seq, "step/end", json!({"turn":turn,"step":step}))
}

pub(crate) fn retry(
    seq: u64,
    turn: u64,
    step: u64,
    retry: u64,
    max_retries: u64,
    delay_ms: u64,
    message: &str,
) -> Value {
    at(
        seq,
        "llm/retry",
        json!({
            "turn":turn,"step":step,"provider":"fake","mode":"normal",
            "policyKey":"fake-normal","retry":retry,"maxRetries":max_retries,
            "delayMs":delay_ms,"failure":{"code":"TRANSPORT","message":message},
        }),
    )
}

pub(crate) fn turn_end(seq: u64, turn: u64, reason: &str) -> Value {
    let reason = if reason == "completed" {
        json!({"kind":"completed"})
    } else {
        json!({"kind":"aborted","reason":{"kind":if reason == "disposed" { "disposed" } else { "user" }}})
    };
    at(seq, "turn/end", json!({"turn":turn,"reason":reason}))
}

pub(crate) fn command_run(seq: u64, command_id: &str, name: &str, args: Option<&str>) -> Value {
    let mut data = serde_json::Map::from_iter([
        ("commandId".to_owned(), json!(command_id)),
        ("name".to_owned(), json!(name)),
        ("source".to_owned(), json!({"kind":"user"})),
    ]);
    if let Some(args) = args {
        data.insert("args".to_owned(), json!(args));
    }
    at(seq, "command/run", Value::Object(data))
}

pub(crate) fn command_done(
    seq: u64,
    command_id: &str,
    kind: &str,
    text: Option<&str>,
    source_event_seq: Option<u64>,
) -> Value {
    let mut data = serde_json::Map::from_iter([
        ("commandId".to_owned(), json!(command_id)),
        ("kind".to_owned(), json!(kind)),
    ]);
    if let Some(text) = text {
        data.insert("text".to_owned(), json!(text));
    }
    if let Some(source_event_seq) = source_event_seq {
        data.insert("sourceEventSeq".to_owned(), json!(source_event_seq));
    }
    at(seq, "command/done", Value::Object(data))
}

pub(crate) fn compact_summary(seq: u64, summary: &str, start: u64, end: u64) -> Value {
    at(
        seq,
        "compaction/summary",
        json!({
            "summary":text(summary),"shadowedRange":{"start":start,"end":end},
            "shadowedSeqs":[start,end],"shadowedTokenCount":100,
            "provider":"fake","model":"compact-1",
        }),
    )
}

pub(crate) fn compact_checkpoint(seq: u64, summary_seq: u64, start: u64, end: u64) -> Value {
    let mut event = at(
        seq,
        "user/message",
        json!({
            "content":text("<context_checkpoint>model only</context_checkpoint>"),
            "source":{"kind":"plugin","plugin":"compact"},
            "role":"user","id":message_id(seq),
        }),
    );
    event["surfaceOp"] = json!({"op":"replace","start":start,"end":end});
    event["sourceEventSeqs"] = json!([summary_seq, start, end]);
    event
}

pub(crate) fn plain_turn(start_seq: u64, turn: u64, ask: &str, answer: &str) -> Vec<Value> {
    vec![
        turn_start(start_seq, turn),
        user(start_seq + 1, ask),
        step_start(start_seq + 2, turn, 0),
        assistant(start_seq + 3, turn, 0, answer),
        step_end(start_seq + 4, turn, 0),
        turn_end(start_seq + 5, turn, "completed"),
    ]
}

pub(crate) fn entries(events: &[Value]) -> Vec<Value> {
    events.iter().map(|event| json!({"event":event})).collect()
}
