//! Replay and live-dispatch parity for the package-owned durable-reading invariant.

use std::sync::Arc;

use seekdeep_cordis::{Context, EventArgs};
use seekdeep_core::{
    session::{AppendOptions, Session, SessionEvent, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_llm::{ContentBlock, ContextSnapshotSection, MessageSource, UserMessage};
use seekdeep_time_context::{invariant::register_invariant, user_rpc_message};
use serde_json::{Value, json};

const SECOND: i64 = 1_783_987_200_000;

async fn setup() -> (Context, Arc<SessionStore>) {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let registry =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("invariants");
    let registration = register_invariant(&registry).expect("registration");
    registration.await_ready().await.expect("invariant ready");
    (context, sessions)
}

fn raw_event(event_type: &str, seq: u64, time: i64, data: Value, surface: bool) -> SessionEvent {
    SessionEvent {
        event_type: event_type.to_owned(),
        seq,
        time,
        data,
        source_event_seqs: None,
        surface_op: surface.then(SurfaceOp::append),
        ignorable: None,
    }
}

fn push(events: &mut Vec<SessionEvent>, event_type: &str, data: Value, surface: bool) {
    events.push(raw_event(
        event_type,
        u64::try_from(events.len()).expect("event count fits u64"),
        SECOND,
        data,
        surface,
    ));
}

fn user(text: &str) -> UserMessage {
    UserMessage::new(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        MessageSource::user(),
    )
}

fn preparing(turn: u64, step: u64, browser_zone: Option<&str>) -> Arc<Session> {
    let mut events = Vec::new();
    for prior_turn in 1..turn {
        push(
            &mut events,
            "turn/start",
            json!({"turn": prior_turn}),
            false,
        );
        push(
            &mut events,
            "turn/end",
            json!({"turn": prior_turn, "reason": {"kind": "completed"}}),
            false,
        );
    }
    push(&mut events, "turn/start", json!({"turn": turn}), false);
    let text = format!("turn {turn}");
    let message = if let Some(zone) = browser_zone {
        user_rpc_message(&text, &format!("turn-{turn}"), zone)
    } else {
        user(&text)
    };
    push(
        &mut events,
        "user/message",
        serde_json::to_value(message).expect("user message"),
        true,
    );
    for prior_step in 1..step {
        push(
            &mut events,
            "step/start",
            json!({"turn": turn, "step": prior_step}),
            false,
        );
        push(
            &mut events,
            "step/end",
            json!({"turn": turn, "step": prior_step}),
            false,
        );
    }
    push(
        &mut events,
        "step/start",
        json!({"turn": turn, "step": step}),
        false,
    );
    Session::create(
        &SessionId::new(format!("time-invariant-{turn}-{step}")),
        Some(events),
        None,
    )
    .expect("preparing session")
}

fn reading(turn: &str, step: &str, baseline: &str, timestamp: &str, browser: &str) -> String {
    format!(
        "Time sampled while preparing turn {turn}, step {step}: {timestamp}\n{browser}\nElapsed since the preceding {baseline}: unavailable."
    )
}

fn default_reading() -> String {
    reading(
        "1",
        "1",
        "model-visible message",
        "2026-07-14T00:00:00+00:00[UTC]",
        "Browser time zone for this request: unavailable. Ask the user to clarify otherwise-unqualified dates and times.",
    )
}

fn reading_event(text: &str, time: i64) -> SessionEvent {
    let mut source = MessageSource::plugin("time-context");
    source
        .fields
        .insert("form".to_owned(), Value::String("snapshot".to_owned()));
    source.fields.insert(
        "sections".to_owned(),
        serde_json::to_value([ContextSnapshotSection {
            name: "time-context".to_owned(),
            text: text.to_owned(),
        }])
        .expect("sections"),
    );
    raw_event(
        "user/message",
        0,
        time,
        serde_json::to_value(UserMessage::new(
            vec![ContentBlock::Text {
                text: text.to_owned(),
            }],
            source,
        ))
        .expect("reading"),
        false,
    )
}

fn emit(context: &Context, session: Arc<Session>, event: SessionEvent) -> anyhow::Result<()> {
    context.events().emit(
        context,
        "session/event",
        &EventArgs::from_values(vec![session, Arc::new(event)]),
    )
}

fn message(error: &anyhow::Error) -> String {
    format!("{error:#}")
}

#[tokio::test]
async fn accepts_coherent_readings_and_long_process_pauses() {
    let (context, _) = setup().await;
    let coherent = concat!(
        "Time sampled while preparing turn 2, step 3: 2026-07-14T00:00:00+00:00[UTC]\n",
        "Browser time zone for this request: unavailable. Ask the user to clarify otherwise-unqualified dates and times.\n",
        "Elapsed since the preceding step context: 4m 2s."
    );
    emit(
        &context,
        preparing(2, 3, None),
        reading_event(coherent, SECOND + 456),
    )
    .expect("coherent reading");
    emit(
        &context,
        preparing(1, 1, None),
        reading_event(&default_reading(), SECOND + 60_000),
    )
    .expect("long pause does not invalidate sampling");
}

#[tokio::test]
async fn browser_policy_and_timestamp_must_match_request_provenance() {
    let (context, _) = setup().await;
    let policy = "Browser time zone for this request: Asia/Shanghai. Interpret otherwise-unqualified dates and times in this zone.";
    let valid = reading(
        "1",
        "1",
        "model-visible message",
        "2026-07-14T08:00:00+08:00[Asia/Shanghai]",
        policy,
    );
    emit(
        &context,
        preparing(1, 1, Some("Asia/Shanghai")),
        reading_event(&valid, SECOND + 456),
    )
    .expect("matching browser zone");

    let wrong_policy = emit(
        &context,
        preparing(1, 1, Some("Asia/Shanghai")),
        reading_event(&default_reading(), SECOND + 456),
    )
    .expect_err("wrong browser policy");
    assert!(message(&wrong_policy).contains("browser-zone text"));

    let wrong_timestamp = reading(
        "1",
        "1",
        "model-visible message",
        "2026-07-14T00:00:00+00:00[UTC]",
        policy,
    );
    let error = emit(
        &context,
        preparing(1, 1, Some("Asia/Shanghai")),
        reading_event(&wrong_timestamp, SECOND + 456),
    )
    .expect_err("wrong zone rendering");
    assert!(message(&error).contains("rendered timestamp does not match the unique browser zone"));
}

#[tokio::test]
async fn invalid_or_mixed_corrupt_browser_provenance_is_rejected_first() {
    let (context, _) = setup().await;
    let invalid_zone = "Not/A_Real_Zone";
    let policy = format!(
        "Browser time zone for this request: {invalid_zone}. Interpret otherwise-unqualified dates and times in this zone."
    );
    let invalid = reading(
        "1",
        "1",
        "model-visible message",
        &format!("2026-07-14T00:00:00+00:00[{invalid_zone}]"),
        &policy,
    );
    let error = emit(
        &context,
        preparing(1, 1, Some(invalid_zone)),
        reading_event(&invalid, SECOND + 456),
    )
    .expect_err("invalid browser zone");
    assert!(message(&error).contains("browser time zone is unsupported"));

    let session = preparing(1, 1, Some("Asia/Shanghai"));
    let mut invalid_source = MessageSource::user();
    invalid_source.fields.insert(
        "rpcId".to_owned(),
        Value::String("turn-1-invalid".to_owned()),
    );
    invalid_source.fields.insert(
        "clientTimeZone".to_owned(),
        Value::String(invalid_zone.to_owned()),
    );
    let invalid_message = UserMessage::new(
        vec![ContentBlock::Text {
            text: "second browser prompt".to_owned(),
        }],
        invalid_source,
    );
    session
        .append(
            "user/message",
            serde_json::to_value(invalid_message).expect("invalid message"),
            seekdeep_core::session::AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..seekdeep_core::session::AppendOptions::default()
            },
        )
        .expect("append prompt");
    let mixed = reading(
        "1",
        "1",
        "model-visible message",
        "2026-07-14T00:00:00+00:00[UTC]",
        "Browser time zone for this request: mixed [\"Asia/Shanghai\",\"Not/A_Real_Zone\"]. Ask the user to clarify otherwise-unqualified dates and times.",
    );
    let error = emit(&context, session, reading_event(&mixed, SECOND + 456))
        .expect_err("one corrupt zone invalidates mixed provenance");
    assert!(message(&error).contains("browser time zone is unsupported"));
}

#[tokio::test]
async fn late_registration_replays_each_reading_against_its_prefix() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).expect("sessions");
    let mut valid_seed = preparing(1, 1, None).events();
    let mut valid = reading_event(&default_reading(), SECOND + 456);
    valid.seq = u64::try_from(valid_seed.len()).expect("length");
    valid.surface_op = Some(SurfaceOp::append());
    valid_seed.push(valid);
    sessions
        .create(
            &context,
            Some(SessionId::new("time-invariant-late-valid")),
            CreateSessionOptions {
                seed: Some(valid_seed),
                ..CreateSessionOptions::default()
            },
        )
        .expect("valid seeded session");
    let registry =
        InvariantRegistry::install(&context, &InvariantConfig::default()).expect("registry");
    let valid_registration = register_invariant(&registry).expect("registration");
    valid_registration
        .await_ready()
        .await
        .expect("valid replay");
    valid_registration.dispose().await.expect("release name");

    let mut invalid_seed = preparing(1, 1, None).events();
    let invalid_text = reading(
        "1",
        "2",
        "step context",
        "2026-07-14T00:00:00+00:00[UTC]",
        "Browser time zone for this request: unavailable. Ask the user to clarify otherwise-unqualified dates and times.",
    );
    let mut invalid = reading_event(&invalid_text, SECOND + 456);
    invalid.seq = u64::try_from(invalid_seed.len()).expect("length");
    invalid.surface_op = Some(SurfaceOp::append());
    invalid_seed.push(invalid);
    sessions
        .create(
            &context,
            Some(SessionId::new("time-invariant-late-invalid")),
            CreateSessionOptions {
                seed: Some(invalid_seed),
                ..CreateSessionOptions::default()
            },
        )
        .expect("invalid seed enters before companion");
    let invalid_registration = register_invariant(&registry).expect("second registration");
    let error = invalid_registration
        .await_ready()
        .await
        .expect_err("invalid replay");
    assert!(message(&error).contains("expected turn 1/step 1"));
}

