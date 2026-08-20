//! Capture coordinator for the session-telemetry capability.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentEvent};
use seekdeep_agent_loop::AgentErrorEvent;
use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply, fiber::EffectHandle};
use seekdeep_core::session::{Session, SessionEvent};
use seekdeep_core::session_store::SESSIONS;
use serde_json::{Map, Value, json};

use crate::{
    SessionTelemetryChannel, SessionTelemetryRecord, SessionTelemetrySeverity, SessionTelemetrySink,
};

/// Whether capture follows live events or reads the canonical log only when requested.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionTelemetryCapture {
    /// Follow the session firehose.
    Live,
    /// Read the canonical log only when explicitly captured.
    OnDemand,
}

/// The handoff cursor: per session, the highest seq handed to a backend.
static HANDOFF_CURSOR: LazyLock<Mutex<HashMap<usize, u64>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn session_key(session: &Arc<Session>) -> usize {
    Arc::as_ptr(session) as usize
}

fn severity_of(event: &SessionEvent) -> SessionTelemetrySeverity {
    match event.event_type.as_str() {
        "tool/result" => {
            let is_error = event
                .data
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array)
                .and_then(|content| content.first())
                .and_then(|block| block.get("isError"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if is_error {
                SessionTelemetrySeverity::Error
            } else {
                SessionTelemetrySeverity::Info
            }
        }
        "turn/end" => {
            let is_error = event
                .data
                .get("reason")
                .and_then(|reason| reason.get("kind"))
                .and_then(Value::as_str)
                == Some("error");
            if is_error {
                SessionTelemetrySeverity::Error
            } else {
                SessionTelemetrySeverity::Info
            }
        }
        _ => SessionTelemetrySeverity::Info,
    }
}

fn shutdown_record(session: &Arc<Session>) -> SessionTelemetryRecord {
    let mut attributes = Map::new();
    attributes.insert("telemetry.op".to_owned(), json!("shutdown"));
    attributes.insert("session.id".to_owned(), json!(session.id().as_str()));
    SessionTelemetryRecord {
        channel: SessionTelemetryChannel::Ops,
        time: now_millis(),
        severity: SessionTelemetrySeverity::Info,
        attributes,
        body: json!({"op": "shutdown"}),
    }
}

fn identity_of(session: &Arc<Session>, event: &SessionEvent) -> Map<String, Value> {
    let mut attributes = Map::new();
    attributes.insert("session.id".to_owned(), json!(session.id().as_str()));
    attributes.insert("event.type".to_owned(), json!(event.event_type));
    attributes.insert("event.seq".to_owned(), json!(event.seq));
    let header = session.header();
    if let Some(cwd) = &header.cwd {
        attributes.insert("session.cwd".to_owned(), json!(cwd));
    }
    if let Some(parent) = &header.parent_session {
        attributes.insert("session.parent_id".to_owned(), json!(parent.as_str()));
    }
    if let Some(seed_length) = header.seed_length {
        attributes.insert("session.seed_length".to_owned(), json!(seed_length));
    }
    attributes
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
        })
}

/// Installs the telemetry capture side onto a context for one backend.
pub struct SessionTelemetryCoordinator {
    context: Context,
    backend: Arc<dyn SessionTelemetrySink>,
    adopted: Mutex<HashSet<usize>>,
    chunk_seen: Mutex<HashMap<usize, HashSet<String>>>,
}

