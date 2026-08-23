//! Deterministic Rust port of the source Client event-script fixture.

#[path = "support/event_script.rs"]
mod event_script;

use serde_json::json;

#[test]
fn fixture_builders_cover_every_source_event_shape_and_optional_field_branch() {
    let events = vec![
        event_script::turn_start(1, 0),
        event_script::user(2, "ask"),
        event_script::step_start(3, 0, 0),
        event_script::chunk_start(4, 0, 0, 0),
        event_script::chunk_text(5, 0, 0, 0, "a"),
        event_script::assistant(6, 0, 0, "answer"),
        event_script::tool_call(7, 0, 0, "call", "read", "{}"),
        event_script::tool_result(8, 0, 0, "call", "done"),
        event_script::code_dispatch_start(9, "call", 1, "child", &json!({"x":1})),
        event_script::code_dispatch(10, "call", 1, "child", &json!({}), "done", false),
        event_script::step_end(11, 0, 0),
        event_script::retry(12, 0, 0, 1, 2, 500, "temporary"),
        event_script::turn_end(13, 0, "completed"),
        event_script::turn_end(14, 1, "aborted"),
        event_script::turn_end(15, 2, "disposed"),
        event_script::command_run(16, "cmd", "help", Some("x")),
        event_script::command_run(17, "cmd-2", "help", None),
        event_script::command_done(18, "cmd", "success", Some("ok"), Some(16)),
        event_script::command_done(19, "cmd", "error", None, None),
        event_script::compact_summary(20, "summary", 1, 19),
        event_script::compact_checkpoint(21, 20, 1, 19),
    ];
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["seq"], json!(index as u64 + 1));
        assert!(event["type"].is_string());
        assert!(event["data"].is_object());
    }
    assert_eq!(events[1]["surfaceOp"], "append");
    assert_eq!(
        events[7]["data"]["message"]["content"][0]["type"],
        "tool-result"
    );
    assert!(events[16]["data"].get("args").is_none());
    assert!(events[18]["data"].get("text").is_none());
    assert_eq!(events[20]["surfaceOp"]["op"], "replace");
    assert_eq!(events[20]["sourceEventSeqs"], json!([20, 1, 19]));
    let turn = event_script::plain_turn(30, 4, "ask", "answer");
    assert_eq!(turn.len(), 6);
    assert_eq!(turn[0]["type"], "turn/start");
    assert_eq!(turn[5]["type"], "turn/end");
    let entries = event_script::entries(&turn);
    assert_eq!(entries[0]["event"], turn[0]);
}
