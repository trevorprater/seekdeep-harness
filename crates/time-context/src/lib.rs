//! Opt-in durable per-step request clock context.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use parking_lot::Mutex;
use seekdeep_agent::{Agent, AgentEvent, PreStepDecision};
use seekdeep_agent_loop::AgentPreStepEvent;
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin};
use seekdeep_core::session::SessionEvent;
use seekdeep_llm::{ContentBlock, ContextSnapshotSection, MessageSource, UserMessage};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Durable-reading invariant companion.
pub mod invariant;
/// Browser-zone provenance derivation.
pub mod request_zone;
/// IANA timestamp formatting.
pub mod timestamp;

use request_zone::{
    BrowserTimeZoneContext, derive_browser_time_zone_context, render_browser_time_zone_context,
};
use timestamp::{TimestampFormatter, create_timestamp_formatter, format_timestamp};

/// Cordis plugin name.
pub const NAME: &str = "time-context";
/// The agent registry owns pre-step processing.
pub const INJECT: &[&str] = &["agents"];
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Request clock formatting and durable refresh scheduling.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TimeContextConfig {
    /// Fallback display zone when the open turn has no unique browser zone.
    pub time_zone: Option<String>,
    /// Minimum milliseconds between durable injections in one session.
    pub refresh_interval_ms: Option<f64>,
}

#[derive(Debug)]
struct ResolvedConfig {
    fallback_formatter: TimestampFormatter,
    fallback_time_zone: String,
    refresh_interval_ms: Option<i64>,
}

fn validate_refresh_interval(value: Option<f64>) -> anyhow::Result<Option<i64>> {
    let Some(value) = value else {
        return Ok(None);
    };
    anyhow::ensure!(
        value.is_finite() && value.fract() == 0.0 && (0.0..=MAX_SAFE_INTEGER).contains(&value),
        "time-context: refreshIntervalMs must be a non-negative safe integer, got {}",
        javascript_number(value)
    );
    Ok(Some(
        format!("{value:.0}")
            .parse()
            .expect("validated safe integer fits i64"),
    ))
}

fn validate_plugin_config(value: &Value) -> anyhow::Result<Value> {
    if value.is_null() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("expected object but got {}", javascript_value(value)))?;
    if let Some(time_zone) = object.get("timeZone") {
        if time_zone.is_null() {
            anyhow::bail!("time-context: invalid IANA timeZone null");
        }
        anyhow::ensure!(
            time_zone.is_string(),
            "$.timeZone expected string but got {}",
            javascript_value(time_zone)
        );
    }
    if let Some(interval) = object.get("refreshIntervalMs") {
        if interval.is_null() {
            anyhow::bail!(
                "time-context: refreshIntervalMs must be a non-negative safe integer, got null"
            );
        }
        anyhow::ensure!(
            interval.is_number(),
            "$.refreshIntervalMs expected number but got {}",
            javascript_value(interval)
        );
    }
    let config: TimeContextConfig = serde_json::from_value(value.clone())?;
    validate_refresh_interval(config.refresh_interval_ms)?;
    Ok(value.clone())
}

fn javascript_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(values) => values
            .iter()
            .map(javascript_value)
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_owned(),
    }
}

fn javascript_number(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "Infinity".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else {
        ryu_js::Buffer::new().format(value).to_owned()
    }
}

fn resolve_config(config: &TimeContextConfig) -> anyhow::Result<ResolvedConfig> {
    let refresh_interval_ms = validate_refresh_interval(config.refresh_interval_ms)?;
    let fallback_formatter =
        create_timestamp_formatter(config.time_zone.as_deref()).map_err(|error| {
            let message = config.time_zone.as_ref().map_or_else(
                || "time-context: failed to resolve the system time zone".to_owned(),
                |zone| {
                    format!(
                        "time-context: invalid IANA timeZone {}",
                        serde_json::to_string(zone).expect("zone string serializes")
                    )
                },
            );
            anyhow::anyhow!("{message}: {error}")
        })?;
    let fallback_time_zone = fallback_formatter.resolved_time_zone().to_owned();
    Ok(ResolvedConfig {
        fallback_formatter,
        fallback_time_zone,
        refresh_interval_ms,
    })
}

