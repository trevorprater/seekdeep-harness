//! Package-owned durable clock-context invariants.

use std::sync::{Arc, OnceLock};

use chrono::DateTime;
use regex::Regex;
use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply};
use seekdeep_core::{
    session::{Session, SessionEvent},
    session_store::SESSIONS,
};
use seekdeep_invariants::{
    InvariantFailure, InvariantInstaller, InvariantRegistration, InvariantRegistry,
};
use seekdeep_llm::UserMessage;
use serde_json::Value;

use crate::{
    NAME,
    request_zone::{
        BrowserTimeZoneContext, derive_browser_time_zone_context, render_browser_time_zone_context,
    },
    timestamp::{create_timestamp_formatter, format_timestamp},
};

const PACKAGE_NAME: &str = "seekdeep-time-context";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

fn reading_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"^Time sampled while preparing turn (\d+), step (\d+): (\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:Z|[+-]\d{2}:\d{2})\[[^\]]+\])\n(Browser time zone for this request: .+)\nElapsed since the preceding (model-visible message|step context): (?:unavailable|(?:(?:\d+d )?(?:\d+h )?(?:\d+m )?\d+s))\.$",
        )
        .expect("static durable-reading pattern")
    })
}

fn violation<T>(failure: &InvariantFailure, message: impl Into<String>) -> anyhow::Result<T> {
    Err(failure.fail(message).into())
}

fn preparation_position(
    history: &[SessionEvent],
    failure: &InvariantFailure,
) -> anyhow::Result<(u64, u64)> {
    let mut open_turn = None;
    let mut open_step = None;
    let mut request_started = false;
    for event in history {
        match event.event_type.as_str() {
            "turn/start" => {
                open_turn = event.data["turn"].as_u64();
                open_step = None;
                request_started = false;
            }
            "step/start" => {
                open_step = event.data["step"].as_u64();
                request_started = false;
            }
            "request/header" => request_started = true,
            "step/end" => {
                open_step = None;
                request_started = false;
            }
            "turn/end" => {
                open_turn = None;
                open_step = None;
                request_started = false;
            }
            _ => {}
        }
    }
    let Some(turn) = open_turn else {
        return violation(
            failure,
            "time-context reading must be appended inside an open turn",
        );
    };
    let Some(step) = open_step else {
        return violation(failure, "time-context reading must follow step/start");
    };
    if request_started {
        return violation(failure, "time-context reading must precede request/header");
    }
    Ok((turn, step))
}

fn request_messages(history: &[SessionEvent], turn: u64) -> Vec<UserMessage> {
    let start = history.iter().rposition(|event| {
        event.event_type == "turn/start" && event.data["turn"].as_u64() == Some(turn)
    });
    let selected = start.map_or(&history[0..0], |index| &history[index + 1..]);
    selected
        .iter()
        .filter(|event| event.event_type == "user/message")
        .filter_map(|event| serde_json::from_value(event.data.clone()).ok())
        .collect()
}

fn is_owned(event: &SessionEvent) -> bool {
    event.event_type == "user/message"
        && event.data["source"]["kind"] == "plugin"
        && event.data["source"]["plugin"] == NAME
}

