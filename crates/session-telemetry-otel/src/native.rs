//! Native Rust OpenTelemetry SDK binding.

use std::{collections::HashMap, sync::Arc, time::Duration};

use opentelemetry::{
    InstrumentationScope, Key, KeyValue,
    logs::{AnyValue, LogRecord as _, Logger as _, LoggerProvider as _, Severity},
};
use opentelemetry_otlp::{
    Compression, LogExporter, Protocol, WithExportConfig as _, WithHttpConfig as _,
};
use opentelemetry_sdk::{
    Resource,
    logs::{BatchConfigBuilder, SdkLogger, SdkLoggerProvider},
    runtime,
};
use seekdeep_anonymous_user_id::{AnonymousUserIdOptions, get_or_create_anonymous_user_id};
use seekdeep_core::session::JsonValue;
use serde_json::Value;

use crate::{OtelLogPipeline, OtelLogPipelineFactory, OtelLogRecord, OtelPipelineOptions};

const DEFAULT_EXPORT_TIMEOUT_MILLIS: u64 = 30_000;
const DEFAULT_MAX_EXPORT_BATCH_SIZE: usize = 512;
const DEFAULT_MAX_QUEUE_SIZE: usize = 2_048;
const DEFAULT_SCHEDULED_DELAY_MILLIS: u64 = 1_000;
const LEDGER_SCOPE: &str = "@seekdeep-ai/seekdeep-session-telemetry-otel";
const OPS_SCOPE: &str = "@seekdeep-ai/seekdeep-session-telemetry-otel/ops";

/// Default native factory backed by the Rust OpenTelemetry SDK and OTLP/HTTP exporter.
#[derive(Clone, Debug, Default)]
pub struct NativeOtelLogPipelineFactory {
    anonymous_user_id: AnonymousUserIdOptions,
}

impl NativeOtelLogPipelineFactory {
    /// Creates a factory with explicit identity resolution inputs.
    #[must_use]
    pub fn new(anonymous_user_id: AnonymousUserIdOptions) -> Self {
        Self { anonymous_user_id }
    }
}

impl OtelLogPipelineFactory for NativeOtelLogPipelineFactory {
    fn anonymous_user_id(&self) -> anyhow::Result<String> {
        Ok(get_or_create_anonymous_user_id(self.anonymous_user_id.clone())?.to_string())
    }

    fn create(&self, options: OtelPipelineOptions) -> anyhow::Result<Arc<dyn OtelLogPipeline>> {
        Ok(Arc::new(NativeOtelLogPipeline::new(options)?))
    }
}

struct NativeOtelLogPipeline {
    provider: SdkLoggerProvider,
    ledger: SdkLogger,
    ops: SdkLogger,
}

impl NativeOtelLogPipeline {
    fn new(options: OtelPipelineOptions) -> anyhow::Result<Self> {
        let exporter = exporter(&options.exporter)?;
        let batch = batch_config(options.processor.as_ref())?;
        let processor =
            opentelemetry_sdk::logs::log_processor_with_async_runtime::BatchLogProcessor::builder(
                exporter,
                runtime::Tokio,
            )
            .with_batch_config(batch)
            .build();
        let resource = Resource::builder_empty()
            .with_attributes(
                options
                    .resource
                    .attributes
                    .into_iter()
                    .map(|(key, value)| KeyValue::new(key, value)),
            )
            .build();
        let provider = SdkLoggerProvider::builder()
            .with_resource(resource)
            .with_log_processor(processor)
            .build();
        let ledger = provider.logger_with_scope(
            InstrumentationScope::builder(LEDGER_SCOPE)
                .with_version(env!("CARGO_PKG_VERSION"))
                .build(),
        );
        let ops = provider.logger_with_scope(
            InstrumentationScope::builder(OPS_SCOPE)
                .with_version(env!("CARGO_PKG_VERSION"))
                .build(),
        );
        Ok(Self {
            provider,
            ledger,
            ops,
        })
    }
}

