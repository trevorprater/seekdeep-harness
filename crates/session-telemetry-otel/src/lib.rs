//! OpenTelemetry-shaped provider for the Session telemetry capability.

mod native;

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use futures::{FutureExt as _, future::Shared};
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin, ServiceKey};
use seekdeep_core::session::{Session, SessionEvent};
use seekdeep_llm::APP_IDENTITY;
use seekdeep_session_telemetry::{
    SessionTelemetryBackend, SessionTelemetryCapture, SessionTelemetryChannel,
    SessionTelemetryCoordinator, SessionTelemetryRecord, SessionTelemetryService,
    SessionTelemetrySeverity, SessionTelemetrySharingStatus, SessionTelemetrySink,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub use native::NativeOtelLogPipelineFactory;

/// Default sharing mode.
pub const DEFAULT_TELEMETRY_MODE: SessionTelemetryMode = SessionTelemetryMode::Disabled;
/// Default outer shutdown deadline.
pub const DEFAULT_SHUTDOWN_TIMEOUT_MILLIS: f64 = 3_000.0;
/// Largest delay accepted by Node-compatible timer semantics.
pub const MAX_TIMER_DELAY_MILLIS: f64 = 2_147_483_647.0;
/// Cordis plugin name.
pub const NAME: &str = "session-telemetry-otel";
/// Required source services.
pub const INJECT: &[&str] = &["sessions"];

const DISABLED_FEEDBACK_WARNING: &str =
    "session telemetry is DISABLED; nothing will be shared and this feedback remains local";
const NON_CANONICAL_FEEDBACK_WARNING: &str =
    "session telemetry ignored a feedback event absent from the canonical session log";

/// Session sharing policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SessionTelemetryMode {
    /// Export every captured event.
    Full,
    /// Export canonical prefixes only when feedback records consent.
    FeedbackOnly,
    /// Keep all records local.
    Disabled,
}

/// Provider configuration with verbatim exporter and processor objects.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct OtelTelemetryConfig {
    /// Serialized mode. Omission defaults to disabled; unknown values fail closed.
    pub mode: Option<String>,
    /// Verbatim exporter options.
    pub exporter: Option<Value>,
    /// Verbatim batch-processor options.
    pub processor: Option<Value>,
    /// Backend-owned complete shutdown deadline.
    pub shutdown_timeout_millis: Option<f64>,
}

/// Resource identity supplied once to the exporter pipeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtelResource {
    /// Resource attributes.
    pub attributes: BTreeMap<String, String>,
}

/// One OpenTelemetry-shaped log record after severity and scope mapping.
#[derive(Clone, Debug, PartialEq)]
pub struct OtelLogRecord {
    /// Instrumentation scope.
    pub scope: String,
    /// Instrumentation version.
    pub scope_version: String,
    /// Event and observed timestamp in epoch milliseconds.
    pub timestamp: i64,
    /// `OTel` severity number.
    pub severity_number: u8,
    /// `OTel` severity text.
    pub severity_text: &'static str,
    /// Record attributes.
    pub attributes: Map<String, Value>,
    /// Record body.
    pub body: Value,
}

/// Validated exporter pipeline creation request.
#[derive(Clone, Debug, PartialEq)]
pub struct OtelPipelineOptions {
    /// Verbatim exporter options.
    pub exporter: Value,
    /// Verbatim processor options.
    pub processor: Option<Value>,
    /// Resource identity.
    pub resource: OtelResource,
}

/// Object-safe log pipeline supplied by a concrete OpenTelemetry SDK binding.
#[async_trait::async_trait]
pub trait OtelLogPipeline: Send + Sync + 'static {
    /// Enqueues one record without blocking capture.
    fn emit(&self, record: OtelLogRecord);
    /// Drains and quiesces the SDK pipeline.
    async fn shutdown(&self) -> anyhow::Result<()>;
}

