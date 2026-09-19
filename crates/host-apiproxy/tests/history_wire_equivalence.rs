//! One durable event reaches the browser unchanged when the envelope is built directly.
//!
//! A history page used to serialize each event and parse it back into the wire envelope. The
//! direct builder must produce the same bytes, so these cases compare the two paths.

use seekdeep_core::session::{JsonValue, SessionEvent as DurableEvent, SurfaceOp, SurfaceReplace};
use seekdeep_host_apiproxy::api::sessions::SessionEvent as WireEvent;

fn durable(data: &str) -> DurableEvent {
    DurableEvent {
        event_type: "assistant/chunk".to_owned(),
        seq: 42,
        time: 1_700_000_000_123,
        data: JsonValue::parse(data.to_owned()).expect("valid data"),
        source_event_seqs: None,
        surface_op: None,
        ignorable: None,
    }
}

/// The path this replaces: serialize the durable event, parse the wire envelope back out.
fn round_trip(event: &DurableEvent) -> WireEvent {
    JsonValue::from_serialize(event)
        .expect("durable event serializes")
        .deserialize()
        .expect("wire envelope parses")
}

#[test]
fn building_the_wire_envelope_directly_matches_the_serialize_and_parse_round_trip() {
    let cases = vec![
        durable(r#"{"turn":1,"step":2,"chunk":{"type":"text-delta","index":0,"text":"hi"}}"#),
        durable(r#"{"note":"escaped \"quotes\" and a \\ backslash\u0000"}"#),
        durable(r#"{"big":9007199254740993,"float":1.5,"null":null,"list":[1,2,3]}"#),
        DurableEvent {
            event_type: "user/message".to_owned(),
            seq: 7,
            time: 0,
            data: JsonValue::parse(r#"{"text":"solo"}"#.to_owned()).expect("valid data"),
            source_event_seqs: Some(vec![0, 1, 2]),
            surface_op: Some(SurfaceOp::Marker("append".to_owned())),
            ignorable: Some(true),
        },
        DurableEvent {
            event_type: "assistant/message".to_owned(),
            seq: 9,
            time: 1_700_000_000_000,
            data: JsonValue::parse(r#"{"message":{"id":"m"}}"#.to_owned()).expect("valid data"),
            source_event_seqs: Some(Vec::new()),
            surface_op: Some(SurfaceOp::Replace(SurfaceReplace {
                op: "replace".to_owned(),
                start: 3,
                end: 5,
            })),
            ignorable: Some(false),
        },
    ];
    for event in &cases {
        let direct = WireEvent::from_durable(event).expect("direct envelope");
        assert_eq!(direct, round_trip(event), "{:?}", event.event_type);
        assert_eq!(
            JsonValue::from_serialize(&direct).expect("direct serializes"),
            JsonValue::from_serialize(&round_trip(event)).expect("round trip serializes"),
            "wire text for {:?}",
            event.event_type
        );
    }
}

#[test]
fn a_negative_timestamp_survives_the_direct_envelope() {
    let mut event = durable(r#"{"text":"past"}"#);
    event.time = -1;
    let direct = WireEvent::from_durable(&event).expect("direct envelope");
    assert_eq!(direct, round_trip(&event));
    // Bit equality: a negative epoch millisecond must not change value or sign.
    assert_eq!(direct.time.to_bits(), (-1.0_f64).to_bits());
}
