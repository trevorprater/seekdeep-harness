//! Optional durable tmux location sampled during first-step preparation.

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use seekdeep_agent::{Agent, AgentEvent, PreStepDecision};
use seekdeep_agent_loop::AgentPreStepEvent;
use seekdeep_cordis::{Context, EventOptions, EventReply, Plugin};
use seekdeep_llm::{ContentBlock, ContextSnapshotSection, MessageSource, UserMessage};
use seekdeep_shell::{SHELL, ShellExecRequest, ShellService};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Explained-empty invariant companion.
pub mod invariant;

/// Cordis plugin name.
pub const NAME: &str = "tmux-context";
/// The agent registry owns pre-step processing.
pub const INJECT: &[&str] = &["agents"];
const FIELD_SEPARATOR: &str = "\\t";
const READING_PREFIX: &str = "tmux location (turn ";
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

const TMUX_FIELDS: [&str; 8] = [
    "#{session_name}",
    "#{window_index}",
    "#{window_name}",
    "#{pane_index}",
    "#{pane_id}",
    "#{window_active}",
    "#{pane_active}",
    "#{window_layout}",
];

/// Per-turn tmux-location scheduling configuration.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TmuxContextConfig {
    /// Minimum milliseconds between durable injections in one session.
    pub refresh_interval_ms: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TmuxLocation {
    session_name: String,
    window_index: String,
    window_name: String,
    pane_index: String,
    pane_id: String,
    window_active: String,
    pane_active: String,
    window_layout: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PriorReading {
    state: String,
    time: i64,
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

fn validate_refresh_interval(value: Option<f64>) -> anyhow::Result<Option<i64>> {
    let Some(value) = value else {
        return Ok(None);
    };
    anyhow::ensure!(
        value.is_finite() && value.fract() == 0.0 && (0.0..=MAX_SAFE_INTEGER).contains(&value),
        "tmux-context: refreshIntervalMs must be a non-negative safe integer, got {}",
        javascript_number(value)
    );
    Ok(Some(
        format!("{value:.0}")
            .parse()
            .expect("validated safe integer fits i64"),
    ))
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

fn validate_plugin_config(value: &Value) -> anyhow::Result<Value> {
    if value.is_null() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("expected object but got {}", javascript_value(value)))?;
    if let Some(interval) = object.get("refreshIntervalMs") {
        if interval.is_null() {
            anyhow::bail!(
                "tmux-context: refreshIntervalMs must be a non-negative safe integer, got null"
            );
        }
        anyhow::ensure!(
            interval.is_number(),
            "$.refreshIntervalMs expected number but got {}",
            javascript_value(interval)
        );
    }
    let config: TmuxContextConfig = serde_json::from_value(value.clone())?;
    validate_refresh_interval(config.refresh_interval_ms)?;
    Ok(value.clone())
}

fn query_command(process_id: u32) -> String {
    let format = TMUX_FIELDS.join(FIELD_SEPARATOR);
    [
        "[ -n \"$TMUX_PANE\" ] || exit 1".to_owned(),
        format!("self_tty=$(ps -o tty= -p {process_id} | tr -d ' ')"),
        "[ -n \"$self_tty\" ] || exit 1".to_owned(),
        "pane_tty=$(tmux display-message -t \"$TMUX_PANE\" -p '#{pane_tty}') || exit 1".to_owned(),
        "[ \"$pane_tty\" = \"/dev/$self_tty\" ] || exit 1".to_owned(),
        format!("exec tmux display-message -t \"$TMUX_PANE\" -p '{format}'"),
    ]
    .join("\n")
}

async fn query_tmux_location(
    shell: &ShellService,
    process_id: u32,
    signal: seekdeep_llm::AbortSignal,
) -> anyhow::Result<Option<TmuxLocation>> {
    let mut request = ShellExecRequest::new(query_command(process_id));
    request.signal = Some(signal);
    let spec = shell.resolve(request)?;
    let result = shell.run(spec).await?;
    if result.exit_code != Some(0) {
        return Ok(None);
    }
    let line = result.stdout.text.split('\n').next().unwrap_or_default();
    let parts = line.split(FIELD_SEPARATOR).collect::<Vec<_>>();
    let [
        session_name,
        window_index,
        window_name,
        pane_index,
        pane_id,
        window_active,
        pane_active,
        window_layout,
    ] = parts.as_slice()
    else {
        return Ok(None);
    };
    if pane_id.is_empty() {
        return Ok(None);
    }
    Ok(Some(TmuxLocation {
        session_name: (*session_name).to_owned(),
        window_index: (*window_index).to_owned(),
        window_name: (*window_name).to_owned(),
        pane_index: (*pane_index).to_owned(),
        pane_id: (*pane_id).to_owned(),
        window_active: (*window_active).to_owned(),
        pane_active: (*pane_active).to_owned(),
        window_layout: (*window_layout).to_owned(),
    }))
}

fn render_state(location: &TmuxLocation) -> String {
    format!(
        "session {}, window {} {}, pane {} {}\nwindow active={}, pane active={}, layout {}",
        location.session_name,
        location.window_index,
        serde_json::to_string(&location.window_name).expect("window name serializes"),
        location.pane_index,
        location.pane_id,
        location.window_active,
        location.pane_active,
        location.window_layout,
    )
}

fn render_reading(location: &TmuxLocation, turn: u64) -> String {
    format!("{READING_PREFIX}{turn}):\n{}", render_state(location))
}

fn latest_injected_state(agent: &Agent) -> Option<PriorReading> {
    for event in agent.session().events().into_iter().rev() {
        if event.event_type != "user/message"
            || event.data["source"]["kind"] != "plugin"
            || event.data["source"]["plugin"] != NAME
        {
            continue;
        }
        let block = event.data["content"].as_array()?.first()?;
        if block["type"] != "text" {
            return None;
        }
        let text = block["text"].as_str()?;
        let state = text
            .find('\n')
            .map_or_else(String::new, |newline| text[newline + 1..].to_owned());
        return Some(PriorReading {
            state,
            time: event.time,
        });
    }
    None
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

fn system_now_millis() -> i64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// Captures contained optional-query failures in compatibility tests.
#[doc(hidden)]
pub type WarningSink = Arc<dyn Fn(String) + Send + Sync>;

fn install_listener(
    context: &Context,
    refresh_interval_ms: Option<i64>,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    process_id: u32,
    warning: WarningSink,
) -> anyhow::Result<()> {
    context.events().on_waterfall(
        context,
        "agent/pre-step",
        move |dispatch, args, next| {
            let Some(event) = args.get::<AgentEvent<AgentPreStepEvent>>(0) else {
                return Box::pin(async {
                    Err(anyhow::anyhow!("agent/pre-step is missing its event"))
                });
            };
            let clock = clock.clone();
            let warning = warning.clone();
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
                if event.payload.signal.is_aborted() || event.payload.step != 1 {
                    return Ok(EventReply::Value(Arc::new(PreStepDecision::Enter {
                        messages,
                    })));
                }
                let Some(shell) = dispatch.get(SHELL) else {
                    return Ok(EventReply::Value(Arc::new(PreStepDecision::Enter {
                        messages,
                    })));
                };
                let previous = latest_injected_state(&event.agent);
                if let (Some(interval), Some(previous)) = (refresh_interval_ms, &previous)
                    && interval > 0
                {
                    let now = clock();
                    if now >= previous.time && now - previous.time < interval {
                        return Ok(EventReply::Value(Arc::new(PreStepDecision::Enter {
                            messages,
                        })));
                    }
                }
                let location = match query_tmux_location(
                    &shell,
                    process_id,
                    event.payload.signal.clone(),
                )
                .await
                {
                    Ok(location) => location,
                    Err(error) => {
                        warning(format!(
                            "tmux location query failed: {error}; injecting no location this turn"
                        ));
                        None
                    }
                };
                let Some(location) = location else {
                    return Ok(EventReply::Value(Arc::new(PreStepDecision::Enter {
                        messages,
                    })));
                };
                let state = render_state(&location);
                if previous.is_some_and(|previous| previous.state == state) {
                    return Ok(EventReply::Value(Arc::new(PreStepDecision::Enter {
                        messages,
                    })));
                }
                messages.insert(0, reading(render_reading(&location, event.payload.turn)));
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

/// Installs the production listener.
///
/// # Errors
///
/// Returns invalid refresh scheduling or listener registration failures.
pub fn apply(context: &Context, config: &TmuxContextConfig) -> anyhow::Result<()> {
    install_listener(
        context,
        validate_refresh_interval(config.refresh_interval_ms)?,
        Arc::new(system_now_millis),
        std::process::id(),
        Arc::new(|message| tracing::warn!("{message}")),
    )
}

/// Deterministic test seam for clock, process identity, and warning capture.
#[doc(hidden)]
pub fn apply_with_environment(
    context: &Context,
    config: &TmuxContextConfig,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    process_id: u32,
    warning: WarningSink,
) -> anyhow::Result<()> {
    install_listener(
        context,
        validate_refresh_interval(config.refresh_interval_ms)?,
        clock,
        process_id,
        warning,
    )
}

/// Builds the Loader-compatible plugin.
#[must_use]
pub fn plugin() -> Plugin {
    Plugin::new(NAME, INJECT.iter().copied(), move |context, config| {
        Box::pin(async move {
            let config: TmuxContextConfig = serde_json::from_value(config)?;
            apply(&context, &config)
        })
    })
    .with_config_validator(validate_plugin_config)
}
