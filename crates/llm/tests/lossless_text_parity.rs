//! Exact JavaScript tool-result text through message construction and JSON.

use seekdeep_llm::{
    BlockAssembler, CallId, ContentBlock, JsonString, Message, MessageId, MessageRole,
    MessageSource, StreamChunk, assistant_text,
};
use seekdeep_lossless_json::JsonValue;

#[test]
fn text_retains_surrogates_and_distinguishes_literal_json_escapes() {
    for (raw, units) in [
        (r#""\ud800""#, vec![0xd800]),
        (r#""\udfff""#, vec![0xdfff]),
        (r#""😀""#, vec![0xd83d, 0xde00]),
        (r#""\\ud800""#, "\\ud800".encode_utf16().collect()),
        (
            r#""a\ud800\n\udfffz""#,
            vec![0x61, 0xd800, 0x0a, 0xdfff, 0x7a],
        ),
    ] {
        let wire = format!(r#"{{"type":"text","text":{raw}}}"#);
        let block = ContentBlock::text_utf16(&units);
        assert_eq!(serde_json::to_string(&block).unwrap(), wire);
        assert_eq!(serde_json::from_str::<ContentBlock>(&wire).unwrap(), block);
        let ContentBlock::Text { text } = block else {
            panic!("text block")
        };
        assert_eq!(text.utf16_units(), units);
        assert_eq!(text.as_str().is_some(), String::from_utf16(&units).is_ok());
    }
}

#[test]
fn source_root_string_result_is_unquoted_text_inside_the_tool_message() {
    let call = CallId::new("lossless-call");
    let tool = Message::tool_result(&call, vec![ContentBlock::text_utf16(&[0xd800])], false);
    let message = Message::from_existing(
        MessageId::new("lossless-message"),
        tool.role(),
        tool.content().to_vec(),
        tool.source().clone(),
        tool.fields().clone(),
    );
    let raw = r#"{"source":{"kind":"tool","callId":"lossless-call"},"content":[{"type":"tool-result","toolCallId":"lossless-call","content":[{"type":"text","text":"\ud800"}],"isError":false}],"role":"user","id":"lossless-message"}"#;
    assert_eq!(serde_json::to_string(&message).unwrap(), raw);
    let snapshot = JsonValue::from_serialize(&message).unwrap();
    assert_eq!(snapshot.as_raw(), raw);
    assert_eq!(snapshot.deserialize::<Message>().unwrap(), message);
    assert_eq!(serde_json::from_str::<Message>(raw).unwrap(), message);
}

#[test]
fn nested_structured_content_and_ordinary_fields_round_trip_together() {
    let raw = r#"{"content":[{"type":"text","text":"before \ud800"},{"type":"tool-result","toolCallId":"inner","content":[{"type":"text","text":"\udfff"},{"type":"text","text":"\\ud800"}],"isError":false},{"type":"text","text":"😀"}],"source":{"kind":"plugin","plugin":"fixture"},"extra":{"keep":true},"role":"user","id":"nested"}"#;
    let message: Message = serde_json::from_str(raw).unwrap();
    assert_eq!(message.role(), MessageRole::User);
    assert_eq!(message.source(), &MessageSource::plugin("fixture"));
    assert_eq!(serde_json::to_string(&message).unwrap(), raw);
    assert_eq!(
        JsonValue::from_serialize(&message)
            .unwrap()
            .deserialize::<Message>()
            .unwrap(),
        message
    );
}

#[test]
fn text_concatenation_joins_exact_code_units_across_block_boundaries() {
    let text = assistant_text(&[
        ContentBlock::text("start "),
        ContentBlock::text_utf16(&[0xd800]),
        ContentBlock::Reasoning {
            text: "hidden".into(),
        },
        ContentBlock::text_utf16(&[0xdc00, 0xdfff]),
        ContentBlock::text(" end"),
    ]);
    assert_eq!(text.as_raw(), r#""start 𐀀\udfff end""#);
    assert_eq!(
        text,
        JsonString::parse(r#""start \ud800\udc00\udfff end""#.into()).unwrap()
    );
    assert!(text.as_str().is_none());
}

#[test]
fn ordinary_rust_strings_keep_the_existing_value_wire_form() {
    let block = ContentBlock::text(String::from("ordinary 😀 text"));
    let value = serde_json::to_value(&block).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"type":"text","text":"ordinary 😀 text"})
    );
    assert_eq!(
        serde_json::from_value::<ContentBlock>(value).unwrap(),
        block
    );
}

#[test]
fn complete_stream_blocks_round_trip_without_buffering_the_text_as_utf8() {
    for block in [
        ContentBlock::text("ordinary"),
        ContentBlock::text_utf16(&[0xd800]),
    ] {
        let chunk = StreamChunk::BlockEnd { index: 0, block };
        let raw = serde_json::to_string(&chunk).unwrap();
        assert_eq!(serde_json::from_str::<StreamChunk>(&raw).unwrap(), chunk);
    }
}

#[test]
fn persisted_duplicate_message_fields_use_the_last_json_value() {
    let raw = r#"{"id":"old","id":"kept","role":"assistant","role":"user","content":[],"content":[{"type":"text","text":"old","text":"\ud800"}],"source":{"kind":"user"},"extra":false,"extra":true}"#;
    let message: Message = serde_json::from_str(raw).unwrap();
    assert_eq!(message.id().as_str(), "kept");
    assert_eq!(message.role(), MessageRole::User);
    assert_eq!(message.content(), [ContentBlock::text_utf16(&[0xd800])]);
    assert_eq!(message.fields()["extra"], true);
    assert_eq!(
        serde_json::to_string(&message).unwrap(),
        r#"{"content":[{"type":"text","text":"\ud800"}],"source":{"kind":"user"},"extra":true,"role":"user","id":"kept"}"#,
    );
}

#[test]
fn opaque_message_and_source_fields_retain_surrogate_keys_values_and_order() {
    let raw = r#"{"content":[{"type":"text","text":"body"}],"source":{"kind":"plugin","plugin":"fixture","opaque":{"\ud800":"\udfff","n":1.2300}},"first":true,"\ud800":{"literal":"\\ud800","text":"\ud800"},"role":"user","id":"opaque"}"#;
    let message: Message = serde_json::from_str(raw).unwrap();
    assert_eq!(serde_json::to_string(&message).unwrap(), raw);
    assert_eq!(
        message.source().fields.get("opaque").unwrap().as_raw(),
        r#"{"\ud800":"\udfff","n":1.2300}"#
    );
    assert_eq!(
        message
            .fields()
            .get_key(&JsonString::from_utf16(&[0xd800]))
            .unwrap()
            .as_raw(),
        r#"{"literal":"\\ud800","text":"\ud800"}"#
    );
}

#[test]
fn finish_and_assembly_retain_opaque_replay_values_including_explicit_null() {
    for replay in [r#"{"\ud800":["\udfff",1.2300]}"#, "null"] {
        let raw =
            format!(r#"{{"type":"finish","reason":{{"kind":"stop"}},"replayState":{replay}}}"#);
        let chunk: StreamChunk = serde_json::from_str(&raw).unwrap();
        assert_eq!(serde_json::to_string(&chunk).unwrap(), raw);
        let mut assembler = BlockAssembler::new();
        assembler.push(chunk);
        let state = assembler.replay_state().unwrap();
        assert_eq!(state.as_raw(), replay);
        let mut source = MessageSource::model("fixture", "fixture");
        source.fields.insert("replayState", state.clone());
        let source = JsonValue::from_serialize(&source).unwrap();
        assert_eq!(source.get("replayState").unwrap().as_raw(), replay);
    }
}