#[tokio::test]
async fn named_position_must_equal_the_open_turn_and_step() {
    let (context, _) = setup().await;
    for (turn, step, expected) in [
        ("1", "3", "expected turn 2/step 3"),
        ("2", "2", "expected turn 2/step 3"),
    ] {
        let text = reading(
            turn,
            step,
            "step context",
            "2026-07-14T00:00:00+00:00[UTC]",
            "Browser time zone for this request: unavailable. Ask the user to clarify otherwise-unqualified dates and times.",
        );
        let error = emit(
            &context,
            preparing(2, 3, None),
            reading_event(&text, SECOND + 456),
        )
        .expect_err("position mismatch");
        assert!(message(&error).contains(expected));
    }
}

#[tokio::test]
async fn reading_requires_an_open_pre_request_step() {
    let (context, _) = setup().await;
    let closed = preparing(1, 2, None);
    closed
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "aborted", "reason": {"kind": "user"}}}),
            AppendOptions::default(),
        )
        .expect("close turn");
    let error = emit(
        &context,
        closed,
        reading_event(&reading("1", "2", "step context", "2026-07-14T00:00:00+00:00[UTC]", "Browser time zone for this request: unavailable. Ask the user to clarify otherwise-unqualified dates and times."), SECOND + 456),
    )
    .expect_err("closed turn");
    assert!(message(&error).contains("inside an open turn"));

    let ended_step = preparing(1, 1, None);
    ended_step
        .append(
            "step/end",
            json!({"turn": 1, "step": 1}),
            AppendOptions::default(),
        )
        .expect("end step");
    let error = emit(
        &context,
        ended_step,
        reading_event(&default_reading(), SECOND + 456),
    )
    .expect_err("ended step");
    assert!(message(&error).contains("follow step/start"));

    let turn_only = Session::create(
        &SessionId::new("time-invariant-turn-only"),
        Some(vec![raw_event(
            "turn/start",
            0,
            SECOND,
            json!({"turn": 1}),
            false,
        )]),
        None,
    )
    .expect("turn only");
    let error = emit(
        &context,
        turn_only,
        reading_event(&default_reading(), SECOND + 456),
    )
    .expect_err("no step");
    assert!(message(&error).contains("follow step/start"));

    let empty = Session::create(&SessionId::new("time-invariant-empty"), None, None)
        .expect("empty session");
    let error = emit(
        &context,
        empty,
        reading_event(&default_reading(), SECOND + 456),
    )
    .expect_err("no turn");
    assert!(message(&error).contains("inside an open turn"));

    let requested = preparing(1, 1, None);
    requested
        .append(
            "request/header",
            json!({"header": {"config": {"provider": "mock", "model": "model"}}, "reason": "initial"}),
            AppendOptions::default(),
        )
        .expect("request header");
    let error = emit(
        &context,
        requested,
        reading_event(&default_reading(), SECOND + 456),
    )
    .expect_err("request already started");
    assert!(message(&error).contains("precede request/header"));
}

