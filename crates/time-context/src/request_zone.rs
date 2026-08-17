//! Browser-zone derivation and model-facing policy text for one open turn.

use std::sync::OnceLock;

use regex::Regex;
use seekdeep_llm::UserMessage;
use serde_json::Value;

use crate::timestamp::canonical_time_zone_name;

/// Browser-zone facts derived from user-RPC messages in one open turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BrowserTimeZoneContext {
    /// Exactly one canonical browser zone was supplied.
    Resolved {
        /// Canonical IANA zone.
        time_zone: String,
    },
    /// Multiple canonical browser zones were supplied.
    Mixed {
        /// Sorted, duplicate-free zones.
        time_zones: Vec<String>,
    },
    /// No qualifying browser zone was supplied.
    Missing,
}

fn iana_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"^[A-Za-z][A-Za-z0-9_+.-]*(?:/[A-Za-z0-9_+.-]+)+$")
            .expect("static browser-zone pattern")
    })
}

fn browser_time_zone(message: &UserMessage) -> anyhow::Result<Option<String>> {
    if message.source().kind != "user"
        || !message
            .source()
            .fields
            .get("rpcId")
            .is_some_and(Value::is_string)
    {
        return Ok(None);
    }
    let Some(value) = message
        .source()
        .fields
        .get("clientTimeZone")
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    anyhow::ensure!(
        value == "UTC" || iana_pattern().is_match(value),
        "browser time zone must be canonical UTC or IANA Area/Location: {value:?}"
    );
    let canonical = canonical_time_zone_name(value)
        .ok_or_else(|| anyhow::anyhow!("browser time zone is unsupported: {value:?}"))?;
    anyhow::ensure!(
        canonical == value,
        "browser time zone must be canonical: {value:?}"
    );
    Ok(Some(value.to_owned()))
}

/// Derives the unique, mixed, or missing browser zone for one open turn.
///
/// # Errors
///
/// Returns when any qualifying user-RPC message carries an invalid,
/// unsupported, or noncanonical zone.
pub fn derive_browser_time_zone_context(
    messages: &[UserMessage],
) -> anyhow::Result<BrowserTimeZoneContext> {
    let mut time_zones = Vec::new();
    for message in messages {
        if let Some(zone) = browser_time_zone(message)?
            && !time_zones.contains(&zone)
        {
            time_zones.push(zone);
        }
    }
    time_zones.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    Ok(match time_zones.as_slice() {
        [] => BrowserTimeZoneContext::Missing,
        [time_zone] => BrowserTimeZoneContext::Resolved {
            time_zone: time_zone.clone(),
        },
        [_, _, ..] => BrowserTimeZoneContext::Mixed { time_zones },
    })
}

/// Renders the durable model instruction for browser-zone provenance.
#[must_use]
pub fn render_browser_time_zone_context(context: &BrowserTimeZoneContext) -> String {
    match context {
        BrowserTimeZoneContext::Resolved { time_zone } => format!(
            "Browser time zone for this request: {time_zone}. Interpret otherwise-unqualified dates and times in this zone."
        ),
        BrowserTimeZoneContext::Mixed { time_zones } => format!(
            "Browser time zone for this request: mixed {}. Ask the user to clarify otherwise-unqualified dates and times.",
            serde_json::json!(time_zones)
        ),
        BrowserTimeZoneContext::Missing =>
            "Browser time zone for this request: unavailable. Ask the user to clarify otherwise-unqualified dates and times.".to_owned(),
    }
}