fn system_now_millis() -> i64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

fn format_duration(elapsed_ms: i128) -> String {
    let mut seconds = elapsed_ms.max(0) / 1_000;
    let days = seconds / 86_400;
    seconds %= 86_400;
    let hours = seconds / 3_600;
    seconds %= 3_600;
    let minutes = seconds / 60;
    seconds %= 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    parts.push(format!("{seconds}s"));
    parts.join(" ")
}

fn is_time_context_event(event: &SessionEvent) -> bool {
    event.event_type == "user/message"
        && event.data["source"]["kind"] == "plugin"
        && event.data["source"]["plugin"] == NAME
}

fn preceding_message_time(agent: &Agent) -> Option<i64> {
    agent
        .session()
        .events()
        .into_iter()
        .rev()
        .find(|event| {
            matches!(
                event.event_type.as_str(),
                "user/message" | "assistant/message" | "tool/result"
            )
        })
        .map(|event| event.time)
}

fn preceding_step_context_time(agent: &Agent, turn: u64) -> Option<i64> {
    for event in agent.session().events().into_iter().rev() {
        if event.event_type == "turn/start" && event.data["turn"].as_u64() == Some(turn) {
            return None;
        }
        if is_time_context_event(&event) {
            return Some(event.time);
        }
    }
    None
}

fn latest_injection_time(agent: &Agent) -> Option<i64> {
    agent
        .session()
        .events()
        .into_iter()
        .rev()
        .find(is_time_context_event)
        .map(|event| event.time)
}

fn request_messages(agent: &Agent, turn: u64, proposed: &[UserMessage]) -> Vec<UserMessage> {
    let events = agent.session().events();
    let start = events.iter().rposition(|event| {
        event.event_type == "turn/start" && event.data["turn"].as_u64() == Some(turn)
    });
    let entered = start.map_or(&events[0..0], |index| &events[index + 1..]);
    entered
        .iter()
        .filter(|event| event.event_type == "user/message")
        .filter_map(|event| serde_json::from_value(event.data.clone()).ok())
        .chain(proposed.iter().cloned())
        .collect()
}

fn render_text(
    now: i64,
    turn: u64,
    step: u64,
    previous: Option<i64>,
    formatter: &TimestampFormatter,
    time_zone: &str,
    browser_context: &BrowserTimeZoneContext,
) -> anyhow::Result<String> {
    let elapsed = previous.map_or_else(
        || "unavailable".to_owned(),
        |previous| format_duration(i128::from(now) - i128::from(previous)),
    );
    let baseline = if step == 1 {
        "model-visible message"
    } else {
        "step context"
    };
    Ok(format!(
        "Time sampled while preparing turn {turn}, step {step}: {}\n{}\nElapsed since the preceding {baseline}: {elapsed}.",
        format_timestamp(now, formatter, time_zone)?,
        render_browser_time_zone_context(browser_context),
    ))
}

fn reading(text: String) -> UserMessage {
    let mut source = MessageSource::plugin(NAME);
    source
        .fields
        .insert("form".to_owned(), Value::String("snapshot".to_owned()));
    source.fields.insert(
        "sections".to_owned(),
        serde_json::to_value([ContextSnapshotSection {
            name: NAME.to_owned(),
            text: text.clone(),
        }])
        .expect("snapshot section serializes"),
    );
    UserMessage::new(vec![ContentBlock::Text { text }], source)
}