#[tokio::test]
async fn malformed_reading_shapes_numbers_baselines_and_times_are_rejected() {
    let (context, _) = setup().await;
    let policy = "Browser time zone for this request: unavailable. Ask the user to clarify otherwise-unqualified dates and times.";
    let cases = [
        ("not a reading".to_owned(), SECOND, "durable reading format"),
        (
            reading(
                "0",
                "1",
                "model-visible message",
                "2026-07-14T00:00:00+00:00[UTC]",
                policy,
            ),
            SECOND,
            "positive safe integers",
        ),
        (
            reading(
                "999999999999999999999",
                "1",
                "model-visible message",
                "2026-07-14T00:00:00+00:00[UTC]",
                policy,
            ),
            SECOND,
            "positive safe integers",
        ),
        (
            reading(
                "1",
                "0",
                "step context",
                "2026-07-14T00:00:00+00:00[UTC]",
                policy,
            ),
            SECOND,
            "positive safe integers",
        ),
        (
            reading(
                "1",
                "999999999999999999999",
                "step context",
                "2026-07-14T00:00:00+00:00[UTC]",
                policy,
            ),
            SECOND,
            "positive safe integers",
        ),
        (
            reading(
                "1",
                "1",
                "step context",
                "2026-07-14T00:00:00+00:00[UTC]",
                policy,
            ),
            SECOND,
            "wrong elapsed-time baseline",
        ),
        (
            reading(
                "1",
                "2",
                "model-visible message",
                "2026-07-14T00:00:00+00:00[UTC]",
                policy,
            ),
            SECOND,
            "wrong elapsed-time baseline",
        ),
        (
            reading(
                "1",
                "1",
                "model-visible message",
                "2026-99-99T00:00:00+00:00[UTC]",
                policy,
            ),
            SECOND,
            "must parse and not postdate",
        ),
        (default_reading(), i64::MAX, "must parse and not postdate"),
        (default_reading(), SECOND - 1, "must parse and not postdate"),
    ];
    for (text, time, expected) in cases {
        let step = u64::from(text.contains("turn 1, step 2:")) + 1;
        let error = emit(
            &context,
            preparing(1, step, None),
            reading_event(&text, time),
        )
        .expect_err("incoherent reading");
        assert!(message(&error).contains(expected), "expected {expected}");
    }
}