/// Object-safe factory for one OpenTelemetry log pipeline.
pub trait OtelLogPipelineFactory: Send + Sync + 'static {
    /// Returns the process-stable anonymous resource identity.
    ///
    /// # Errors
    ///
    /// Returns identity storage or resolution failures.
    fn anonymous_user_id(&self) -> anyhow::Result<String>;
    /// Creates a pipeline from validated and verbatim options.
    ///
    /// # Errors
    ///
    /// Returns concrete SDK configuration or construction failures.
    fn create(&self, options: OtelPipelineOptions) -> anyhow::Result<Arc<dyn OtelLogPipeline>>;
}

/// Concrete factory service wrapper.
pub struct OtelLogFactoryService(Arc<dyn OtelLogPipelineFactory>);

impl std::fmt::Debug for OtelLogFactoryService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("OtelLogFactoryService")
            .field(&"dyn OtelLogPipelineFactory")
            .finish()
    }
}

impl OtelLogFactoryService {
    /// Wraps one concrete SDK binding.
    #[must_use]
    pub fn new(factory: Arc<dyn OtelLogPipelineFactory>) -> Arc<Self> {
        Arc::new(Self(factory))
    }

    /// Provides the factory for the caller's lifetime.
    ///
    /// # Errors
    ///
    /// Returns duplicate-Service or inactive-owner failures.
    pub fn provide(self: &Arc<Self>, context: &Context) -> anyhow::Result<()> {
        context.provide(OTEL_LOG_FACTORY, self.clone())?;
        Ok(())
    }
}

/// Optional SDK-factory override, read only in uploading modes.
pub const OTEL_LOG_FACTORY: ServiceKey<OtelLogFactoryService> = ServiceKey::new("otelLogFactory");

/// Session telemetry backend mapped onto an OpenTelemetry log pipeline.
pub struct OpenTelemetrySessionBackend {
    sharing: SessionTelemetrySharingStatus,
    pipeline: Option<Arc<dyn OtelLogPipeline>>,
    direct: bool,
    shutdown_timeout: Duration,
    shutdown: parking_lot::Mutex<Option<SharedShutdown>>,
}

type SharedShutdown = Shared<futures::future::BoxFuture<'static, Result<(), Arc<str>>>>;

struct CoordinatorSink(Arc<OpenTelemetrySessionBackend>);

#[async_trait::async_trait]
impl SessionTelemetrySink for CoordinatorSink {
    fn emit(&self, record: SessionTelemetryRecord) {
        self.0.enqueue(record);
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        self.0.shutdown().await
    }
}

impl std::fmt::Debug for OpenTelemetrySessionBackend {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenTelemetrySessionBackend")
            .field("sharing", &self.sharing)
            .field("pipeline", &self.pipeline.as_ref().map(|_| "<pipeline>"))
            .finish_non_exhaustive()
    }
}