fn install_listener(
    context: &Context,
    config: ResolvedConfig,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
) -> anyhow::Result<()> {
    let fallback_time_zone = config.fallback_time_zone;
    let formatters = Arc::new(Mutex::new(HashMap::from([(
        fallback_time_zone.clone(),
        config.fallback_formatter,
    )])));
    let refresh_interval_ms = config.refresh_interval_ms;
    context.events().on_waterfall(
        context,
        "agent/pre-step",
        move |_, args, next| {
            let Some(event) = args.get::<AgentEvent<AgentPreStepEvent>>(0) else {
                return Box::pin(async {
                    Err(anyhow::anyhow!("agent/pre-step is missing its event"))
                });
            };
            let clock = clock.clone();
            let formatters = formatters.clone();
            let fallback_time_zone = fallback_time_zone.clone();
            Box::pin(async move {
                let reply = next.run().await?;
                let decision = reply
                    .downcast::<PreStepDecision>()
                    .map(|decision| (*decision).clone())
                    .ok_or_else(|| {
                        anyhow::anyhow!("agent/pre-step returned an invalid decision")
                    })?;
                let PreStepDecision::Enter { mut messages } = decision else {
                    return Ok(EventReply::Value(Arc::new(decision)));
                };
                if event.payload.signal.is_aborted() {
                    return Ok(EventReply::Value(Arc::new(PreStepDecision::Enter {
                        messages,
                    })));
                }
                let now = clock();
                if let Some(interval) = refresh_interval_ms
                    && interval > 0
                    && let Some(last) = latest_injection_time(&event.agent)
                    && now >= last
                    && now - last < interval
                {
                    return Ok(EventReply::Value(Arc::new(PreStepDecision::Enter {
                        messages,
                    })));
                }
                let previous = if event.payload.step == 1 {
                    preceding_message_time(&event.agent)
                } else {
                    preceding_step_context_time(&event.agent, event.payload.turn)
                };
                let request = request_messages(&event.agent, event.payload.turn, &messages);
                let browser = derive_browser_time_zone_context(&request)?;
                let selected = match &browser {
                    BrowserTimeZoneContext::Resolved { time_zone } => time_zone.clone(),
                    BrowserTimeZoneContext::Mixed { .. } | BrowserTimeZoneContext::Missing => {
                        fallback_time_zone
                    }
                };
                let formatter = {
                    let mut formatters = formatters.lock();
                    if !formatters.contains_key(&selected) {
                        formatters.insert(
                            selected.clone(),
                            create_timestamp_formatter(Some(&selected))?,
                        );
                    }
                    formatters
                        .get(&selected)
                        .expect("formatter inserted")
                        .clone()
                };
                let text = render_text(
                    now,
                    event.payload.turn,
                    event.payload.step,
                    previous,
                    &formatter,
                    &selected,
                    &browser,
                )?;
                messages.push(reading(text));
                Ok(EventReply::Value(Arc::new(PreStepDecision::Enter {
                    messages,
                })))
            })
        },
        EventOptions {
            prepend: true,
            ..EventOptions::default()
        },
    )?;
    Ok(())
}

/// Installs the time-context listener directly.
///
/// # Errors
///
/// Returns configuration, time-zone resolution, or listener failures.
pub fn apply(context: &Context, config: &TimeContextConfig) -> anyhow::Result<()> {
    install_listener(
        context,
        resolve_config(config)?,
        Arc::new(system_now_millis),
    )
}

/// Test seam for a deterministic epoch-millisecond clock.
#[doc(hidden)]
pub fn apply_with_clock(
    context: &Context,
    config: &TimeContextConfig,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
) -> anyhow::Result<()> {
    install_listener(context, resolve_config(config)?, clock)
}

/// Builds the loader-compatible time-context plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: TimeContextConfig = serde_json::from_value(config)?;
            apply(&context, &config)
        })
    })
    .with_config_validator(validate_plugin_config)
}

/// Constructs one source-shaped user-RPC message for compatibility tests.
#[doc(hidden)]
#[must_use]
pub fn user_rpc_message(text: &str, rpc_id: &str, time_zone: &str) -> UserMessage {
    let mut source = MessageSource::user();
    source
        .fields
        .insert("rpcId".to_owned(), Value::String(rpc_id.to_owned()));
    source.fields.insert(
        "clientTimeZone".to_owned(),
        Value::String(time_zone.to_owned()),
    );
    UserMessage::new(
        vec![ContentBlock::Text {
            text: text.to_owned(),
        }],
        source,
    )
}