#[tokio::test]
async fn content_must_be_exactly_one_plain_text_block() {
    let (context, _) = setup().await;
    let mut cases = vec![
        json!([]),
        json!([{"type": "image", "data": "x", "mimeType": "image/png"}]),
        json!([{"type": "text", "text": "one"}, {"type": "text", "text": "two"}]),
        json!([{"type": "text", "text": default_reading(), "extra": true}]),
    ];
    for content in cases.drain(..) {
        let mut candidate = reading_event(&default_reading(), SECOND + 456);
        candidate.data["content"] = content;
        let error =
            emit(&context, preparing(1, 1, None), candidate).expect_err("malformed content");
        assert!(message(&error).contains("exactly one text block"));
    }
}

#[tokio::test]
async fn snapshot_provenance_is_exact_and_carries_no_request_authority() {
    let (context, _) = setup().await;
    let base = reading_event(&default_reading(), SECOND + 456);
    let sources = vec![
        json!({"kind": "plugin", "plugin": "time-context"}),
        json!({"kind": "plugin", "plugin": "time-context", "form": "snapshot", "sections": [{"name": "time-context", "text": default_reading()}], "authority": {}}),
        json!({"kind": "plugin", "plugin": "time-context", "form": "snapshot", "sections": [{"name": "time-context", "text": "different"}]}),
        json!({"kind": "plugin", "plugin": "time-context", "form": "snapshot", "sections": {"0": {"name": "time-context", "text": default_reading()}, "length": 1}}),
        json!({"kind": "plugin", "plugin": "time-context", "form": "snapshot", "sections": [{"name": "time-context", "text": default_reading(), "extra": true}]}),
    ];
    for source in sources {
        let mut malformed = base.clone();
        malformed.data["source"] = source;
        let error =
            emit(&context, preparing(1, 1, None), malformed).expect_err("malformed provenance");
        assert!(message(&error).contains("must carry only the exact snapshot text"));
    }
}