#[async_trait::async_trait]
impl OtelLogPipeline for NativeOtelLogPipeline {
    fn emit(&self, record: OtelLogRecord) {
        let logger = if record.scope == OPS_SCOPE {
            &self.ops
        } else {
            &self.ledger
        };
        let mut output = logger.create_log_record();
        let timestamp = unix_millis(record.timestamp);
        output.set_timestamp(timestamp);
        output.set_observed_timestamp(timestamp);
        output.set_severity_number(match record.severity_number {
            13 => Severity::Warn,
            17 => Severity::Error,
            _ => Severity::Info,
        });
        output.set_severity_text(record.severity_text);
        match json_value(&record.body) {
            Ok(Some(body)) => output.set_body(body),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "OpenTelemetry SDK rejected an unrepresentable log body");
                return;
            }
        }
        for (key, value) in record.attributes {
            match json_value(&value.into()) {
                Ok(Some(value)) => output.add_attribute(Key::new(key), value),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%error, "OpenTelemetry SDK rejected an unrepresentable log attribute");
                    return;
                }
            }
        }
        logger.emit(output);
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        let provider = self.provider.clone();
        tokio::task::spawn_blocking(move || provider.shutdown())
            .await
            .map_err(|error| anyhow::anyhow!("OpenTelemetry shutdown task failed: {error}"))?
            .map_err(|error| anyhow::anyhow!("OpenTelemetry provider shutdown failed: {error}"))
    }
}

fn exporter(config: &Value) -> anyhow::Result<LogExporter> {
    let object = config
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("session-telemetry-otel: exporter must be an object"))?;
    let endpoint = object
        .get("url")
        .and_then(Value::as_str)
        .expect("validated exporter.url");
    let mut builder = LogExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpJson)
        .with_endpoint(endpoint);
    if let Some(timeout) = optional_duration(object.get("timeoutMillis"), "exporter.timeoutMillis")?
    {
        builder = builder.with_timeout(timeout);
    }
    if let Some(headers) = object.get("headers") {
        builder = builder.with_headers(string_map(headers, "exporter.headers")?);
    }
    if let Some(compression) = object.get("compression") {
        match compression.as_str() {
            Some("none") => {}
            Some("gzip") => builder = builder.with_compression(Compression::Gzip),
            _ => anyhow::bail!(
                "session-telemetry-otel: exporter.compression must be \"none\" or \"gzip\", got {compression}"
            ),
        }
    }
    if let Some(user_agent) = object.get("userAgent") {
        let user_agent = user_agent.as_str().ok_or_else(|| {
            anyhow::anyhow!("session-telemetry-otel: exporter.userAgent must be a string")
        })?;
        builder = builder.with_headers(HashMap::from([(
            "user-agent".to_owned(),
            user_agent.to_owned(),
        )]));
    }
    builder.build().map_err(|error| {
        anyhow::anyhow!("session-telemetry-otel: cannot create OTLP exporter: {error}")
    })
}

fn batch_config(processor: Option<&Value>) -> anyhow::Result<opentelemetry_sdk::logs::BatchConfig> {
    let object = processor
        .map(|value| {
            value.as_object().ok_or_else(|| {
                anyhow::anyhow!("session-telemetry-otel: processor must be an object")
            })
        })
        .transpose()?;
    let max_queue_size = optional_usize(
        object.and_then(|value| value.get("maxQueueSize")),
        "processor.maxQueueSize",
    )?
    .unwrap_or(DEFAULT_MAX_QUEUE_SIZE);
    let max_export_batch_size = optional_usize(
        object.and_then(|value| value.get("maxExportBatchSize")),
        "processor.maxExportBatchSize",
    )?
    .unwrap_or(DEFAULT_MAX_EXPORT_BATCH_SIZE);
    let scheduled_delay = optional_duration(
        object.and_then(|value| value.get("scheduledDelayMillis")),
        "processor.scheduledDelayMillis",
    )?
    .unwrap_or(Duration::from_millis(DEFAULT_SCHEDULED_DELAY_MILLIS));
    let export_timeout = optional_duration(
        object.and_then(|value| value.get("exportTimeoutMillis")),
        "processor.exportTimeoutMillis",
    )?
    .unwrap_or(Duration::from_millis(DEFAULT_EXPORT_TIMEOUT_MILLIS));
    Ok(BatchConfigBuilder::default()
        .with_max_queue_size(max_queue_size)
        .with_max_export_batch_size(max_export_batch_size)
        .with_scheduled_delay(scheduled_delay)
        .with_max_export_timeout(export_timeout)
        .build())
}

