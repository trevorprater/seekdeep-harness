use seekdeep_core::session::{
    AppendOptions, JsonValue, Session, SessionEvent, SessionId, SurfaceOp, derive_event_message,
};
use seekdeep_llm::{Message, MessageSource};
use serde_json::json;

#[test]
fn record_message_metadata_boundaries() {
    let cases = [
        (
            "content-surrogate-control",
            r#"{"id":"m","role":"user","content":[{"type":"text","text":"\ud800"}],"source":{"kind":"user"}}"#,
        ),
        (
            "literal-escape-control",
            r#"{"id":"m","role":"user","content":[{"type":"text","text":"body"}],"source":{"kind":"user"},"extra":{"text":"\\ud800"}}"#,
        ),
        (
            "scalar-extra-control",
            r#"{"id":"m","role":"user","content":[{"type":"text","text":"body"}],"source":{"kind":"user"},"extra":{"b":true,"a":[1,null]}}"#,
        ),
        (
            "message-extra-value",
            r#"{"id":"m","role":"user","content":[{"type":"text","text":"body"}],"source":{"kind":"user"},"extra":{"text":"\ud800"}}"#,
        ),
        (
            "message-extra-key",
            r#"{"id":"m","role":"user","content":[{"type":"text","text":"body"}],"source":{"kind":"user"},"extra":{"\ud800":"\udfff"}}"#,
        ),
        (
            "message-extra-name",
            r#"{"id":"m","role":"user","content":[{"type":"text","text":"body"}],"source":{"kind":"user"},"\ud800":true}"#,
        ),
        (
            "source-notice-summary",
            r#"{"id":"m","role":"user","content":[{"type":"text","text":"body"}],"source":{"kind":"plugin","plugin":"fixture","form":"notice","summary":"\ud800"}}"#,
        ),
        (
            "source-snapshot-text",
            r#"{"id":"m","role":"user","content":[{"type":"text","text":"body"}],"source":{"kind":"plugin","plugin":"fixture","form":"snapshot","sections":[{"name":"runtime","text":"\ud800"}]}}"#,
        ),
        (
            "source-model-replay",
            r#"{"id":"m","role":"assistant","content":[{"type":"text","text":"body"}],"source":{"kind":"model","provider":"mock","model":"fixture","replayState":{"nested":["\ud800"]}}}"#,
        ),
    ];
    let mut results = Vec::new();
    for (name, raw) in cases {
        let value = JsonValue::parse(raw.to_owned()).unwrap();
        let decoded = serde_json::from_str::<Message>(raw);
        let source = value.get("source").unwrap().deserialize::<MessageSource>();
        let assistant = value.get("role").unwrap().deserialize::<String>().unwrap() == "assistant";
        let event_type = if assistant {
            "assistant/message"
        } else {
            "user/message"
        };
        let data = if assistant {
            JsonValue::object([
                ("turn", json!(1).into()),
                ("step", json!(1).into()),
                ("message", value.clone()),
            ])
        } else {
            value.clone()
        };
        let options = AppendOptions {
            surface_op: Some(SurfaceOp::append()),
            source_event_seqs: assistant.then(Vec::new),
            ..AppendOptions::default()
        };
        let event = SessionEvent {
            event_type: event_type.into(),
            seq: 0,
            time: 0,
            data: data.clone(),
            surface_op: options.surface_op.clone(),
            source_event_seqs: options.source_event_seqs.clone(),
            ignorable: None,
        };
        let session = Session::create(&SessionId::new(name), None, None).unwrap();
        let appended = session.append_json(event_type, data, options);
        results.push(json!({
            "name":name,"rawMessage":raw,"eventType":event_type,
            "rawRetained":value.as_raw()==raw,
            "messageAccepted":decoded.is_ok(),"messageError":decoded.err().map(|e|e.to_string()),
            "sourceAccepted":source.is_ok(),"sourceError":source.err().map(|e|e.to_string()),
            "appendAccepted":appended.is_ok(),"appendError":appended.err().map(|e|e.to_string()),
            "standaloneProjectionPresent":derive_event_message(&event).is_some(),
        }));
    }
    std::fs::write(
        "/tmp/seekdeep-finish35/message-metadata-native.json",
        serde_json::to_string_pretty(&results).unwrap() + "\n",
    )
    .unwrap();
}
