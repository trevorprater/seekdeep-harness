//! Time constants, parsing, and formatting helpers.

use std::sync::{
    OnceLock,
    atomic::{AtomicI32, Ordering},
};

use chrono::{DateTime, Datelike, Local, TimeZone, Timelike};

/// One millisecond.
pub const MILLISECOND: f64 = 1.0;
/// One second in milliseconds.
pub const SECOND: f64 = 1_000.0;
/// One minute in milliseconds.
pub const MINUTE: f64 = SECOND * 60.0;
/// One hour in milliseconds.
pub const HOUR: f64 = MINUTE * 60.0;
/// One day in milliseconds.
pub const DAY: f64 = HOUR * 24.0;
/// One week in milliseconds.
pub const WEEK: f64 = DAY * 7.0;

static TIMEZONE_OFFSET: OnceLock<AtomicI32> = OnceLock::new();

fn timezone_offset_cell() -> &'static AtomicI32 {
    TIMEZONE_OFFSET.get_or_init(|| AtomicI32::new(-Local::now().offset().local_minus_utc() / 60))
}

/// Overrides the default timezone offset in minutes west of UTC.
pub fn set_timezone_offset(offset: i32) {
    timezone_offset_cell().store(offset, Ordering::Relaxed);
}

/// Returns the configured timezone offset in minutes west of UTC.
#[must_use]
pub fn get_timezone_offset() -> i32 {
    timezone_offset_cell().load(Ordering::Relaxed)
}

/// Maps an epoch millisecond timestamp to its timezone-adjusted day number.
#[must_use]
pub fn get_date_number(timestamp_millis: i64, offset: Option<i32>) -> i64 {
    let offset = i64::from(offset.unwrap_or_else(get_timezone_offset));
    (timestamp_millis.div_euclid(60_000) - offset).div_euclid(1_440)
}

/// Maps a timezone-adjusted day number back to epoch milliseconds.
#[must_use]
pub fn from_date_number(value: i64, offset: Option<i32>) -> i64 {
    let offset = i64::from(offset.unwrap_or_else(get_timezone_offset));
    value
        .saturating_mul(86_400_000)
        .saturating_add(offset.saturating_mul(60_000))
}

/// Parses concatenated week/day/hour/minute/second quantities.
///
/// # Panics
///
/// Panics only if the compile-time constant duration expression is invalid.
#[must_use]
pub fn parse_time(source: &str) -> f64 {
    static TIME: OnceLock<regex::Regex> = OnceLock::new();
    let expression = TIME.get_or_init(|| {
        regex::Regex::new(concat!(
            r"^(\d+(?:\.\d+)?w(?:eek(?:s)?)?)?",
            r"(\d+(?:\.\d+)?d(?:ay(?:s)?)?)?",
            r"(\d+(?:\.\d+)?h(?:our(?:s)?)?)?",
            r"(\d+(?:\.\d+)?m(?:in(?:ute)?(?:s)?)?)?",
            r"(\d+(?:\.\d+)?s(?:ec(?:ond)?(?:s)?)?)?$"
        ))
        .expect("static duration regex is valid")
    });
    let Some(captures) = expression.captures(source) else {
        return 0.0;
    };
    [WEEK, DAY, HOUR, MINUTE, SECOND]
        .into_iter()
        .enumerate()
        .map(|(index, unit)| {
            captures
                .get(index + 1)
                .and_then(|capture| leading_number(capture.as_str()))
                .map_or(0.0, |value| value * unit)
        })
        .sum()
}

fn leading_number(value: &str) -> Option<f64> {
    let length = value
        .char_indices()
        .take_while(|(_, character)| character.is_ascii_digit() || *character == '.')
        .map(|(index, character)| index + character.len_utf8())
        .last()?;
    value[..length].parse().ok()
}

/// Formats a duration using the source harness's threshold and rounding rules.
#[must_use]
pub fn format(milliseconds: f64) -> String {
    let absolute = milliseconds.abs();
    if absolute >= DAY - HOUR / 2.0 {
        format!("{}d", javascript_round(milliseconds / DAY))
    } else if absolute >= HOUR - MINUTE / 2.0 {
        format!("{}h", javascript_round(milliseconds / HOUR))
    } else if absolute >= MINUTE - SECOND / 2.0 {
        format!("{}m", javascript_round(milliseconds / MINUTE))
    } else if absolute >= SECOND {
        format!("{}s", javascript_round(milliseconds / SECOND))
    } else {
        format!("{}ms", javascript_number(milliseconds))
    }
}

fn javascript_round(value: f64) -> String {
    javascript_number((value + 0.5).floor())
}

fn javascript_number(value: f64) -> String {
    if value == 0.0 {
        "0".to_owned()
    } else if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}

/// Pads a number's source-like string representation on the left with zeroes.
#[must_use]
pub fn to_digits(source: i64, length: usize) -> String {
    let value = source.to_string();
    if value.len() >= length {
        value
    } else {
        format!("{}{value}", "0".repeat(length - value.len()))
    }
}

/// Replaces the first occurrence of each date token in source order.
#[must_use]
pub fn template(template: &str, time: DateTime<Local>) -> String {
    template
        .replacen("yyyy", &time.year().to_string(), 1)
        .replacen("yy", &time.year().to_string()[2..], 1)
        .replacen("MM", &to_digits(i64::from(time.month()), 2), 1)
        .replacen("dd", &to_digits(i64::from(time.day()), 2), 1)
        .replacen("hh", &to_digits(i64::from(time.hour()), 2), 1)
        .replacen("mm", &to_digits(i64::from(time.minute()), 2), 1)
        .replacen("ss", &to_digits(i64::from(time.second()), 2), 1)
        .replacen(
            "SSS",
            &to_digits(i64::from(time.timestamp_subsec_millis()), 3),
            1,
        )
}

/// Returns the current local timestamp in epoch milliseconds.
#[must_use]
pub fn now_millis() -> i64 {
    Local::now().timestamp_millis()
}

/// Builds a local timestamp when every calendar field is valid and unambiguous.
#[must_use]
pub fn local_timestamp_millis(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Option<i64> {
    Local
        .with_ymd_and_hms(year, month, day, hour, minute, second)
        .single()
        .map(|value| value.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ordered_duration_vocabulary() {
        let expected = WEEK + 2.0 * DAY + 3.0 * HOUR + 4.0 * MINUTE + 5.0 * SECOND;
        assert!((parse_time("1week2days3h4min5sec") - expected).abs() < f64::EPSILON);
        assert!(parse_time("1h2d").abs() < f64::EPSILON);
        assert!(parse_time("").abs() < f64::EPSILON);
    }

    #[test]
    fn formats_around_exact_source_thresholds() {
        assert_eq!(format(999.0), "999ms");
        assert_eq!(format(1_500.0), "2s");
        assert_eq!(format(-1_500.0), "-1s");
        assert_eq!(format(MINUTE - SECOND / 2.0), "1m");
    }

    #[test]
    fn date_number_uses_floor_for_negative_epochs() {
        assert_eq!(get_date_number(-1, Some(0)), -1);
        assert_eq!(from_date_number(-1, Some(0)), -86_400_000);
    }
}
