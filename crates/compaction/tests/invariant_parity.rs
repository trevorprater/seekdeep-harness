//! Behavioral mirror of the compaction invariant source suite.

use std::sync::Arc;

use seekdeep_commands::CommandId;
use seekdeep_compaction::{CompactionId, compact_checkpoint_source};
use seekdeep_cordis::{Context, EventArgs};
use seekdeep_core::{
    session::{AppendOptions, Session, SessionEvent, SessionId, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_invariants::{InvariantConfig, InvariantRegistry};
use seekdeep_llm::{ContentBlock, Message, MessageSource};
use serde_json::{Value, json};

const ID: &str = "test-compaction";
const NEXT_ID: &str = "next-test-compaction";
const COMMAND: &str = "test-command";
const NEXT_COMMAND: &str = "next-test-command";

async fn register(context: &Context) -> anyhow::Result<()> {
    let registry = InvariantRegistry::install(context, &InvariantConfig::default())?;
    seekdeep_compaction::invariant::register_invariant(&registry)?
        .await_ready()
        .await
}

async fn setup() -> (Context, Arc<SessionStore>) {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    register(&context).await.unwrap();
    (context, sessions)
}

fn create(context: &Context, sessions: &SessionStore, id: &str) -> Arc<Session> {
    sessions
        .create(
            context,
            Some(SessionId::new(id)),
            CreateSessionOptions::default(),
        )
        .unwrap()
}

fn start_turn(session: &Session, turn: u64) {
    session
        .append(
            "turn/start",
            json!({"turn": turn}),
            AppendOptions::default(),
        )
        .unwrap();
}

fn end_turn(session: &Session, turn: u64) -> anyhow::Result<()> {
    session
        .append(
            "turn/end",
            json!({"turn": turn, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .map(|_| ())
        .map_err(Into::into)
}

fn start(
    session: &Session,
    id: &str,
    turn: Option<u64>,
    command: Option<&str>,
) -> anyhow::Result<()> {
    let mut data = serde_json::Map::from_iter([
        ("compactionId".to_owned(), json!(id)),
        ("turn".to_owned(), turn.map_or(Value::Null, Value::from)),
    ]);
    if let Some(command) = command {
        data.insert("sourceCommandId".to_owned(), json!(command));
    }
    session
        .append(
            "compaction/start",
            Value::Object(data),
            AppendOptions::default(),
        )
        .map(|_| ())
        .map_err(Into::into)
}

fn summary(overrides: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    let mut value = json!({
        "compactionId": ID,
        "summary": [{"type": "text", "text": "short"}],
        "shadowedRange": {"start": 2, "end": 4},
        "shadowedSeqs": [2, 3, 4],
        "shadowedTokenCount": 12,
        "provider": "mock",
        "model": "mock"
    });
    let object = value.as_object_mut().unwrap();
    for (key, value) in overrides {
        object.insert(key.to_owned(), value);
    }
    value
}

fn append_summary(session: &Session, value: Value) -> anyhow::Result<()> {
    session
        .append("compaction/summary", value, AppendOptions::default())
        .map(|_| ())
        .map_err(Into::into)
}

fn end(
    session: &Session,
    id: &str,
    turn: Option<u64>,
    command: Option<&str>,
    error: Option<&str>,
) -> anyhow::Result<()> {
    let mut data = serde_json::Map::from_iter([
        ("compactionId".to_owned(), json!(id)),
        ("turn".to_owned(), turn.map_or(Value::Null, Value::from)),
    ]);
    if let Some(command) = command {
        data.insert("sourceCommandId".to_owned(), json!(command));
    }
    if let Some(error) = error {
        data.insert("error".to_owned(), json!(error));
    }
    session
        .append(
            "compaction/end",
            Value::Object(data),
            AppendOptions::default(),
        )
        .map(|_| ())
        .map_err(Into::into)
}

fn assert_error(result: anyhow::Result<()>, fragment: &str) {
    let error = result.unwrap_err().to_string();
    assert!(
        error.contains(fragment),
        "expected {fragment:?} in {error:?}"
    );
}

#[tokio::test]
async fn accepts_numbered_and_standalone_success_failure_and_late_registration() {
    let (context, sessions) = setup().await;
    let success = create(&context, &sessions, "numbered-success");
    start_turn(&success, 1);
    start(&success, ID, Some(1), None).unwrap();
    append_summary(&success, summary([])).unwrap();
    end(&success, ID, Some(1), None, None).unwrap();
    let failed = create(&context, &sessions, "numbered-failed");
    start_turn(&failed, 2);
    start(&failed, ID, Some(2), None).unwrap();
    end(&failed, ID, Some(2), None, Some("provider failed")).unwrap();

    let standalone = create(&context, &sessions, "standalone-success");
    start(&standalone, ID, None, None).unwrap();
    append_summary(&standalone, summary([])).unwrap();
    end(&standalone, ID, None, None, None).unwrap();
    let standalone_failed = create(&context, &sessions, "standalone-failed");
    start(&standalone_failed, ID, None, None).unwrap();
    end(&standalone_failed, ID, None, None, Some("provider failed")).unwrap();

    let late_context = Context::new();
    let late_sessions = SessionStore::install(&late_context).unwrap();
    let late = create(&late_context, &late_sessions, "late-open");
    start_turn(&late, 1);
    start(&late, ID, Some(1), None).unwrap();
    register(&late_context).await.unwrap();
    end(&late, ID, Some(1), None, Some("resume failed")).unwrap();
    end_turn(&late, 1).unwrap();
}

#[tokio::test]
async fn end_seed_clears_inherited_orphans_but_rejects_closed_cross_turn_brackets() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let standalone_source =
        Session::create(&SessionId::new("standalone-source"), None, None).unwrap();
    start(&standalone_source, ID, None, None).unwrap();
    let replayed = sessions
        .create(
            &context,
            Some(SessionId::new("standalone-replayed")),
            CreateSessionOptions {
                seed: Some(standalone_source.events()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        replayed
            .events()
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["compaction/start", "session/end-seed"]
    );
    register(&context).await.unwrap();
    start(&replayed, NEXT_ID, None, None).unwrap();
    end(&replayed, NEXT_ID, None, None, Some("new failed")).unwrap();

    let numbered_context = Context::new();
    let numbered_sessions = SessionStore::install(&numbered_context).unwrap();
    let numbered_source = Session::create(&SessionId::new("numbered-source"), None, None).unwrap();
    start_turn(&numbered_source, 1);
    start(&numbered_source, ID, Some(1), None).unwrap();
    let numbered = numbered_sessions
        .create(
            &numbered_context,
            Some(SessionId::new("numbered-replayed")),
            CreateSessionOptions {
                seed: Some(numbered_source.events()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        numbered
            .events()
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["turn/start", "compaction/start", "session/end-seed"]
    );
    register(&numbered_context).await.unwrap();
    end_turn(&numbered, 1).unwrap();

    let bad_context = Context::new();
    let bad_sessions = SessionStore::install(&bad_context).unwrap();
    let bad_source = Session::create(&SessionId::new("bad-source"), None, None).unwrap();
    start(&bad_source, ID, None, None).unwrap();
    start_turn(&bad_source, 1);
    end_turn(&bad_source, 1).unwrap();
    end(&bad_source, ID, None, None, Some("crossed turn")).unwrap();
    bad_sessions
        .create(
            &bad_context,
            Some(SessionId::new("bad-replayed")),
            CreateSessionOptions {
                seed: Some(bad_source.events()),
                ..Default::default()
            },
        )
        .unwrap();
    let error = register(&bad_context).await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("turn/start cannot cross an open standalone compaction")
    );
}

#[tokio::test]
async fn inherited_repair_boundaries_before_end_seed_clear_a_standalone_orphan() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let source = Session::create(&SessionId::new("repaired-source"), None, None).unwrap();
    start(&source, ID, None, None).unwrap();
    start_turn(&source, 1);
    end_turn(&source, 1).unwrap();
    let replayed = sessions
        .create(
            &context,
            Some(SessionId::new("repaired-replayed")),
            CreateSessionOptions {
                seed: Some(source.events()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        replayed
            .events()
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        [
            "compaction/start",
            "turn/start",
            "turn/end",
            "session/end-seed"
        ]
    );

    register(&context).await.unwrap();
    start_turn(&replayed, 2);
    end_turn(&replayed, 2).unwrap();
}

#[tokio::test]
async fn late_registration_rejects_an_unenclosed_existing_compaction_event() {
    let context = Context::new();
    let sessions = SessionStore::install(&context).unwrap();
    let session = create(&context, &sessions, "unenclosed-existing");
    start_turn(&session, 1);
    end_turn(&session, 1).unwrap();
    start(&session, ID, Some(1), None).unwrap();

    let error = register(&context).await.unwrap_err();
    assert!(error.to_string().contains("outside any open turn"));
}

#[tokio::test]
async fn adopts_a_bare_session_and_ignores_unrelated_committed_events() {
    let (context, _sessions) = setup().await;
    let session = Session::create(&SessionId::new("bare-compaction-session"), None, None).unwrap();
    let events = [
        SessionEvent {
            event_type: "turn/start".to_owned(),
            seq: 0,
            time: 0,
            data: json!({"turn": 1}),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        },
        SessionEvent {
            event_type: "step/start".to_owned(),
            seq: 1,
            time: 1,
            data: json!({"turn": 1, "step": 1}),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        },
        SessionEvent {
            event_type: "compaction/start".to_owned(),
            seq: 2,
            time: 2,
            data: json!({"compactionId": ID, "turn": 1}),
            source_event_seqs: None,
            surface_op: None,
            ignorable: None,
        },
    ];
    for event in events {
        context
            .events()
            .emit(
                &context,
                "session/event",
                &EventArgs::from_values(vec![session.clone(), Arc::new(event)]),
            )
            .unwrap();
    }
}

#[tokio::test]
async fn rejects_wrong_owners_nested_brackets_and_crossing_turn_boundaries() {
    let (context, sessions) = setup().await;
    let session = create(&context, &sessions, "wrong-owner");
    assert_error(start(&session, ID, Some(1), None), "outside any open turn");
    start_turn(&session, 1);
    assert_error(start(&session, ID, Some(2), None), "open turn is 1");
    assert_error(start(&session, ID, None, None), "turn 1 is open");
    start(&session, ID, Some(1), None).unwrap();
    assert_error(start(&session, NEXT_ID, Some(1), None), "still compacting");
    assert_error(end_turn(&session, 1), "cannot cross an open compaction");
    end(&session, ID, Some(1), None, Some("cancelled")).unwrap();
    end_turn(&session, 1).unwrap();

    let standalone = create(&context, &sessions, "nested-standalone");
    start(&standalone, ID, None, None).unwrap();
    assert_error(
        start(&standalone, NEXT_ID, None, None),
        "standalone compaction is still compacting",
    );
    let turn_error = standalone
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .unwrap_err();
    assert!(turn_error.to_string().contains("cannot cross"));
}

fn append_user(session: &Session, text: &str, source: MessageSource) -> u64 {
    let message = Message::user(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        source,
    );
    session
        .append(
            "user/message",
            serde_json::to_value(message).unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..Default::default()
            },
        )
        .unwrap()
        .seq
}

#[tokio::test]
async fn checkpoint_requires_matching_open_transaction_and_nonempty_command_identity() {
    let (context, sessions) = setup().await;
    let session = create(&context, &sessions, "checkpoint-mismatch");
    let original = append_user(&session, "original", MessageSource::user());
    start_turn(&session, 1);
    start(&session, ID, Some(1), None).unwrap();
    append_summary(&session, summary([])).unwrap();
    let checkpoint = Message::user(
        vec![ContentBlock::Text {
            text: "checkpoint".to_owned(),
        }],
        compact_checkpoint_source(&CompactionId::new(NEXT_ID), None),
    );
    let error = session
        .append(
            "user/message",
            serde_json::to_value(checkpoint).unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::replace(original, original)),
                source_event_seqs: Some(vec![original]),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("does not match compaction/start id")
    );

    let without = create(&context, &sessions, "checkpoint-without-start");
    let original = append_user(&without, "original", MessageSource::user());
    let checkpoint = Message::user(
        vec![ContentBlock::Text {
            text: "checkpoint".to_owned(),
        }],
        compact_checkpoint_source(&CompactionId::new(ID), None),
    );
    let error = without
        .append(
            "user/message",
            serde_json::to_value(checkpoint).unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::replace(original, original)),
                source_event_seqs: Some(vec![original]),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("no matching compaction/start"));

    let empty = create(&context, &sessions, "checkpoint-empty-command");
    let original = append_user(&empty, "original", MessageSource::user());
    start_turn(&empty, 1);
    start(&empty, ID, Some(1), None).unwrap();
    let checkpoint = Message::user(
        vec![ContentBlock::Text {
            text: "checkpoint".to_owned(),
        }],
        compact_checkpoint_source(&CompactionId::new(ID), Some(&CommandId::new(""))),
    );
    let error = empty
        .append(
            "user/message",
            serde_json::to_value(checkpoint).unwrap(),
            AppendOptions {
                surface_op: Some(SurfaceOp::replace(original, original)),
                source_event_seqs: Some(vec![original]),
                ..Default::default()
            },
        )
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("sourceCommandId must be a non-empty string")
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // one table mirrors the source invalid lifecycle inventory
async fn rejects_every_malformed_lifecycle_shape_before_commit() {
    let (context, sessions) = setup().await;
    let cases = [
        (
            "empty-start",
            0,
            "compaction/start compactionId must be a non-empty string",
        ),
        (
            "empty-start-command",
            1,
            "compaction/start sourceCommandId must be a non-empty string",
        ),
        ("summary-without-start", 2, "no matching compaction/start"),
        ("nested-start", 3, "still compacting"),
        ("repeated-summary", 4, "repeated within one compaction"),
        ("summary-other-id", 5, "does not match compaction/start id"),
        (
            "summary-other-command",
            6,
            "does not match compaction/start sourceCommandId",
        ),
        ("empty-shadow", 7, "shadowedSeqs must be non-empty"),
        ("wrong-endpoints", 8, "shadowedRange must match"),
        ("invalid-token-count", 9, "non-negative safe integer"),
        ("end-without-start", 10, "no matching compaction/start"),
        ("wrong-end-turn", 11, "does not match"),
        ("end-other-id", 12, "does not match compaction/start id"),
        (
            "end-missing-command",
            13,
            "does not match compaction/start sourceCommandId",
        ),
        (
            "empty-end-command",
            14,
            "compaction/end sourceCommandId must be a non-empty string",
        ),
        (
            "success-without-summary",
            15,
            "requires one compaction/summary",
        ),
    ];
    for (name, kind, fragment) in cases {
        let session = create(&context, &sessions, name);
        start_turn(&session, 1);
        let result = match kind {
            0 => start(&session, "", Some(1), None),
            1 => start(&session, ID, Some(1), Some("")),
            2 => append_summary(&session, summary([])),
            3 => start(&session, ID, Some(1), None)
                .and_then(|()| start(&session, NEXT_ID, Some(2), None)),
            4 => start(&session, ID, Some(1), None)
                .and_then(|()| append_summary(&session, summary([])))
                .and_then(|()| append_summary(&session, summary([]))),
            5 => start(&session, ID, Some(1), None).and_then(|()| {
                append_summary(&session, summary([("compactionId", json!(NEXT_ID))]))
            }),
            6 => start(&session, ID, Some(1), Some(COMMAND)).and_then(|()| {
                append_summary(
                    &session,
                    summary([("sourceCommandId", json!(NEXT_COMMAND))]),
                )
            }),
            7 => start(&session, ID, Some(1), None)
                .and_then(|()| append_summary(&session, summary([("shadowedSeqs", json!([]))]))),
            8 => start(&session, ID, Some(1), None).and_then(|()| {
                append_summary(
                    &session,
                    summary([("shadowedRange", json!({"start": 1, "end": 4}))]),
                )
            }),
            9 => start(&session, ID, Some(1), None).and_then(|()| {
                append_summary(&session, summary([("shadowedTokenCount", json!(-1))]))
            }),
            10 => end(&session, ID, Some(1), None, Some("failed")),
            11 => start(&session, ID, Some(1), None)
                .and_then(|()| end(&session, ID, Some(2), None, Some("failed"))),
            12 => start(&session, ID, Some(1), None)
                .and_then(|()| end(&session, NEXT_ID, Some(1), None, Some("failed"))),
            13 => start(&session, ID, Some(1), Some(COMMAND))
                .and_then(|()| end(&session, ID, Some(1), None, Some("failed"))),
            14 => start(&session, ID, Some(1), Some(COMMAND))
                .and_then(|()| end(&session, ID, Some(1), Some(""), Some("failed"))),
            15 => start(&session, ID, Some(1), None)
                .and_then(|()| end(&session, ID, Some(1), None, None)),
            _ => unreachable!(),
        };
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains(fragment),
            "{name}: expected {fragment:?} in {error:?}"
        );
    }
}
