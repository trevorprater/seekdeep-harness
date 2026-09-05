//! Fake-pipeline parity for sharing policy, record mapping, config, and shutdown.

use std::sync::Arc;

use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventArgs, Fiber};
use seekdeep_core::{
    session::{AppendOptions, SessionEvent, SessionId},
    session_store::{CreateSessionOptions, SessionStore},
};
use seekdeep_session_telemetry::{
    SessionTelemetryBackend as _, SessionTelemetryChannel, SessionTelemetryRecord,
    SessionTelemetrySeverity, SessionTelemetrySharingStatus, SessionTelemetrySink as _,
};
use seekdeep_session_telemetry_otel::{
    DEFAULT_TELEMETRY_MODE, OpenTelemetrySessionBackend, OtelLogFactoryService, OtelLogPipeline,
    OtelLogPipelineFactory, OtelLogRecord, OtelPipelineOptions, OtelTelemetryConfig,
    SessionTelemetryMode, plugin,
};
use serde_json::{Map, Value, json};

#[derive(Default)]
struct PipelineState {
    records: Vec<OtelLogRecord>,
    shutdowns: usize,
    gate: Option<tokio::sync::oneshot::Receiver<()>>,
}

struct FakePipeline {
    state: Arc<tokio::sync::Mutex<PipelineState>>,
}

#[async_trait::async_trait]
impl OtelLogPipeline for FakePipeline {
    fn emit(&self, record: OtelLogRecord) {
        let state = self.state.clone();
        tokio::spawn(async move { state.lock().await.records.push(record) });
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        let gate = {
            let mut state = self.state.lock().await;
            state.shutdowns += 1;
            state.gate.take()
        };
        if let Some(gate) = gate {
            let _ = gate.await;
        }
        Ok(())
    }
}

struct FakeFactory {
    pipeline: Arc<FakePipeline>,
    options: Mutex<Vec<OtelPipelineOptions>>,
    creations: std::sync::atomic::AtomicUsize,
}

impl FakeFactory {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            pipeline: Arc::new(FakePipeline {
                state: Arc::new(tokio::sync::Mutex::new(PipelineState::default())),
            }),
            options: Mutex::new(Vec::new()),
            creations: std::sync::atomic::AtomicUsize::new(0),
        })
    }
}

impl OtelLogPipelineFactory for FakeFactory {
    fn anonymous_user_id(&self) -> anyhow::Result<String> {
        Ok("anonymous-test-user".to_owned())
    }

    fn create(&self, options: OtelPipelineOptions) -> anyhow::Result<Arc<dyn OtelLogPipeline>> {
        self.creations
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        self.options.lock().push(options);
        Ok(self.pipeline.clone())
    }
}

struct Harness {
    root: Context,
    sessions: Arc<SessionStore>,
    factory: Arc<FakeFactory>,
}

impl Harness {
    fn new(with_factory: bool) -> Self {
        let root = Context::new();
        let sessions = SessionStore::install(&root).unwrap();
        let factory = FakeFactory::new();
        if with_factory {
            OtelLogFactoryService::new(factory.clone())
                .provide(&root)
                .unwrap();
        }
        Self {
            root,
            sessions,
            factory,
        }
    }

    fn context(&self, name: &str) -> (Context, Arc<Fiber>) {
        let fiber = Fiber::active_child(name);
        (self.root.with_fiber(fiber.clone()), fiber)
    }

    fn session(&self, id: &str) -> Arc<seekdeep_core::session::Session> {
        self.sessions
            .create(
                &self.root,
                Some(SessionId::new(id)),
                CreateSessionOptions::default(),
            )
            .unwrap()
    }
}

fn config(mode: &str) -> OtelTelemetryConfig {
    OtelTelemetryConfig {
        mode: Some(mode.to_owned()),
        exporter: Some(json!({
            "url": "https://collector.example/v1/logs",
            "headers": {"authorization": "Bearer test-token"},
            "compression": "gzip"
        })),
        processor: Some(json!({"scheduledDelayMillis": 10, "maxExportBatchSize": 32})),
        shutdown_timeout_millis: None,
    }
}

async fn records(factory: &FakeFactory) -> Vec<OtelLogRecord> {
    tokio::task::yield_now().await;
    factory.pipeline.state.lock().await.records.clone()
}

fn event_types(records: &[OtelLogRecord]) -> Vec<&str> {
    records
        .iter()
        .filter_map(|record| record.attributes.get("event.type").and_then(Value::as_str))
        .collect()
}

