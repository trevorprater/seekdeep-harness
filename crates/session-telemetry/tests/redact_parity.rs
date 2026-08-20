//! Behavioral mirror of packages/session/session-telemetry/tests/redact.spec.ts.

use std::sync::Arc;

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
use serde_json::{Map, Value, json};

const SECRET: &str = "sk-fixture1234567890";

#[derive(Default)]
struct CollectingBackend {
    records: Mutex<Vec<SessionTelemetryRecord>>,
}

#[async_trait]
impl SessionTelemetrySink for CollectingBackend {
    fn emit(&self, record: SessionTelemetryRecord) {
        self.records.lock().push(record);
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
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

fn setup() -> (Context, Arc<CollectingBackend>) {
    let context = Context::new();
    SessionStore::install(&context).expect("sessions");
    let backend = Arc::new(CollectingBackend::default());
    SessionTelemetryCoordinator::install(&context, backend.clone(), SessionTelemetryCapture::Live)
        .expect("coordinator");
    (context, backend)
}

fn append(context: &Context, session_id: &str, text: &str) {
    let sessions = context
        .get(seekdeep_core::session_store::SESSIONS)
        .expect("sessions");
    let session = sessions
        .create(
            context,
            Some(seekdeep_core::session::SessionId::new(session_id)),
            CreateSessionOptions::default(),
        )
        .expect("session");
    session
        .append(
            "user/message",
            user_message(text),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("append");
}

#[tokio::test]
async fn passes_records_through_unchanged_when_no_listener_is_mounted() {
    let (context, backend) = setup();
    append(&context, "w", &format!("key {SECRET}"));
    let records = backend.records.lock();
    let body = records[0].body.as_object().expect("body object");
    let text = body["content"][0]["text"].as_str().expect("text");
    assert_eq!(text, format!("key {SECRET}"));
}

#[tokio::test]
async fn applies_a_mounted_rule_to_every_outbound_record() {
    let (context, backend) = setup();
    context
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
                        .expect("downstream record");
                    record.body = json!({"scrubbed": true});
                    Ok(EventReply::Value(Arc::new(record)))
                })
            },
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
        )
        .expect("rule");
    append(&context, "rule", SECRET);
    let records = backend.records.lock();
    assert_eq!(records[0].body, json!({"scrubbed": true}));
}

#[tokio::test]
async fn keeps_the_canonical_log_untouched_by_a_mounted_rule() {
    let (context, _backend) = setup();
    context
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
                        .expect("downstream record");
                    record.body = Value::Null;
                    Ok(EventReply::Value(Arc::new(record)))
                })
            },
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
        )
        .expect("rule");
    let sessions = context
        .get(seekdeep_core::session_store::SESSIONS)
        .expect("sessions");
    let session = sessions
        .create(
            &context,
            Some(seekdeep_core::session::SessionId::new("log")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    session
        .append(
            "user/message",
            user_message(SECRET),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("append");
    let events = session.events();
    let logged = events[0].data.as_object().expect("object");
    assert_eq!(logged["content"][0]["text"].as_str(), Some(SECRET));
}

#[tokio::test]
async fn stacks_listeners_outermost_first_around_next() {
    let (context, backend) = setup();
    let order = Arc::new(Mutex::new(Vec::new()));
    let outer_order = order.clone();
    context
        .events()
        .on_waterfall(
            &context,
            "session-telemetry/record",
            move |_ctx, _args, next| {
                let order = outer_order.clone();
                Box::pin(async move {
                    order.lock().push("outer-before".to_owned());
                    let reply = next.run().await?;
                    order.lock().push("outer-after".to_owned());
                    let mut record = reply
                        .downcast::<SessionTelemetryRecord>()
                        .map(|record| (*record).clone())
                        .expect("downstream record");
                    record.attributes.insert("outer".to_owned(), json!(1));
                    Ok(EventReply::Value(Arc::new(record)))
                })
            },
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
        )
        .expect("outer");
    let inner_order = order.clone();
    context
        .events()
        .on_waterfall(
            &context,
            "session-telemetry/record",
            move |_ctx, _args, next| {
                let order = inner_order.clone();
                Box::pin(async move {
                    order.lock().push("inner".to_owned());
                    let reply = next.run().await?;
                    let mut record = reply
                        .downcast::<SessionTelemetryRecord>()
                        .map(|record| (*record).clone())
                        .expect("downstream record");
                    record.attributes.insert("inner".to_owned(), json!(1));
                    Ok(EventReply::Value(Arc::new(record)))
                })
            },
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
        )
        .expect("inner");
    append(&context, "stack", "hi");
    assert_eq!(*order.lock(), vec!["outer-before", "inner", "outer-after"]);
    let records = backend.records.lock();
    assert_eq!(records[0].attributes.get("outer"), Some(&json!(1)));
    assert_eq!(records[0].attributes.get("inner"), Some(&json!(1)));
}

#[tokio::test]
async fn a_listener_that_skips_next_replaces_everything_beneath_it() {
    let (context, backend) = setup();
    let inner_called = Arc::new(Mutex::new(false));
    let inner_flag = inner_called.clone();
    context
        .events()
        .on_waterfall(
            &context,
            "session-telemetry/record",
            move |_ctx, _args, _next| {
                Box::pin(async move {
                    let replacement = SessionTelemetryRecord {
                        channel: SessionTelemetryChannel::Ops,
                        time: 0,
                        severity: SessionTelemetrySeverity::Info,
                        attributes: Map::new(),
                        body: json!("replaced"),
                    };
                    Ok(EventReply::Value(Arc::new(replacement)))
                })
            },
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
        )
        .expect("outer veto");
    context
        .events()
        .on_waterfall(
            &context,
            "session-telemetry/record",
            move |_ctx, _args, next| {
                let flag = inner_flag.clone();
                Box::pin(async move {
                    *flag.lock() = true;
                    next.run().await
                })
            },
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
        )
        .expect("inner");
    append(&context, "veto", "hi");
    let records = backend.records.lock();
    assert_eq!(records[0].body, json!("replaced"));
    assert!(!*inner_called.lock());
}

#[tokio::test]
async fn a_throwing_rule_withholds_the_record_fail_closed() {
    let (context, backend) = setup();
    context
        .events()
        .on_waterfall(
            &context,
            "session-telemetry/record",
            move |_ctx, _args, _next| {
                Box::pin(async move { Err(anyhow::anyhow!("rule exploded")) })
            },
            EventOptions {
                global: true,
                ..EventOptions::default()
            },
        )
        .expect("throwing rule");
    let sessions = context
        .get(seekdeep_core::session_store::SESSIONS)
        .expect("sessions");
    let session = sessions
        .create(
            &context,
            Some(seekdeep_core::session::SessionId::new("closed")),
            CreateSessionOptions::default(),
        )
        .expect("session");
    session
        .append(
            "user/message",
            user_message("hi"),
            AppendOptions {
                surface_op: Some(SurfaceOp::append()),
                ..AppendOptions::default()
            },
        )
        .expect("append succeeds, capture withheld");
    assert!(backend.records.lock().is_empty());
    assert_eq!(session.events().len(), 1);
}