#[allow(clippy::too_many_lines)]
fn validate_reading(
    history: &[SessionEvent],
    event: &SessionEvent,
    failure: &InvariantFailure,
) -> anyhow::Result<()> {
    let content = event.data["content"].as_array();
    let block = content
        .and_then(|content| (content.len() == 1).then(|| &content[0]))
        .and_then(Value::as_object);
    let block_text = block
        .filter(|block| {
            block.len() == 2 && block.get("type").and_then(Value::as_str) == Some("text")
        })
        .and_then(|block| block.get("text"))
        .and_then(Value::as_str);
    let Some(block_text) = block_text else {
        return violation(
            failure,
            "time-context messages must contain exactly one text block",
        );
    };
    let Some(captures) = reading_pattern().captures(block_text) else {
        return violation(
            failure,
            "time-context message does not match the durable reading format",
        );
    };
    let turn = captures[1].parse::<u64>().ok();
    let step = captures[2].parse::<u64>().ok();
    let (Some(turn), Some(step)) = (turn, step) else {
        return violation(
            failure,
            "time-context turn and step must be positive safe integers",
        );
    };
    if turn == 0 || step == 0 || turn > MAX_SAFE_INTEGER || step > MAX_SAFE_INTEGER {
        return violation(
            failure,
            "time-context turn and step must be positive safe integers",
        );
    }
    let expected = preparation_position(history, failure)?;
    if (turn, step) != expected {
        return violation(
            failure,
            format!(
                "time-context reading names turn {turn}/step {step}, expected turn {}/step {}",
                expected.0, expected.1
            ),
        );
    }
    let Some(source) = event.data["source"].as_object() else {
        return violation(failure, "time-context source must retain package ownership");
    };
    if source.get("kind").and_then(Value::as_str) != Some("plugin")
        || source.get("plugin").and_then(Value::as_str) != Some(NAME)
    {
        return violation(failure, "time-context source must retain package ownership");
    }
    let sections = source.get("sections").and_then(Value::as_array);
    let section = sections
        .filter(|sections| sections.len() == 1)
        .and_then(|sections| sections[0].as_object());
    let snapshot_valid = source.len() == 4
        && source.get("form").and_then(Value::as_str) == Some("snapshot")
        && section.is_some_and(|section| {
            section.len() == 2
                && section.get("name").and_then(Value::as_str) == Some(NAME)
                && section.get("text").and_then(Value::as_str) == Some(block_text)
        });
    if !snapshot_valid {
        return violation(
            failure,
            "time-context source must carry only the exact snapshot text, not request authority",
        );
    }
    let browser = derive_browser_time_zone_context(&request_messages(history, turn))?;
    if captures[4] != render_browser_time_zone_context(&browser) {
        return violation(
            failure,
            "time-context browser-zone text does not match current-turn user messages",
        );
    }
    let baseline = &captures[5];
    if (step == 1) != (baseline == "model-visible message") {
        return violation(
            failure,
            format!(
                "time-context step {step} uses the wrong elapsed-time baseline {}",
                serde_json::to_string(baseline).expect("baseline serializes")
            ),
        );
    }
    let rendered = &captures[3];
    let Some(bracket) = rendered.rfind('[') else {
        return violation(
            failure,
            "time-context rendered timestamp must parse and not postdate its durable event",
        );
    };
    let parsed = DateTime::parse_from_rfc3339(&rendered[..bracket]);
    let Ok(parsed) = parsed else {
        return violation(
            failure,
            "time-context rendered timestamp must parse and not postdate its durable event",
        );
    };
    let rendered_time = parsed.timestamp_millis();
    if event.time.unsigned_abs() > MAX_SAFE_INTEGER || event.time < rendered_time {
        return violation(
            failure,
            "time-context rendered timestamp must parse and not postdate its durable event",
        );
    }
    if let BrowserTimeZoneContext::Resolved { time_zone } = browser {
        let formatter = create_timestamp_formatter(Some(&time_zone)).map_err(|error| {
            failure.fail(format!(
                "time-context browser zone cannot format its durable timestamp: {error}"
            ))
        })?;
        let expected =
            format_timestamp(rendered_time, &formatter, &time_zone).map_err(|error| {
                failure.fail(format!(
                    "time-context browser zone cannot format its durable timestamp: {error}"
                ))
            })?;
        if rendered != expected {
            return violation(
                failure,
                "time-context rendered timestamp does not match the unique browser zone",
            );
        }
    }
    Ok(())
}

fn validate_session(session: &Session, failure: &InvariantFailure) -> anyhow::Result<()> {
    let events = session.events();
    for (index, event) in events.iter().enumerate() {
        if is_owned(event) {
            validate_reading(&events[..index], event, failure)?;
        }
    }
    Ok(())
}

fn required_session(args: &EventArgs) -> anyhow::Result<Arc<Session>> {
    args.get::<Session>(0)
        .ok_or_else(|| anyhow::anyhow!("session event lacks a session"))
}

fn global() -> EventOptions {
    EventOptions {
        global: true,
        ..EventOptions::default()
    }
}

fn install(context: &Context, failure: &InvariantFailure) -> anyhow::Result<()> {
    let sessions = context
        .get(SESSIONS)
        .ok_or_else(|| anyhow::anyhow!("seekdeep-time-context invariant requires sessions"))?;
    for session in sessions.list() {
        validate_session(&session, failure)?;
    }
    let created_failure = failure.clone();
    context.events().on_sync(
        context,
        "session/created",
        move |_, args| {
            let session = required_session(&args)?;
            validate_session(&session, &created_failure)?;
            Ok(EventReply::Undefined)
        },
        global(),
    )?;
    let dispatch_failure = failure.clone();
    context.events().on_sync(
        context,
        "internal/dispatch",
        move |_, args| {
            let Some(event_name) = args.get::<String>(1) else {
                return Ok(EventReply::Undefined);
            };
            if event_name.as_str() != "session/event" {
                return Ok(EventReply::Undefined);
            }
            let carried = args
                .get::<EventArgs>(2)
                .ok_or_else(|| anyhow::anyhow!("internal/dispatch lacks event arguments"))?;
            let session = required_session(&carried)?;
            let event = carried
                .get::<SessionEvent>(1)
                .ok_or_else(|| anyhow::anyhow!("session/event lacks an event"))?;
            if is_owned(&event) {
                validate_reading(&session.events(), &event, &dispatch_failure)?;
            }
            Ok(EventReply::Undefined)
        },
        global(),
    )?;
    Ok(())
}

/// Registers validation for loaded and newly appended time readings.
///
/// # Errors
///
/// Returns ordinary invariant registration or installer failures.
pub fn register_invariant(
    registry: &Arc<InvariantRegistry>,
) -> anyhow::Result<InvariantRegistration> {
    registry.register(
        PACKAGE_NAME,
        InvariantInstaller::new(["sessions"], |context, failure| async move {
            install(&context, &failure)
        }),
    )
}
