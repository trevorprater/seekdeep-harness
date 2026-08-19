//! Session-telemetry service definition: the outbound record vocabulary and
//! the backend seat the capture coordinator hands records to.

use std::{ops::Deref, sync::Arc};

use async_trait::async_trait;
use seekdeep_cordis::{Context, ServiceKey, fiber::EffectHandle};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Package-owned invariant companion.
pub mod invariant;

/// Severity of a telemetry record, pre-mapped at capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionTelemetrySeverity {
    /// Informational.
    Info,
    /// Warning.
    Warn,
    /// Error.
    Error,
}

/// Ledger (session-log mirror) or ops (operational signal) channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionTelemetryChannel {
    /// Session-log mirror rows.
    Ledger,
    /// Operational signals with no log home.
    Ops,
}

/// Deployment-selected session-sharing policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionTelemetrySharingStatus {
    /// Full sharing.
    Full,
    /// Feedback-only sharing.
    FeedbackOnly,
    /// Disabled.
    Disabled,
}

/// One logical record handed to a backend.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTelemetryRecord {
    /// Ledger or ops channel.
    pub channel: SessionTelemetryChannel,
    /// Unix epoch milliseconds.
    pub time: i64,
    /// Pre-mapped alerting severity.
    pub severity: SessionTelemetrySeverity,
    /// Identity attributes; string or number values only.
    pub attributes: Map<String, Value>,
    /// The complete payload, never mutated after handoff.
    pub body: Value,
}

/// The minimum backend contract the coordinator requires.
#[async_trait]
pub trait SessionTelemetrySink: Send + Sync + 'static {
    /// Hands one record to the backend's pipeline; must be non-blocking.
    fn emit(&self, record: SessionTelemetryRecord);

    /// Optional hint that a turn ended.
    fn flush(&self) {}

    /// Flushes queued records and reaches quiescence.
    async fn shutdown(&self) -> anyhow::Result<()>;
}

/// Loadable form of the backend contract: one implementation per context.
pub trait SessionTelemetryBackend: SessionTelemetrySink {
    /// Deployment-selected session-sharing policy.
    fn sharing(&self) -> SessionTelemetrySharingStatus;
}

/// Typed Cordis seat corresponding to `ctx.sessionTelemetry`.
pub const SESSION_TELEMETRY: ServiceKey<SessionTelemetryService> =
    ServiceKey::new("sessionTelemetry");

/// Dynamically dispatched exact backend occupying the telemetry service seat.
#[derive(Clone)]
pub struct SessionTelemetryService(Arc<dyn SessionTelemetryBackend>);

impl std::fmt::Debug for SessionTelemetryService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("SessionTelemetryService")
            .field(&"dyn SessionTelemetryBackend")
            .finish()
    }
}

impl SessionTelemetryService {
    /// Wraps one concrete backend.
    #[must_use]
    pub fn new(backend: Arc<dyn SessionTelemetryBackend>) -> Arc<Self> {
        Arc::new(Self(backend))
    }

    /// Returns the object-safe backend.
    #[must_use]
    pub fn backend(&self) -> Arc<dyn SessionTelemetryBackend> {
        self.0.clone()
    }

    /// Publishes this backend on the source-compatible Cordis seat.
    ///
    /// # Errors
    ///
    /// Returns inactive-fiber or duplicate-service failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> anyhow::Result<EffectHandle> {
        Ok(context.provide(SESSION_TELEMETRY, self.clone())?)
    }
}

impl Deref for SessionTelemetryService {
    type Target = dyn SessionTelemetryBackend;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_round_trips() {
        let record = SessionTelemetryRecord {
            channel: SessionTelemetryChannel::Ledger,
            time: 1_700_000_000_000,
            severity: SessionTelemetrySeverity::Error,
            attributes: Map::from_iter([
                ("session.id".to_owned(), Value::String("s1".to_owned())),
                ("event.seq".to_owned(), Value::Number(7.into())),
            ]),
            body: serde_json::json!({"type": "tool/result"}),
        };
        let value = serde_json::to_value(&record).expect("serialize");
        assert_eq!(value["channel"], "ledger");
        assert_eq!(value["severity"], "error");
        assert_eq!(value["attributes"]["session.id"], "s1");
        assert_eq!(value["body"]["type"], "tool/result");
    }

    #[test]
    fn sharing_status_serializes_kebab_case() {
        assert_eq!(
            serde_json::to_string(&SessionTelemetrySharingStatus::FeedbackOnly).expect("status"),
            "\"feedback-only\""
        );
    }

    #[derive(Debug)]
    struct MockBackend {
        sharing: SessionTelemetrySharingStatus,
    }

    #[async_trait]
    impl SessionTelemetrySink for MockBackend {
        fn emit(&self, _record: SessionTelemetryRecord) {}

        async fn shutdown(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    impl SessionTelemetryBackend for MockBackend {
        fn sharing(&self) -> SessionTelemetrySharingStatus {
            self.sharing
        }
    }

    #[tokio::test]
    async fn service_seat_round_trips_the_backend() {
        let context = Context::new();
        let service = SessionTelemetryService::new(Arc::new(MockBackend {
            sharing: SessionTelemetrySharingStatus::Full,
        }));
        service.provide(&context).expect("provide");
        let seat = context.get(SESSION_TELEMETRY).expect("seat");
        assert_eq!(seat.sharing(), SessionTelemetrySharingStatus::Full);
    }
}