#[tokio::test]
async fn full_mode_maps_resource_scope_severity_direct_and_coordinator_records() {
    let harness = Harness::new(true);
    let (context, fiber) = harness.context("otel-full");
    let backend = OpenTelemetrySessionBackend::install(&context, config("FULL")).unwrap();
    assert_eq!(backend.sharing(), SessionTelemetrySharingStatus::Full);
    let session = harness.session("wire");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .unwrap();
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "error", "error": {"message": "boom", "code": "UNKNOWN"}}}),
            AppendOptions::default(),
        )
        .unwrap();
    backend.emit(SessionTelemetryRecord {
        channel: SessionTelemetryChannel::Ledger,
        time: 7,
        severity: SessionTelemetrySeverity::Warn,
        attributes: Map::from_iter([
            ("session.id".to_owned(), json!("wire")),
            ("event.type".to_owned(), json!("manual")),
        ]),
        body: json!({"direct": true}),
    });
    fiber.dispose().await.unwrap();

    let records = records(&harness.factory).await;
    let types = event_types(&records);
    assert!(types.contains(&"turn/start"));
    assert!(types.contains(&"turn/end"));
    assert!(types.contains(&"manual"));
    assert!(records.iter().any(|record| record.scope.ends_with("/ops")));
    assert!(records.iter().any(|record| {
        record.attributes.get("event.type") == Some(&json!("turn/end"))
            && record.severity_number == 17
    }));
    assert!(records.iter().any(|record| {
        record.attributes.get("event.type") == Some(&json!("manual"))
            && record.severity_number == 13
            && record.timestamp == 7
    }));
    {
        let options = harness.factory.options.lock();
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].exporter["compression"], "gzip");
        assert_eq!(
            options[0].resource.attributes["service.name"],
            "seekdeep-harness"
        );
        assert_eq!(
            options[0].resource.attributes["user.id"],
            "anonymous-test-user"
        );
    }
    assert_eq!(harness.factory.pipeline.state.lock().await.shutdowns, 1);
    harness.root.root_fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn feedback_only_exports_only_canonical_prefixes_and_ignores_direct_emits() {
    let harness = Harness::new(true);
    let (context, fiber) = harness.context("otel-feedback");
    let backend = OpenTelemetrySessionBackend::install(&context, config("FEEDBACK_ONLY")).unwrap();
    assert_eq!(
        backend.sharing(),
        SessionTelemetrySharingStatus::FeedbackOnly
    );
    let session = harness.session("feedback-only");
    session
        .append("turn/start", json!({"turn": 1}), AppendOptions::default())
        .unwrap();
    session
        .append(
            "feedback/record",
            json!({"text": "first report"}),
            AppendOptions::default(),
        )
        .unwrap();
    session
        .append(
            "turn/end",
            json!({"turn": 1, "reason": {"kind": "completed"}}),
            AppendOptions::default(),
        )
        .unwrap();
    session
        .append(
            "feedback/record",
            json!({"text": "second report"}),
            AppendOptions::default(),
        )
        .unwrap();
    session
        .append("turn/start", json!({"turn": 2}), AppendOptions::default())
        .unwrap();
    context
        .events()
        .emit(
            &context,
            "session/event",
            &EventArgs::from_values(vec![
                session.clone(),
                Arc::new(SessionEvent {
                    event_type: "feedback/record".to_owned(),
                    seq: u64::try_from(session.events().len()).unwrap(),
                    time: 123,
                    data: json!({"text": "not committed"}),
                    source_event_seqs: None,
                    surface_op: None,
                    ignorable: None,
                }),
            ]),
        )
        .unwrap();
    backend.emit(SessionTelemetryRecord {
        channel: SessionTelemetryChannel::Ledger,
        time: 0,
        severity: SessionTelemetrySeverity::Info,
        attributes: Map::from_iter([("event.type".to_owned(), json!("direct-bypass"))]),
        body: Value::Null,
    });
    fiber.dispose().await.unwrap();
    let records = records(&harness.factory).await;
    assert_eq!(
        event_types(&records),
        [
            "turn/start",
            "feedback/record",
            "turn/end",
            "feedback/record"
        ]
    );
    assert!(!records.iter().any(|record| record.scope.ends_with("/ops")));
    harness.root.root_fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn disabled_mode_constructs_no_pipeline_and_discloses_local_only_policy() {
    let harness = Harness::new(false);
    let (context, fiber) = harness.context("otel-disabled");
    let backend = OpenTelemetrySessionBackend::install(
        &context,
        OtelTelemetryConfig {
            mode: Some("DISABLED".to_owned()),
            exporter: Some(json!({"url": "not read"})),
            processor: Some(json!({"maxExportBatchSize": 0})),
            shutdown_timeout_millis: Some(0.0),
        },
    )
    .unwrap();
    assert_eq!(backend.sharing(), SessionTelemetrySharingStatus::Disabled);
    assert_eq!(DEFAULT_TELEMETRY_MODE, SessionTelemetryMode::Disabled);
    let session = harness.session("disabled");
    session
        .append(
            "feedback/record",
            json!({"text": "local"}),
            AppendOptions::default(),
        )
        .unwrap();
    backend.emit(SessionTelemetryRecord {
        channel: SessionTelemetryChannel::Ledger,
        time: 0,
        severity: SessionTelemetrySeverity::Info,
        attributes: Map::new(),
        body: Value::Null,
    });
    backend.shutdown().await.unwrap();
    fiber.dispose().await.unwrap();
    assert_eq!(
        harness
            .factory
            .creations
            .load(std::sync::atomic::Ordering::Acquire),
        0
    );
    assert!(records(&harness.factory).await.is_empty());
    harness.root.root_fiber().dispose().await.unwrap();
}

#[test]
fn uploading_modes_validate_mode_url_processor_and_shutdown_before_factory_creation() {
    let harness = Harness::new(true);
    let cases = [
        (
            OtelTelemetryConfig {
                mode: Some("FULL".to_owned()),
                ..OtelTelemetryConfig::default()
            },
            "exporter.url is required",
        ),
        (
            OtelTelemetryConfig {
                mode: Some("FEEDBACK_ONLY".to_owned()),
                ..OtelTelemetryConfig::default()
            },
            "exporter.url is required",
        ),
        (
            OtelTelemetryConfig {
                mode: Some("FULL".to_owned()),
                exporter: Some(json!({"url": "not a url"})),
                ..OtelTelemetryConfig::default()
            },
            "not a valid URL",
        ),
        (
            OtelTelemetryConfig {
                mode: Some("FULL".to_owned()),
                exporter: Some(json!({"url": "ftp://collector"})),
                ..OtelTelemetryConfig::default()
            },
            "must be http(s)",
        ),
        (
            OtelTelemetryConfig {
                mode: Some("INVALID".to_owned()),
                exporter: Some(json!({"url": "https://collector"})),
                ..OtelTelemetryConfig::default()
            },
            "unsupported mode",
        ),
        (
            OtelTelemetryConfig {
                mode: Some("FULL".to_owned()),
                exporter: Some(json!({"url": "https://collector"})),
                processor: Some(json!({"maxExportBatchSize": 0})),
                ..OtelTelemetryConfig::default()
            },
            "maxExportBatchSize",
        ),
        (
            OtelTelemetryConfig {
                mode: Some("FULL".to_owned()),
                exporter: Some(json!({"url": "https://collector"})),
                processor: Some(json!({"maxExportBatchSize": 0.5})),
                ..OtelTelemetryConfig::default()
            },
            "maxExportBatchSize",
        ),
        (
            OtelTelemetryConfig {
                mode: Some("FULL".to_owned()),
                exporter: Some(json!({"url": "https://collector"})),
                shutdown_timeout_millis: Some(0.0),
                ..OtelTelemetryConfig::default()
            },
            "shutdownTimeoutMillis",
        ),
        (
            OtelTelemetryConfig {
                mode: Some("FULL".to_owned()),
                exporter: Some(json!({"url": "https://collector"})),
                shutdown_timeout_millis: Some(f64::INFINITY),
                ..OtelTelemetryConfig::default()
            },
            "shutdownTimeoutMillis",
        ),
    ];
    for (index, (config, expected)) in cases.into_iter().enumerate() {
        let (context, _fiber) = harness.context(&format!("invalid-{index}"));
        assert!(
            OpenTelemetrySessionBackend::install(&context, config)
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
    }
    assert_eq!(
        harness
            .factory
            .creations
            .load(std::sync::atomic::Ordering::Acquire),
        0
    );
}

#[tokio::test]
async fn shutdown_deadline_detaches_a_still_pending_pipeline_shutdown() {
    let harness = Harness::new(true);
    let (send, receive) = tokio::sync::oneshot::channel();
    harness.factory.pipeline.state.lock().await.gate = Some(receive);
    let (context, _fiber) = harness.context("bounded-shutdown");
    let mut config = config("FULL");
    config.shutdown_timeout_millis = Some(10.0);
    let backend = OpenTelemetrySessionBackend::install(&context, config).unwrap();
    let started = std::time::Instant::now();
    let error = backend.shutdown().await.unwrap_err();
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
    assert!(
        error
            .to_string()
            .contains("provider shutdown exceeded 10ms")
    );
    assert!(
        backend
            .shutdown()
            .await
            .unwrap_err()
            .to_string()
            .contains("provider shutdown exceeded 10ms")
    );
    send.send(()).unwrap();
    tokio::task::yield_now().await;
    backend.shutdown().await.unwrap();
    assert_eq!(harness.factory.pipeline.state.lock().await.shutdowns, 1);
    harness.root.root_fiber().dispose().await.unwrap();
}

#[tokio::test]
async fn loader_plugin_mounts_the_backend_service_with_static_injection() {
    let harness = Harness::new(true);
    let fiber = harness
        .root
        .plugin(plugin(), serde_json::to_value(config("FULL")).unwrap())
        .unwrap();
    fiber.await_settled().await.unwrap();
    assert_eq!(fiber.inject(), ["sessions"]);
    assert!(
        harness
            .root
            .get(seekdeep_session_telemetry::SESSION_TELEMETRY)
            .is_some()
    );
    fiber.dispose().await.unwrap();
    harness.root.root_fiber().dispose().await.unwrap();
}
