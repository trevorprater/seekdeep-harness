//! ISO-shaped timestamp formatting in resolved IANA zones.

use std::str::FromStr as _;

use chrono::{Datelike as _, Offset as _, TimeZone as _, Timelike as _, Utc};
use chrono_tz::Tz;
use icu_timezone::TimeZoneIdMapper;

/// Resolved formatter state captured when the plugin loads.
#[derive(Clone, Debug)]
pub struct TimestampFormatter {
    zone: Tz,
    resolved_name: String,
}

impl TimestampFormatter {
    /// Canonical zone label selected by the formatter.
    #[must_use]
    pub fn resolved_time_zone(&self) -> &str {
        &self.resolved_name
    }
}

/// Creates the timestamp formatter for an explicit zone or process fallback.
///
/// # Errors
///
/// Returns when the selected process or explicit zone is unsupported.
pub fn create_timestamp_formatter(time_zone: Option<&str>) -> anyhow::Result<TimestampFormatter> {
    let selected = match time_zone {
        Some(zone) => zone.to_owned(),
        None => process_time_zone()?,
    };
    let zone = Tz::from_str(&selected)
        .map_err(|_| anyhow::anyhow!("unsupported IANA time zone {selected:?}"))?;
    let resolved_name = canonical_time_zone_name(&selected)
        .ok_or_else(|| anyhow::anyhow!("unsupported IANA time zone {selected:?}"))?;
    Ok(TimestampFormatter {
        zone,
        resolved_name,
    })
}

pub(crate) fn canonical_time_zone_name(time_zone: &str) -> Option<String> {
    if time_zone.eq_ignore_ascii_case("MET") {
        return Some("Europe/Brussels".to_owned());
    }
    TimeZoneIdMapper::new()
        .as_borrowed()
        .canonicalize_iana(time_zone)
        .map(|(canonical, _)| intl_canonical_name(canonical.as_ref()).to_owned())
}

fn intl_canonical_name(time_zone: &str) -> &str {
    match time_zone {
        "Africa/Asmara" => "Africa/Asmera",
        "America/Argentina/Buenos_Aires" => "America/Buenos_Aires",
        "America/Argentina/Catamarca" => "America/Catamarca",
        "America/Argentina/Cordoba" => "America/Cordoba",
        "America/Argentina/Jujuy" => "America/Jujuy",
        "America/Argentina/Mendoza" => "America/Mendoza",
        "America/Atikokan" => "America/Coral_Harbour",
        "America/Indiana/Indianapolis" => "America/Indianapolis",
        "America/Kentucky/Louisville" => "America/Louisville",
        "America/Nuuk" => "America/Godthab",
        "Asia/Ho_Chi_Minh" => "Asia/Saigon",
        "Asia/Kathmandu" => "Asia/Katmandu",
        "Asia/Kolkata" => "Asia/Calcutta",
        "Asia/Yangon" => "Asia/Rangoon",
        "Atlantic/Faroe" => "Atlantic/Faeroe",
        "Etc/GMT" | "Etc/UTC" => "UTC",
        "Europe/Kyiv" => "Europe/Kiev",
        "Pacific/Chuuk" => "Pacific/Truk",
        "Pacific/Kanton" => "Pacific/Enderbury",
        "Pacific/Pohnpei" => "Pacific/Ponape",
        canonical => canonical,
    }
}

fn process_time_zone() -> anyhow::Result<String> {
    if let Some(zone) = std::env::var_os("TZ")
        && !zone.is_empty()
    {
        return zone
            .into_string()
            .map_err(|_| anyhow::anyhow!("process TZ is not valid UTF-8"));
    }
    Ok(iana_time_zone::get_timezone()?)
}

/// Formats epoch milliseconds as an ISO-shaped local timestamp plus zone.
///
/// # Errors
///
/// Returns when the epoch value lies outside Chrono's supported range.
pub fn format_timestamp(
    now_ms: i64,
    formatter: &TimestampFormatter,
    time_zone: &str,
) -> anyhow::Result<String> {
    let utc = Utc
        .timestamp_millis_opt(now_ms)
        .single()
        .ok_or_else(|| anyhow::anyhow!("timestamp {now_ms} is outside the supported range"))?;
    let local = utc.with_timezone(&formatter.zone);
    let seconds = local.offset().fix().local_minus_utc();
    let sign = if seconds < 0 { '-' } else { '+' };
    let magnitude = seconds.unsigned_abs();
    let hours = magnitude / 3_600;
    let minutes = magnitude % 3_600 / 60;
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{sign}{hours:02}:{minutes:02}[{time_zone}]",
        local.year(),
        local.month(),
        local.day(),
        local.hour(),
        local.minute(),
        local.second(),
    ))
}