impl SessionTelemetryCoordinator {
    /// Installs live or on-demand capture for one backend.
    ///
    /// # Errors
    ///
    /// Returns listener registration failures.
    pub fn install(
        context: &Context,
        backend: Arc<dyn SessionTelemetrySink>,
        capture: SessionTelemetryCapture,
    ) -> anyhow::Result<Arc<Self>> {
        let coordinator = Arc::new(Self {
            context: context.clone(),
            backend,
            adopted: Mutex::new(HashSet::new()),
            chunk_seen: Mutex::new(HashMap::new()),
        });
        if capture == SessionTelemetryCapture::Live {
            coordinator.register(context)?;
        }
        let cleanup = coordinator.clone();
        context.own(EffectHandle::new("telemetry capture", move || {
            Box::pin(async move {
                let adopted: Vec<_> = cleanup.adopted.lock().iter().copied().collect();
                let sessions = cleanup
                    .context
                    .get(SESSIONS)
                    .map(|store| store.list())
                    .unwrap_or_default();
                for key in adopted {
                    if let Some(session) = sessions.iter().find(|s| session_key(s) == key) {
                        cleanup.deliver(session, cleanup.redact(shutdown_record(session)), None);
                    }
                }
                if let Err(error) = cleanup.backend.shutdown().await {
                    tracing::warn!("telemetry: backend shutdown failed: {error}");
                }
                Ok(())
            })
        }))?;
        Ok(coordinator)
    }

