//! JSON-pipe settlement frames retain the child's completed content blocks.

use seekdeep_llm::{ContentBlock, JsonString};
use seekdeep_workflow_worker_thread::{ChildResult, HostToWorkerMessage};

#[test]
fn child_settlement_json_round_trip_preserves_scalar_and_surrogate_text() {
    for text in [
        JsonString::from("scalar child answer"),
        JsonString::from_utf16(&[0xd800, 0x61, 0xdfff, 0xd83d, 0xde00]),
    ] {
        let message = HostToWorkerMessage::ChildSettled {
            call_id: 1,
            result: ChildResult {
                output: vec![ContentBlock::Text { text }],
                structured: None,
                stop_reason: "completed".to_owned(),
            },
        };
        let encoded = serde_json::to_string(&message).unwrap();
        let decoded: HostToWorkerMessage =
            serde_json::from_str(&encoded).expect("child settlement frame decodes");
        assert_eq!(decoded, message);
    }
}

#[test]
fn host_message_json_keeps_closed_tags_and_required_field_validation() {
    for raw in [
        r#"{}"#,
        r#"{"type":"unknown"}"#,
        r#"{"type":"go","type":"go"}"#,
        r#"{"type":"child-started","callId":1,"callId":2,"childId":"child"}"#,
        r#"{"type":"child-settled","callId":1,"result":null}"#,
    ] {
        assert!(
            serde_json::from_str::<HostToWorkerMessage>(raw).is_err(),
            "{raw}"
        );
    }
    let raw = r#"{"type":"go","ignored":{"\ud800":"\udfff"}}"#;
    assert_eq!(
        serde_json::from_str::<HostToWorkerMessage>(raw).unwrap(),
        HostToWorkerMessage::Go
    );
}