impl OpenTelemetrySessionBackend {
    /// Installs the backend and its selected capture policy.
    ///
    /// # Errors
    ///
    /// Returns configuration, identity, factory, service, or listener failures.
    #[allow(
        clippy::too_many_lines,
        reason = "one installation transaction owns mode validation, service publication, and capture policy"
    )]
    pub fn install(context: &Context, config: OtelTelemetryConfig) -> anyhow::Result<Arc<Self>> {
        let mode = resolve_mode(config.mode.as_deref())?;
        if mode == SessionTelemetryMode::Disabled {
            let backend = Arc::new(Self {
                sharing: SessionTelemetrySharingStatus::Disabled,
                pipeline: None,
                direct: false,
                shutdown_timeout: Duration::from_millis(3_000),
                shutdown: parking_lot::Mutex::new(None),
            });
            SessionTelemetryService::new(backend.clone()).provide(context)?;
            context.events().on_sync(
                context,
                "session/event",
                move |_, args| {
                    if args
                        .get::<SessionEvent>(1)
                        .is_some_and(|event| event.event_type == "feedback/record")
                    {
                        tracing::warn!("{DISABLED_FEEDBACK_WARNING}");
                    }
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )?;
            return Ok(backend);
        }

        let exporter = config.exporter.ok_or_else(|| {
            anyhow::anyhow!(
                "session-telemetry-otel: exporter.url is required (the full OTLP logs endpoint)"
            )
        })?;
        validate_exporter(&exporter)?;
        validate_processor(config.processor.as_ref())?;
        let shutdown_timeout = validate_shutdown(config.shutdown_timeout_millis)?;
        let factory: Arc<dyn OtelLogPipelineFactory> =
            if let Some(factory) = context.get(OTEL_LOG_FACTORY) {
                factory.0.clone()
            } else {
                Arc::new(NativeOtelLogPipelineFactory::default())
            };
        let user_id = factory.anonymous_user_id()?;
        let pipeline = factory.create(OtelPipelineOptions {
            exporter,
            processor: config.processor,
            resource: OtelResource {
                attributes: BTreeMap::from([
                    ("service.name".to_owned(), APP_IDENTITY.product.to_owned()),
                    (
                        "service.version".to_owned(),
                        APP_IDENTITY.version.to_owned(),
                    ),
                    ("user.id".to_owned(), user_id),
                ]),
            },
        })?;
        let sharing = match mode {
            SessionTelemetryMode::Full => SessionTelemetrySharingStatus::Full,
            SessionTelemetryMode::FeedbackOnly => SessionTelemetrySharingStatus::FeedbackOnly,
            SessionTelemetryMode::Disabled => unreachable!("handled above"),
        };
        let backend = Arc::new(Self {
            sharing,
            pipeline: Some(pipeline),
            direct: mode == SessionTelemetryMode::Full,
            shutdown_timeout,
            shutdown: parking_lot::Mutex::new(None),
        });
        SessionTelemetryService::new(backend.clone()).provide(context)?;
        let sink: Arc<dyn SessionTelemetrySink> = Arc::new(CoordinatorSink(backend.clone()));
        if mode == SessionTelemetryMode::Full {
            SessionTelemetryCoordinator::install(context, sink, SessionTelemetryCapture::Live)?;
        } else {
            let coordinator = SessionTelemetryCoordinator::install(
                context,
                sink,
                SessionTelemetryCapture::OnDemand,
            )?;
            context.events().on_sync(
                context,
                "session/event",
                move |_, args| {
                    let (Some(session), Some(event)) =
                        (args.get::<Session>(0), args.get::<SessionEvent>(1))
                    else {
                        return Ok(EventReply::Undefined);
                    };
                    if event.event_type != "feedback/record" {
                        return Ok(EventReply::Undefined);
                    }
                    let canonical = session.events().iter().any(|stored| {
                        stored.seq == event.seq
                            && stored.event_type == event.event_type
                            && stored.time == event.time
                            && stored.data == event.data
                    });
                    if !canonical {
                        tracing::warn!("{NON_CANONICAL_FEEDBACK_WARNING}");
                        return Ok(EventReply::Undefined);
                    }
                    coordinator.capture_session(&session, Some(event.seq));
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )?;
        }
        Ok(backend)
    }

    fn enqueue(&self, record: SessionTelemetryRecord) {
        let Some(pipeline) = &self.pipeline else {
            return;
        };
        let (severity_number, severity_text) = match record.severity {
            SessionTelemetrySeverity::Info => (9, "INFO"),
            SessionTelemetrySeverity::Warn => (13, "WARN"),
            SessionTelemetrySeverity::Error => (17, "ERROR"),
        };
        let scope = match record.channel {
            SessionTelemetryChannel::Ledger => "@seekdeep-ai/seekdeep-session-telemetry-otel",
            SessionTelemetryChannel::Ops => "@seekdeep-ai/seekdeep-session-telemetry-otel/ops",
        };
        pipeline.emit(OtelLogRecord {
            scope: scope.to_owned(),
            scope_version: env!("CARGO_PKG_VERSION").to_owned(),
            timestamp: record.time,
            severity_number,
            severity_text,
            attributes: record.attributes,
            body: record.body,
        });
    }
}

#[async_trait::async_trait]
impl SessionTelemetrySink for OpenTelemetrySessionBackend {
    fn emit(&self, record: SessionTelemetryRecord) {
        if self.direct {
            self.enqueue(record);
        }
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        let Some(pipeline) = self.pipeline.clone() else {
            return Ok(());
        };
        let shutdown = self
            .shutdown
            .lock()
            .get_or_insert_with(|| {
                async move {
                    tokio::spawn(async move { pipeline.shutdown().await })
                        .await
                        .map_err(|error| {
                            Arc::<str>::from(format!("pipeline shutdown task failed: {error}"))
                        })?
                        .map_err(|error| Arc::<str>::from(format!("{error:#}")))
                }
                .boxed()
                .shared()
            })
            .clone();
        match tokio::time::timeout(self.shutdown_timeout, shutdown).await {
            Ok(result) => result.map_err(|error| anyhow::anyhow!(error.to_string()))?,
            Err(_) => anyhow::bail!(
                "session-telemetry-otel: provider shutdown exceeded {}ms",
                self.shutdown_timeout.as_millis()
            ),
        }
        Ok(())
    }
}

impl SessionTelemetryBackend for OpenTelemetrySessionBackend {
    fn sharing(&self) -> SessionTelemetrySharingStatus {
        self.sharing
    }
}

fn resolve_mode(mode: Option<&str>) -> anyhow::Result<SessionTelemetryMode> {
    match mode.unwrap_or("DISABLED") {
        "FULL" => Ok(SessionTelemetryMode::Full),
        "FEEDBACK_ONLY" => Ok(SessionTelemetryMode::FeedbackOnly),
        "DISABLED" => Ok(SessionTelemetryMode::Disabled),
        mode => anyhow::bail!("session-telemetry-otel: unsupported mode {mode:?}"),
    }
}

fn validate_exporter(exporter: &Value) -> anyhow::Result<()> {
    let url = exporter
        .as_object()
        .and_then(|value| value.get("url"))
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "session-telemetry-otel: exporter.url is required (the full OTLP logs endpoint)"
            )
        })?;
    let parsed = url::Url::parse(url).map_err(|_| {
        anyhow::anyhow!("session-telemetry-otel: exporter.url is not a valid URL: {url:?}")
    })?;
    anyhow::ensure!(
        matches!(parsed.scheme(), "http" | "https"),
        "session-telemetry-otel: exporter.url must be http(s), got {}:",
        parsed.scheme()
    );
    Ok(())
}