    fn register(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        let created = self.clone();
        context.events().on_sync(
            context,
            "session/created",
            move |_, args| {
                let Some(session) = args.get::<Session>(0) else {
                    return Ok(EventReply::Undefined);
                };
                created.adopt(&session);
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;

        let disposed = self.clone();
        context.events().on_sync(
            context,
            "session/disposed",
            move |_, args| {
                let Some(session) = args.get::<Session>(0) else {
                    return Ok(EventReply::Undefined);
                };
                Self::contain(|| {
                    if !disposed.adopted.lock().remove(&session_key(&session)) {
                        return;
                    }
                    disposed.deliver(&session, disposed.redact(shutdown_record(&session)), None);
                });
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;

        let event_scope = self.clone();
        context.events().on_sync(
            context,
            "session/event",
            move |_, args| {
                let Some(session) = args.get::<Session>(0) else {
                    return Ok(EventReply::Undefined);
                };
                let Some(session_event) = args.get::<SessionEvent>(1) else {
                    return Ok(EventReply::Undefined);
                };
                Self::contain(|| event_scope.capture_event(&session, &session_event));
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;

        let flush = self.clone();
        context.events().on_sync(
            context,
            "session/flush",
            move |_, args| {
                let Some(session) = args.get::<Session>(0) else {
                    return Ok(EventReply::Undefined);
                };
                Self::contain(|| flush.hint_flush(&session));
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;

        let error = self.clone();
        context.events().on_sync(
            context,
            "agent/error",
            move |_, args| {
                let Some(event) = args.get::<AgentEvent<AgentErrorEvent>>(0) else {
                    return Ok(EventReply::Undefined);
                };
                Self::contain(|| {
                    error.relay_agent_error(
                        &event.agent,
                        event.payload.turn,
                        event.payload.step,
                        &event.payload.error,
                    );
                });
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        )?;

        if let Some(store) = context.get(SESSIONS) {
            for session in store.list() {
                self.adopt(&session);
            }
        }
        Ok(())
    }

    /// Projects and hands over the canonical session-log suffix after the handoff cursor.
    pub fn capture_session(&self, session: &Arc<Session>, through_seq: Option<u64>) {
        let cursor = HANDOFF_CURSOR
            .lock()
            .get(&session_key(session))
            .copied()
            .unwrap_or_else(|| session.first_live_seq().saturating_sub(1));
        let events = session.events();
        for event in &events {
            if through_seq.is_some_and(|seq| event.seq > seq) {
                break;
            }
            Self::contain(|| {
                if event.seq <= cursor {
                    self.track(session, event);
                } else {
                    self.capture_event(session, event);
                }
            });
        }
    }

    fn adopt(&self, session: &Arc<Session>) {
        if !self.adopted.lock().insert(session_key(session)) {
            return;
        }
        self.capture_session(session, None);
    }

    fn track(&self, session: &Arc<Session>, event: &SessionEvent) {
        if event.event_type == "assistant/chunk" {
            let key = format!(
                "{}:{}",
                event.data.get("turn").and_then(Value::as_u64).unwrap_or(0),
                event.data.get("step").and_then(Value::as_u64).unwrap_or(0)
            );
            self.seen(session).insert(key);
        }
    }

    fn capture_event(&self, session: &Arc<Session>, event: &SessionEvent) {
        if event.event_type == "assistant/chunk" {
            let key = format!(
                "{}:{}",
                event.data.get("turn").and_then(Value::as_u64).unwrap_or(0),
                event.data.get("step").and_then(Value::as_u64).unwrap_or(0)
            );
            let mut seen = self.seen(session);
            if !seen.insert(key) {
                return;
            }
        }
        let record = SessionTelemetryRecord {
            channel: SessionTelemetryChannel::Ledger,
            time: event.time,
            severity: severity_of(event),
            attributes: identity_of(session, event),
            body: event.data.clone(),
        };
        self.deliver(session, self.redact(record), Some(event.seq));
    }

    fn redact(&self, record: SessionTelemetryRecord) -> SessionTelemetryRecord {
        let args = EventArgs::one(record.clone());
        let passthrough = record.clone();
        let inner_fallback = record.clone();
        let result: anyhow::Result<SessionTelemetryRecord> = futures::executor::block_on(async {
            let reply = self
                .context
                .events()
                .waterfall(
                    &self.context,
                    "session-telemetry/record",
                    &args,
                    move || Box::pin(async move { Ok(EventReply::Value(Arc::new(passthrough))) }),
                )
                .await?;
            Ok(reply
                .downcast::<SessionTelemetryRecord>()
                .map_or(inner_fallback, |record| (*record).clone()))
        });
        result.unwrap_or(record)
    }

    fn deliver(&self, session: &Arc<Session>, record: SessionTelemetryRecord, seq: Option<u64>) {
        self.backend.emit(record);
        if let Some(seq) = seq {
            HANDOFF_CURSOR.lock().insert(session_key(session), seq);
        }
    }

    fn hint_flush(&self, session: &Arc<Session>) {
        if self.adopted.lock().contains(&session_key(session)) {
            self.backend.flush();
        }
    }

    fn relay_agent_error(&self, agent: &Arc<Agent>, turn: u64, step: u64, error: &str) {
        let detail = error_detail(error);
        let mut attributes = Map::new();
        attributes.insert("telemetry.op".to_owned(), json!("agent-error"));
        attributes.insert(
            "session.id".to_owned(),
            json!(agent.session().id().as_str()),
        );
        attributes.insert("agent.id".to_owned(), json!(agent.id().as_str()));
        attributes.insert("error.name".to_owned(), json!(detail.name));
        attributes.insert("turn".to_owned(), json!(turn));
        attributes.insert("step".to_owned(), json!(step));
        let record = SessionTelemetryRecord {
            channel: SessionTelemetryChannel::Ops,
            time: now_millis(),
            severity: SessionTelemetrySeverity::Error,
            attributes,
            body: serde_json::to_value(detail).unwrap_or(Value::Null),
        };
        self.deliver(agent.session(), self.redact(record), None);
    }

    fn seen(&self, session: &Arc<Session>) -> parking_lot::MappedMutexGuard<'_, HashSet<String>> {
        parking_lot::MutexGuard::map(self.chunk_seen.lock(), |map| {
            map.entry(session_key(session)).or_default()
        })
    }

    fn contain(step: impl FnOnce()) {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(step)).is_err() {
            tracing::warn!("telemetry: capture step failed");
        }
    }
}

#[derive(serde::Serialize)]
struct ErrorDetail {
    name: String,
    message: String,
}

fn error_detail(error: &str) -> ErrorDetail {
    ErrorDetail {
        name: "Error".to_owned(),
        message: error.to_owned(),
    }
}
