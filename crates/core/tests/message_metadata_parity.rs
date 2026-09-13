//! Session reconstruction preserves opaque message metadata and stream replay state.

use std::sync::Arc;

use seekdeep_core::{
    chunk_rows::{decode_storage_record_json, pack_chunk_runs},
    session::{AppendOptions, JsonValue, Session, SessionEvent, SessionId, SurfaceOp},
};
use seekdeep_llm::{
    CallId, ContentBlock, FinishReason, JsonString, Message, MessageFields, MessageRole,
    MessageSource, StreamChunk,
};

const OPAQUE: &str = r#"{"\ud800":["\udfff",null,1.2300],"literal":"\\ud800"}"#;

fn restore(session: &Session) -> Arc<Session> {
    let mut events = Vec::<SessionEvent>::new();
    for record in pack_chunk_runs(&session.events()) {
        let line = serde_json::to_string(&record).unwrap();
        let raw = JsonValue::parse(line).unwrap();
        for event in decode_storage_record_json(raw).unwrap() {
            events.push(event.deserialize().unwrap());
        }
    }
    assert_eq!(events, session.events());
    let seed_length = u64::try_from(events.len()).unwrap();
    let restored = Session::create(session.id(), Some(events), None).unwrap();
    assert_eq!(restored.first_live_seq(), seed_length);
    assert_eq!(
        restored.events().last().unwrap().event_type,
        "session/end-seed"
    );
    restored
}

fn with_metadata(message: &Message) -> Message {
    let opaque = JsonValue::parse(OPAQUE.to_owned()).unwrap();
    let mut source = message.source().clone();
    source.fields.insert("opaque", opaque.clone());
    let mut fields = MessageFields::new();
    fields.insert("opaque", opaque);
    fields.insert(
        JsonString::from_utf16(&[0xd800]),
        JsonString::from_utf16(&[0xdfff]),
    );
    Message::from_existing(
        message.id().clone(),
        message.role(),
        message.content().to_vec(),
        source,
        fields,
    )
}

fn check_metadata(message: &Message) {
    assert_eq!(message.fields().get("opaque").unwrap().as_raw(), OPAQUE);
    assert_eq!(
        message.source().fields.get("opaque").unwrap().as_raw(),
        OPAQUE
    );
    assert_eq!(
        message
            .fields()
            .get_key(&JsonString::from_utf16(&[0xd800]))
            .unwrap()
            .to_utf16()
            .unwrap(),
        [0xdfff]
    );
}

#[test]
fn opaque_message_and_source_fields_survive_surface_storage_and_replay() {
    let text = vec![ContentBlock::Text {
        text: "kept".into(),
    }];
    let messages = [
        Message::user(text.clone(), MessageSource::plugin("fixture")),
        Message::assistant(text.clone(), "mock", "model"),
        Message::tool_result(&CallId::new("metadata-call"), text, false),
    ]
    .map(|message| with_metadata(&message));
    let session = Session::create(&SessionId::new("raw-message-metadata"), None, None).unwrap();
    for message in &messages {
        let (event_type, data) = match (message.role(), message.source().kind.as_str()) {
            (MessageRole::Assistant, _) => (
                "assistant/message",
                JsonValue::object([("message", JsonValue::from_serialize(message).unwrap())]),
            ),
            (MessageRole::User, "tool") => (
                "tool/result",
                JsonValue::object([("message", JsonValue::from_serialize(message).unwrap())]),
            ),
            (MessageRole::User, _) => ("user/message", JsonValue::from_serialize(message).unwrap()),
            (MessageRole::System, _) => unreachable!("fixture messages enter the surface"),
        };
        session
            .append_json(
                event_type,
                data,
                AppendOptions {
                    surface_op: Some(SurfaceOp::append()),
                    ..Default::default()
                },
            )
            .unwrap();
    }
    let restored = restore(&session);
    assert_eq!(restored.derive_messages(), messages);
    for message in restored.derive_messages() {
        check_metadata(&message);
        let encoded = JsonValue::from_serialize(&message).unwrap();
        let detached: Message = encoded.deserialize().unwrap();
        assert_eq!(detached, message);
        check_metadata(&detached);
    }
}

#[test]
fn finish_replay_state_retains_opaque_values_and_present_null_in_session_storage() {
    let states = [
        None,
        Some(JsonValue::parse("null".to_owned()).unwrap()),
        Some(JsonValue::parse(OPAQUE.to_owned()).unwrap()),
    ];
    let session = Session::create(&SessionId::new("raw-finish-replay"), None, None).unwrap();
    for replay_state in &states {
        let chunk = StreamChunk::Finish {
            reason: FinishReason::Stop,
            replay_state: replay_state.clone(),
        };
        session
            .append_json(
                "assistant/chunk",
                JsonValue::object([("chunk", JsonValue::from_serialize(&chunk).unwrap())]),
                AppendOptions::default(),
            )
            .unwrap();
    }
    let restored = restore(&session);
    for (event, expected) in restored.events().iter().zip(states) {
        let raw = event.data.get("chunk").unwrap();
        assert_eq!(raw.get("replayState").is_some(), expected.is_some());
        let chunk: StreamChunk = raw.deserialize().unwrap();
        let StreamChunk::Finish { replay_state, .. } = chunk else {
            panic!("stored finish chunk")
        };
        assert_eq!(replay_state, expected);
        if let Some(value) = replay_state {
            assert_eq!(
                value.as_raw(),
                if value.is_null() { "null" } else { OPAQUE }
            );
        }
    }
}