fn validate_processor(processor: Option<&Value>) -> anyhow::Result<()> {
    let Some(batch) = processor
        .and_then(Value::as_object)
        .and_then(|value| value.get("maxExportBatchSize"))
    else {
        return Ok(());
    };
    anyhow::ensure!(
        batch.as_u64().is_some_and(|value| value >= 1),
        "session-telemetry-otel: processor.maxExportBatchSize must be a positive integer, got {batch}"
    );
    Ok(())
}

fn validate_shutdown(value: Option<f64>) -> anyhow::Result<Duration> {
    let value = value.unwrap_or(DEFAULT_SHUTDOWN_TIMEOUT_MILLIS);
    anyhow::ensure!(
        value.is_finite() && value > 0.0 && value <= MAX_TIMER_DELAY_MILLIS,
        "session-telemetry-otel: shutdownTimeoutMillis must be a positive finite number no greater than {MAX_TIMER_DELAY_MILLIS:.0}, got {value}"
    );
    Ok(Duration::from_secs_f64(value / 1_000.0))
}

/// Builds the loader-compatible OTEL telemetry plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), |context, config| {
        Box::pin(async move {
            let config: OtelTelemetryConfig = serde_json::from_value(config)?;
            OpenTelemetrySessionBackend::install(&context, config)?;
            Ok(())
        })
    })
}