#[tokio::test]
async fn seeded_session_created_after_registration_is_validated_and_rolled_back() {
    let (context, sessions) = setup().await;
    let mut seed = preparing(1, 1, None).events();
    let invalid_text = reading(
        "1",
        "2",
        "step context",
        "2026-07-14T00:00:00+00:00[UTC]",
        "Browser time zone for this request: unavailable. Ask the user to clarify otherwise-unqualified dates and times.",
    );
    let mut invalid = reading_event(&invalid_text, SECOND + 456);
    invalid.seq = u64::try_from(seed.len()).expect("length");
    invalid.surface_op = Some(SurfaceOp::append());
    seed.push(invalid);
    let error = sessions
        .create(
            &context,
            Some(SessionId::new("time-invariant-created-invalid")),
            CreateSessionOptions {
                seed: Some(seed),
                ..CreateSessionOptions::default()
            },
        )
        .expect_err("creation invariant");
    assert!(format!("{error:#}").contains("expected turn 1/step 1"));
    assert!(
        sessions
            .get(&SessionId::new("time-invariant-created-invalid"))
            .is_none()
    );
}

#[tokio::test]
async fn unrelated_events_and_other_plugin_messages_are_ignored() {
    let (context, _) = setup().await;
    let mut other = reading_event("unrelated", SECOND + 456);
    other.data["source"] = json!({"kind": "plugin", "plugin": "other"});
    emit(&context, preparing(1, 1, None), other).expect("other plugin");

    let user_event = raw_event(
        "user/message",
        0,
        SECOND + 456,
        serde_json::to_value(user("unrelated")).expect("user event"),
        false,
    );
    emit(&context, preparing(1, 1, None), user_event).expect("human message");
    emit(
        &context,
        preparing(1, 1, None),
        raw_event("turn/start", 0, 0, json!({"turn": 1}), false),
    )
    .expect("non-message event");
    context
        .events()
        .emit(&context, "tools/change", &EventArgs::new())
        .expect("unrelated event name");
}
