//! Behavioral mirror of packages/session/session-telemetry/tests/telemetry.spec.ts.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventOptions, EventReply};
use seekdeep_core::{
    session::{AppendOptions, SurfaceOp},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_session_telemetry::{
    SessionTelemetryCapture, SessionTelemetryChannel, SessionTelemetryCoordinator,
    SessionTelemetryRecord, SessionTelemetrySeverity, SessionTelemetrySink,
};
use serde_json::Value;
use serde_json::json;

#[derive(Default)]
struct FakeBackend {
    records: Mutex<Vec<SessionTelemetryRecord>>,
    calls: Mutex<Vec<String>>,
    emit_error: Mutex<Option<String>>,
    reject_seq: Mutex<Option<u64>>,
    shutdown_error: Mutex<Option<String>>,
    flush_calls: AtomicUsize,
}

impl FakeBackend {
    fn ledger(&self) -> Vec<SessionTelemetryRecord> {
        self.records
            .lock()
            .iter()
            .filter(|record| record.channel == SessionTelemetryChannel::Ledger)
            .cloned()
            .collect()
    }
}

#[async_trait]
impl SessionTelemetrySink for FakeBackend {
    #[allow(clippy::manual_assert)]
    fn emit(&self, record: SessionTelemetryRecord) {
        if let Some(message) = self.emit_error.lock().as_ref() {
            panic!("{message}");
        }
        if let Some(seq) = *self.reject_seq.lock()
            && record.attributes.get("event.seq").and_then(Value::as_u64) == Some(seq)
        {
            panic!("backend rejected seq {seq}");
        }
        let label = record
            .attributes
            .get("event.seq")
            .and_then(Value::as_u64)
            .map_or_else(
                || {
                    record
                        .attributes
                        .get("telemetry.op")
                        .map_or("?", |v| v.as_str().unwrap_or("?"))
                },
                |seq| Box::leak(format!("{seq}").into_boxed_str()),
            );
        self.calls.lock().push(format!("emit:{label}"));
        self.records.lock().push(record);
    }

    fn flush(&self) {
        self.flush_calls.fetch_add(1, Ordering::SeqCst);
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        self.calls.lock().push("shutdown".to_owned());
        tokio::time::sleep(Duration::from_millis(5)).await;
        if let Some(message) = self.shutdown_error.lock().as_ref() {
            anyhow::bail!("{message}");
        }
        Ok(())
    }
}

fn user_message(text: &str) -> Value {
    json!({
        "id": "u1",
        "role": "user",
        "content": [{"type": "text", "text": text}],
        "source": {"kind": "user"},
    })
}

fn setup(
    backend: Arc<FakeBackend>,
    capture: SessionTelemetryCapture,
) -> (Context, Arc<FakeBackend>, Arc<SessionTelemetryCoordinator>) {
    let context = Context::new();
    SessionStore::install(&context).expect("sessions");
    let coordinator = SessionTelemetryCoordinator::install(&context, backend.clone(), capture)
        .expect("coordinator");
    (context, backend, coordinator)
}

fn live_session(context: &Context, id: &str) -> Arc<seekdeep_core::session::Session> {
    context
        .get(seekdeep_core::session_store::SESSIONS)
        .expect("sessions")
        .create(
            context,
            Some(seekdeep_core::session::SessionId::new(id)),
            CreateSessionOptions::default(),
        )
        .expect("session")
}

fn append_turn(session: &Arc<seekdeep_core::session::Session>) {
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("turn");
    session
        .append(
            "user/message",
            user_message("hello"),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("user message");
}

#[test]
fn hands_every_appended_event_over_with_envelope_identity_and_cloned_body() {
    let backend = Arc::new(FakeBackend::default());
    let (context, backend, _) = setup(backend.clone(), SessionTelemetryCapture::Live);
    let session = live_session(&context, "cap");
    append_turn(&session);

    let ledger = backend.ledger();
    let start = &ledger[0];
    assert_eq!(start.attributes["session.id"], "cap");
    assert_eq!(start.attributes["event.type"], "turn/start");
    assert_eq!(start.attributes["event.seq"], 0);
    assert_eq!(start.time, session.events()[0].time);
    assert_eq!(start.severity, SessionTelemetrySeverity::Info);

    let message = &ledger[1];
    assert_eq!(message.attributes["event.seq"], 1);
    // Deep-copy isolation: mutating the handed-off body never reaches the log.
    {
        let mut records = backend.records.lock();
        let mut blocks = records[1].body["content"].as_array().unwrap().clone();
        blocks[0].insert("text", json!("tampered").into()).unwrap();
        records[1]
            .body
            .insert("content", seekdeep_core::session::JsonValue::array(&blocks))
            .unwrap();
    }
    let logged = &session.events()[1];
    assert_eq!(logged.data["content"][0]["text"], "hello");
}

#[test]
fn stamps_header_facts_on_every_record_when_present() {
    let backend = Arc::new(FakeBackend::default());
    let (context, backend, _) = setup(backend.clone(), SessionTelemetryCapture::Live);
    let session = context
        .get(seekdeep_core::session_store::SESSIONS)
        .expect("sessions")
        .create(
            &context,
            Some(seekdeep_core::session::SessionId::new("child")),
            CreateSessionOptions {
                cwd: Some("/tmp/proj".to_owned()),
                parent_session: Some(seekdeep_core::session::SessionId::new("parent")),
                ..CreateSessionOptions::default()
            },
        )
        .expect("session");
    append_turn(&session);
    for record in backend.ledger() {
        assert_eq!(record.attributes["session.cwd"], "/tmp/proj");
        assert_eq!(record.attributes["session.parent_id"], "parent");
    }
}

#[test]
fn maps_outcome_flags_to_severity() {
    let backend = Arc::new(FakeBackend::default());
    let (context, backend, _) = setup(backend.clone(), SessionTelemetryCapture::Live);
    let session = live_session(&context, "sev");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("turn");
    let tool_result = |call_id: &str, is_error: bool| {
        json!({
            "turn": 1, "step": 1,
            "message": {
                "id": "m1", "role": "user",
                "content": [{"type": "tool-result", "toolCallId": call_id, "content": [], "isError": is_error}],
                "source": {"kind": "tool", "callId": call_id},
            }
        })
    };
    session
        .append(
            "tool/result",
            tool_result("c1", true),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("error result");
    session
        .append(
            "tool/result",
            tool_result("c2", false),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("ok result");
    session
        .append(
            "telemetry-test/opaque",
            json!({"payload": {"nested": []}}),
            AppendOptions::default(),
        )
        .expect("opaque");
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "error", "error": {"message": "boom", "code": "UNKNOWN"}}}),
            AppendOptions::default(),
        )
        .expect("turn end");

    let severities = backend
        .ledger()
        .iter()
        .map(|record| {
            (
                record.attributes["event.type"].as_str().unwrap().to_owned(),
                record.severity,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        severities,
        vec![
            ("turn/start".to_owned(), SessionTelemetrySeverity::Info),
            ("tool/result".to_owned(), SessionTelemetrySeverity::Error),
            ("tool/result".to_owned(), SessionTelemetrySeverity::Info),
            (
                "telemetry-test/opaque".to_owned(),
                SessionTelemetrySeverity::Info
            ),
            ("turn/end".to_owned(), SessionTelemetrySeverity::Error),
        ]
    );
}

#[test]
fn passes_unknown_merged_event_types_through_unchanged() {
    let backend = Arc::new(FakeBackend::default());
    let (context, backend, _) = setup(backend.clone(), SessionTelemetryCapture::Live);
    let session = live_session(&context, "opaque");
    session
        .append(
            "telemetry-test/opaque",
            json!({"payload": {"nested": ["a", "b"]}}),
            AppendOptions::default(),
        )
        .expect("opaque");
    let record = &backend.ledger()[0];
    assert_eq!(record.attributes["event.type"], "telemetry-test/opaque");
    assert_eq!(record.severity, SessionTelemetrySeverity::Info);
    assert_eq!(record.body, json!({"payload": {"nested": ["a", "b"]}}));
}

#[test]
fn ships_only_the_first_chunk_of_each_turn_step_per_session() {
    let backend = Arc::new(FakeBackend::default());
    let (context, backend, _) = setup(backend.clone(), SessionTelemetryCapture::Live);
    let a = live_session(&context, "a");
    let b = live_session(&context, "b");
    let chunk = |session: &Arc<seekdeep_core::session::Session>,
                 turn: u64,
                 step: u64,
                 text: &str| {
        session
            .append(
                "assistant/chunk",
                json!({"turn": turn, "step": step, "chunk": {"type": "text-delta", "index": 0, "text": text}}),
                AppendOptions::default(),
            )
            .expect("chunk");
    };
    chunk(&a, 1, 1, "a11-first");
    chunk(&a, 1, 1, "a11-second");
    chunk(&a, 1, 2, "a12-first");
    chunk(&b, 1, 1, "b11-first");
    chunk(&b, 1, 1, "b11-second");
    let shipped = backend
        .ledger()
        .iter()
        .map(|record| {
            (
                record.attributes["session.id"].as_str().unwrap().to_owned(),
                record.body["chunk"]["text"].as_str().unwrap().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        shipped,
        vec![
            ("a".to_owned(), "a11-first".to_owned()),
            ("a".to_owned(), "a12-first".to_owned()),
            ("b".to_owned(), "b11-first".to_owned()),
        ]
    );
}

#[test]
fn captures_one_canonical_log_prefix_at_a_time() {
    let backend = Arc::new(FakeBackend::default());
    let (context, backend, coordinator) = setup(backend.clone(), SessionTelemetryCapture::OnDemand);
    let session = live_session(&context, "on-demand-prefix");
    append_turn(&session);
    let first_boundary = session.events()[1].seq;
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("turn end");
    assert!(backend.records.lock().is_empty());

    coordinator.capture_session(&session, Some(first_boundary));
    assert_eq!(
        backend
            .ledger()
            .iter()
            .map(|record| record.attributes["event.type"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        vec!["turn/start".to_owned(), "user/message".to_owned()]
    );

    assert_eq!(backend.ledger().len(), 2);
    coordinator.capture_session(&session, None);
    coordinator.capture_session(&session, None);
    assert_eq!(
        backend
            .ledger()
            .iter()
            .map(|record| record.attributes["event.type"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        vec![
            "turn/start".to_owned(),
            "user/message".to_owned(),
            "turn/end".to_owned(),
        ]
    );
}

#[tokio::test]
async fn runs_the_mounted_redaction_policy_during_canonical_log_capture() {
    let backend = Arc::new(FakeBackend::default());
    let (context, backend, coordinator) = setup(backend.clone(), SessionTelemetryCapture::OnDemand);
    let session = live_session(&context, "on-demand-redacted");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .expect("turn");

    let dispose_rule = context
        .events()
        .on_waterfall(
            &context,
            "session-telemetry/record",
            move |_ctx, _args, next| {
                Box::pin(async move {
                    let reply = next.run().await?;
                    let mut record = reply
                        .downcast::<SessionTelemetryRecord>()
                        .map(|record| (*record).clone())
                        .expect("record");
                    record.body = json!({"scrubbed": true}).into();
                    Ok(EventReply::Value(Arc::new(record)))
                })
            },
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
        )
        .expect("rule");

    coordinator.capture_session(&session, None);
    assert_eq!(backend.ledger()[0].body, json!({"scrubbed": true}));
    dispose_rule.dispose().await.expect("dispose rule");

    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("turn end");
    coordinator.capture_session(&session, None);
    assert_eq!(
        backend.ledger()[1].body,
        json!({"turn": 1, "reason": {"kind": "completed"}})
    );
}

#[test]
fn contains_each_backend_failure_independently_while_replaying_a_prefix() {
    let backend = Arc::new(FakeBackend::default());
    backend.reject_seq.lock().replace(1);
    let (context, backend, coordinator) = setup(backend.clone(), SessionTelemetryCapture::OnDemand);
    let session = live_session(&context, "on-demand-failure");
    append_turn(&session);
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .expect("turn end");

    coordinator.capture_session(&session, None);
    assert_eq!(
        backend
            .ledger()
            .iter()
            .map(|record| record.attributes["event.seq"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        vec![0, 2]
    );
}

/// The source keys its handoff cursor by session in a weak map, so a disposed session's cursor
/// dies with it even when capture is on demand and no adoption listener runs.
#[tokio::test]
async fn on_demand_capture_retires_its_cursor_when_the_session_is_disposed() {
    let backend = Arc::new(FakeBackend::default());
    let (context, backend, coordinator) = setup(backend.clone(), SessionTelemetryCapture::OnDemand);
    let fiber = seekdeep_cordis::Fiber::active_child("session-owner");
    let scoped = context.with_fiber(fiber.clone());
    let session = live_session(&scoped, "on-demand-retired");
    append_turn(&session);
    coordinator.capture_session(&session, None);
    assert_eq!(backend.ledger().len(), 2);
    // A second capture hands nothing over: the cursor already covers the log.
    coordinator.capture_session(&session, None);
    assert_eq!(backend.ledger().len(), 2);

    fiber.dispose().await.expect("dispose the session owner");
    // The disposed session's cursor is gone, so a capture of its retained log starts over
    // instead of inheriting the retired cursor.
    coordinator.capture_session(&session, None);
    assert_eq!(backend.ledger().len(), 4);
    assert!(
        backend
            .records
            .lock()
            .iter()
            .all(|record| record.channel != SessionTelemetryChannel::Ops)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_waits_for_a_session_retirement_record_that_is_being_redacted() {
    let context = Context::new();
    SessionStore::install(&context).unwrap();
    let backend = Arc::new(FakeBackend::default());
    let telemetry_owner = seekdeep_cordis::Fiber::active_child("telemetry-owner");
    SessionTelemetryCoordinator::install(
        &context.with_fiber(telemetry_owner.clone()),
        backend.clone(),
        SessionTelemetryCapture::Live,
    )
    .unwrap();
    let session_owner = seekdeep_cordis::Fiber::active_child("session-owner");
    let _session = live_session(&context.with_fiber(session_owner.clone()), "retiring");
    backend.calls.lock().clear();

    let (entered, retirement_entered) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let held = Arc::new(Mutex::new(Some(released)));
    context
        .events()
        .on_waterfall(
            &context,
            "session-telemetry/record",
            move |_, args, next| {
                let release = args
                    .get::<SessionTelemetryRecord>(0)
                    .filter(|record| record.channel == SessionTelemetryChannel::Ops)
                    .and_then(|_| held.lock().take());
                let entered = entered.clone();
                Box::pin(async move {
                    if let Some(release) = release {
                        entered.send(()).unwrap();
                        let _ = release.recv();
                    }
                    next.run().await
                })
            },
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
        )
        .unwrap();
    let retirement = tokio::spawn(async move { session_owner.dispose().await });
    retirement_entered
        .recv_timeout(Duration::from_secs(5))
        .expect("session retirement did not enter redaction");
    let mut disposal = Box::pin(telemetry_owner.dispose());
    let first_poll = futures::poll!(disposal.as_mut());
    let shutdown_started_early = backend.calls.lock().iter().any(|call| call == "shutdown");
    release.send(()).unwrap();
    retirement.await.unwrap().unwrap();
    match first_poll {
        std::task::Poll::Ready(result) => result.unwrap(),
        std::task::Poll::Pending => disposal.await.unwrap(),
    }
    assert!(
        !shutdown_started_early,
        "the backend shut down before the admitted retirement record was handed off"
    );
    assert_eq!(*backend.calls.lock(), ["emit:shutdown", "shutdown"]);
    context.fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn shutdown_contains_final_record_redaction_failures_and_still_drains_the_backend() {
    let context = Context::new();
    SessionStore::install(&context).unwrap();
    context
        .events()
        .on_waterfall(
            &context,
            "session-telemetry/record",
            |_, args, next| {
                let shutdown = args
                    .get::<SessionTelemetryRecord>(0)
                    .is_some_and(|record| record.channel == SessionTelemetryChannel::Ops);
                Box::pin(async move {
                    anyhow::ensure!(!shutdown, "final record redaction failed");
                    next.run().await
                })
            },
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
        )
        .unwrap();
    let backend = Arc::new(FakeBackend::default());
    let telemetry_owner = seekdeep_cordis::Fiber::active_child("telemetry-owner");
    SessionTelemetryCoordinator::install(
        &context.with_fiber(telemetry_owner.clone()),
        backend.clone(),
        SessionTelemetryCapture::Live,
    )
    .unwrap();
    let session_owner = seekdeep_cordis::Fiber::active_child("session-owner");
    let _session = live_session(&context.with_fiber(session_owner.clone()), "still-live");
    backend.calls.lock().clear();

    telemetry_owner.dispose().await.unwrap();
    assert_eq!(*backend.calls.lock(), ["shutdown"]);
    session_owner.dispose().await.unwrap();
    context.fiber().dispose().await.unwrap();
}
