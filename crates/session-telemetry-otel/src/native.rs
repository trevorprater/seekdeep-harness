//! Native Rust OpenTelemetry SDK binding.

use std::{collections::HashMap, fmt::Write as _, sync::Arc, time::Duration};

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
        let mut escaped = false;
        if let Some(body) = json_value(&record.body, &mut escaped) {
            output.set_body(body);
        }
        for (key, value) in record.attributes {
            if let Some(value) = json_value(&value.into(), &mut escaped) {
                output.add_attribute(Key::new(key), value);
            }
        }
        if escaped {
            // The source serializes an unpaired UTF-16 code unit as a `\uXXXX` JSON escape.
            // The native SDK only carries Unicode scalar strings, so the record keeps that
            // escape as text and says so instead of being dropped (DEV-002).
            output.add_attribute(
                Key::new(ESCAPED_UTF16_ATTRIBUTE),
                AnyValue::String(ESCAPED_UTF16_VALUE.into()),
            );
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
    // The exporter seeds its header map with a `User-Agent` entry under exactly that
    // spelling, and the map is keyed by string, not by header name: a differently cased
    // key would coexist with the default and the wire value would depend on map order.
    let mut headers = match object.get("headers") {
        Some(headers) => string_map(headers, "exporter.headers")?
            .into_iter()
            .map(|(key, value)| (canonical_header_key(&key), value))
            .collect(),
        None => HashMap::new(),
    };
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
        headers.insert(USER_AGENT_HEADER.to_owned(), user_agent.to_owned());
    }
    if !headers.is_empty() {
        builder = builder.with_headers(headers);
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

/// The exporter's own spelling of the user-agent header key.
const USER_AGENT_HEADER: &str = "User-Agent";

/// Maps any spelling of the user-agent key onto the exporter's, leaving other keys as
/// configured.
fn canonical_header_key(key: &str) -> String {
    if key.eq_ignore_ascii_case(USER_AGENT_HEADER) {
        USER_AGENT_HEADER.to_owned()
    } else {
        key.to_owned()
    }
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

/// Record attribute set when a string or key kept an unpaired UTF-16 escape as text.
const ESCAPED_UTF16_ATTRIBUTE: &str = "seekdeep.telemetry.unpaired_utf16";
/// The attribute's value: the escapes are the `\uXXXX` text a JSON reader would decode.
const ESCAPED_UTF16_VALUE: &str = "escaped";

/// Converts a lossless JSON value for the SDK. A string or key with an unpaired UTF-16
/// code unit becomes the text of its JSON escape and sets `escaped`.
fn json_value(value: &JsonValue, escaped: &mut bool) -> Option<AnyValue> {
    if value.is_null() {
        return None;
    }
    if let Some(value) = value.as_bool() {
        return Some(AnyValue::Boolean(value));
    }
    if value.is_string() {
        return Some(AnyValue::String(
            lossless_string(value.as_raw(), escaped).into(),
        ));
    }
    if let Some(values) = value.as_array() {
        let output = values
            .iter()
            .filter_map(|value| json_value(value, escaped))
            .collect();
        return Some(AnyValue::ListAny(Box::new(output)));
    }
    if let Some(values) = value.object_entries() {
        // A repeated key keeps its last value (JSON.parse semantics), so only the
        // effective value is converted and can set the escape marker.
        let mut fields = HashMap::new();
        for (key, value) in values {
            fields.insert(lossless_string(key.as_raw(), escaped), value.to_owned());
        }
        let mut output = HashMap::new();
        for (key, value) in fields {
            if let Some(value) = json_value(&value, escaped) {
                output.insert(Key::new(key), value);
            }
        }
        return Some(AnyValue::Map(Box::new(output)));
    }
    value
        .as_i64()
        .map(AnyValue::Int)
        .or_else(|| {
            value
                .as_u64()
                .and_then(|value| i64::try_from(value).ok())
                .map(AnyValue::Int)
        })
        .or_else(|| value.as_f64().map(AnyValue::Double))
}

/// Decodes a raw JSON string literal, keeping every unpaired surrogate as the text of
/// its `\uXXXX` escape (the bytes the source's JSON serializer emits for it).
fn lossless_string(raw: &str, escaped: &mut bool) -> String {
    if let Ok(text) = serde_json::from_str::<String>(raw) {
        return text;
    }
    let literal = raw
        .strip_prefix('"')
        .and_then(|inner| inner.strip_suffix('"'))
        .unwrap_or(raw);
    let mut output = String::with_capacity(literal.len());
    let mut characters = literal.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        match characters.next() {
            Some('u') => {
                let unit = hex_unit(&mut characters);
                match unit {
                    Some(high @ 0xD800..=0xDBFF) => {
                        let mut lookahead = characters.clone();
                        if lookahead.next() == Some('\\')
                            && lookahead.next() == Some('u')
                            && let Some(low @ 0xDC00..=0xDFFF) = hex_unit(&mut lookahead)
                        {
                            characters = lookahead;
                            let scalar = 0x10000
                                + ((u32::from(high) - 0xD800) << 10)
                                + (u32::from(low) - 0xDC00);
                            output.push(char::from_u32(scalar).unwrap_or('\u{FFFD}'));
                        } else {
                            *escaped = true;
                            let _ = write!(output, "\\u{high:04x}");
                        }
                    }
                    Some(unit @ 0xDC00..=0xDFFF) => {
                        *escaped = true;
                        let _ = write!(output, "\\u{unit:04x}");
                    }
                    Some(unit) => {
                        output.push(char::from_u32(u32::from(unit)).unwrap_or('\u{FFFD}'));
                    }
                    None => output.push('\u{FFFD}'),
                }
            }
            Some('n') => output.push('\n'),
            Some('t') => output.push('\t'),
            Some('r') => output.push('\r'),
            Some('b') => output.push('\u{8}'),
            Some('f') => output.push('\u{c}'),
            Some(other) => output.push(other),
            None => {}
        }
    }
    output
}

fn hex_unit(characters: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<u16> {
    let mut unit = 0u16;
    for _ in 0..4 {
        let digit = characters.next()?.to_digit(16)?;
        unit = (unit << 4) | u16::try_from(digit).ok()?;
    }
    Some(unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_body_conversion_keeps_unpaired_utf16_as_escape_text_and_marks_it() {
        for (raw, expected) in [
            (r#""\ud800""#, AnyValue::String("\\ud800".into())),
            (
                r#"{"nested":[1,"\udfff"]}"#,
                AnyValue::Map(Box::new(HashMap::from([(
                    Key::new("nested"),
                    AnyValue::ListAny(Box::new(vec![
                        AnyValue::Int(1),
                        AnyValue::String("\\udfff".into()),
                    ])),
                )]))),
            ),
            (
                r#"{"\ud800":true}"#,
                AnyValue::Map(Box::new(HashMap::from([(
                    Key::new("\\ud800"),
                    AnyValue::Boolean(true),
                )]))),
            ),
        ] {
            let body = JsonValue::parse(raw.to_owned()).unwrap();
            let mut escaped = false;
            assert_eq!(json_value(&body, &mut escaped), Some(expected));
            assert!(escaped, "{raw}");
            assert_eq!(body.as_raw(), raw);
        }
    }

    #[test]
    fn native_body_conversion_decodes_pairs_and_ordinary_escapes_without_marking() {
        let body = JsonValue::parse(
            r#"{"text":"a\ud83d\ude00b\n\"quoted\" \u00e9\\end","plain":"x"}"#.to_owned(),
        )
        .unwrap();
        let mut escaped = false;
        let Some(AnyValue::Map(values)) = json_value(&body, &mut escaped) else {
            panic!("object body");
        };
        assert!(!escaped);
        assert_eq!(
            values.get(&Key::new("text")),
            Some(&AnyValue::String(
                "a\u{1F600}b\n\"quoted\" \u{e9}\\end".into()
            ))
        );
    }

    #[test]
    fn native_body_conversion_keeps_only_effective_duplicate_field_values() {
        let body =
            JsonValue::parse(r#"{"value":"\ud800","value":"ok","count":3}"#.to_owned()).unwrap();
        let mut escaped = false;
        let Some(AnyValue::Map(values)) = json_value(&body, &mut escaped) else {
            panic!("object body");
        };
        assert!(
            !escaped,
            "an overridden duplicate value must not mark the record"
        );
        assert_eq!(
            values.get(&Key::new("value")),
            Some(&AnyValue::String("ok".into()))
        );
        assert_eq!(values.get(&Key::new("count")), Some(&AnyValue::Int(3)));
    }
}