fn optional_duration(value: Option<&Value>, field: &str) -> anyhow::Result<Option<Duration>> {
    value
        .map(|value| {
            let millis = value
                .as_f64()
                .filter(|value| value.is_finite() && *value >= 0.0)
                .ok_or_else(|| anyhow::anyhow!("session-telemetry-otel: {field} must be a non-negative finite number, got {value}"))?;
            Ok(Duration::from_secs_f64(millis / 1_000.0))
        })
        .transpose()
}

fn optional_usize(value: Option<&Value>, field: &str) -> anyhow::Result<Option<usize>> {
    value
        .map(|value| {
            let number = value.as_u64().ok_or_else(|| {
                anyhow::anyhow!(
                    "session-telemetry-otel: {field} must be a non-negative integer, got {value}"
                )
            })?;
            usize::try_from(number).map_err(|_| {
                anyhow::anyhow!("session-telemetry-otel: {field} is too large, got {value}")
            })
        })
        .transpose()
}

fn string_map(value: &Value, field: &str) -> anyhow::Result<HashMap<String, String>> {
    value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("session-telemetry-otel: {field} must be an object"))?
        .iter()
        .map(|(key, value)| {
            value
                .as_str()
                .map(|value| (key.clone(), value.to_owned()))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "session-telemetry-otel: {field}.{key} must be a string, got {value}"
                    )
                })
        })
        .collect()
}

fn unix_millis(value: i64) -> std::time::SystemTime {
    if value >= 0 {
        std::time::UNIX_EPOCH + Duration::from_millis(value.unsigned_abs())
    } else {
        std::time::UNIX_EPOCH - Duration::from_millis(value.unsigned_abs())
    }
}

fn json_value(value: &JsonValue) -> anyhow::Result<Option<AnyValue>> {
    if value.is_null() {
        return Ok(None);
    }
    if let Some(value) = value.as_bool() {
        return Ok(Some(AnyValue::Boolean(value)));
    }
    if value.is_string() {
        let value = value.as_str().ok_or_else(|| {
            anyhow::anyhow!("OpenTelemetry SDK cannot encode unpaired UTF-16 in a body string")
        })?;
        return Ok(Some(AnyValue::String(value.to_owned().into())));
    }
    if let Some(values) = value.as_array() {
        let mut output = Vec::new();
        for value in values {
            if let Some(value) = json_value(value)? {
                output.push(value);
            }
        }
        return Ok(Some(AnyValue::ListAny(Box::new(output))));
    }
    if let Some(values) = value.object_entries() {
        let mut fields = HashMap::new();
        for (key, value) in values {
            let key: String = key.deserialize().map_err(|_| {
                anyhow::anyhow!(
                    "OpenTelemetry SDK cannot encode unpaired UTF-16 in a body object key"
                )
            })?;
            fields.insert(key, value.to_owned());
        }
        let mut output = HashMap::new();
        for (key, value) in fields {
            if let Some(value) = json_value(&value)? {
                output.insert(Key::new(key), value);
            }
        }
        return Ok(Some(AnyValue::Map(Box::new(output))));
    }
    Ok(value
        .as_i64()
        .map(AnyValue::Int)
        .or_else(|| {
            value
                .as_u64()
                .and_then(|value| i64::try_from(value).ok())
                .map(AnyValue::Int)
        })
        .or_else(|| value.as_f64().map(AnyValue::Double)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_body_conversion_rejects_unrepresentable_strings_and_keys_explicitly() {
        for raw in [
            r#""\ud800""#,
            r#"{"nested":[1,"\udfff"]}"#,
            r#"{"\ud800":true}"#,
        ] {
            let body = JsonValue::parse(raw.to_owned()).unwrap();
            assert!(
                json_value(&body)
                    .unwrap_err()
                    .to_string()
                    .contains("cannot encode unpaired UTF-16")
            );
            assert_eq!(body.as_raw(), raw);
        }
    }

    #[test]
    fn native_body_conversion_keeps_only_effective_duplicate_field_values() {
        let body =
            JsonValue::parse(r#"{"value":"\ud800","value":"ok","count":3}"#.to_owned()).unwrap();
        let Some(AnyValue::Map(values)) = json_value(&body).unwrap() else {
            panic!("object body");
        };
        assert_eq!(
            values.get(&Key::new("value")),
            Some(&AnyValue::String("ok".into()))
        );
        assert_eq!(values.get(&Key::new("count")), Some(&AnyValue::Int(3)));
    }
}
