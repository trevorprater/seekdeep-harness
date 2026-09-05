//! Model argument schemas, normalization, and filter construction.

use seekdeep_core::session::SessionId;
use seekdeep_session_query::{
    SessionAvailability, SessionEventSurface, SessionQueryError, SessionQueryErrorCode,
    SessionResultBound, SessionResultFilter, normalize_session_query_whitespace,
    types::SessionEventMetadataFilter,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Arguments accepted by `session_search`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSearchArgs {
    /// Literal full-text query.
    pub query: String,
    /// Optional included session ids.
    pub session_ids: Option<Vec<String>>,
    /// Inclusive creation-time lower bound.
    pub created_at_from: Option<String>,
    /// Inclusive creation-time upper bound.
    pub created_at_to: Option<String>,
    /// Optional direct parent ids.
    pub parent_session_ids: Option<Vec<String>>,
    /// Whether roots join the parent clause.
    pub include_root_sessions: Option<bool>,
    /// Required source availability alternatives.
    pub availability: Option<Vec<SessionAvailability>>,
    /// Inclusive event sequence lower bound.
    pub event_seq_from: Option<i64>,
    /// Inclusive event sequence upper bound.
    pub event_seq_to: Option<i64>,
    /// Inclusive event-time lower bound.
    pub event_time_from: Option<String>,
    /// Inclusive event-time upper bound.
    pub event_time_to: Option<String>,
    /// Included event types.
    pub event_types: Option<Vec<String>>,
    /// Included event surfaces.
    pub event_surfaces: Option<Vec<SessionEventSurface>>,
}

/// Arguments accepted by `session_event_search`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventSearchArgs {
    /// Target session, defaulting to the caller.
    pub session_id: Option<String>,
    /// Literal full-text query.
    pub query: String,
    /// Inclusive event sequence lower bound.
    pub seq_from: Option<i64>,
    /// Inclusive event sequence upper bound.
    pub seq_to: Option<i64>,
    /// Inclusive event-time lower bound.
    pub time_from: Option<String>,
    /// Inclusive event-time upper bound.
    pub time_to: Option<String>,
    /// Included event types.
    pub event_types: Option<Vec<String>>,
    /// Included surfaces.
    pub surfaces: Option<Vec<SessionEventSurface>>,
}

/// Optional target session arguments.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionTargetArgs {
    /// Target session, defaulting to the caller.
    pub session_id: Option<String>,
}

/// One target event.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventTargetArgs {
    /// Target session, defaulting to the caller.
    pub session_id: Option<String>,
    /// Target sequence.
    pub seq: i64,
}

/// One target event plus neighboring window sizes.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventReadArgs {
    /// Target session, defaulting to the caller.
    pub session_id: Option<String>,
    /// Target sequence.
    pub seq: i64,
    /// Preceding event count.
    pub before: Option<i64>,
    /// Following event count.
    pub after: Option<i64>,
}

/// Owned event filter input shared by both search scopes.
#[derive(Clone, Copy)]
pub struct EventFilterInput<'a> {
    /// Sequence lower bound.
    pub seq_from: Option<i64>,
    /// Sequence upper bound.
    pub seq_to: Option<i64>,
    /// Timestamp lower bound.
    pub time_from: Option<&'a str>,
    /// Timestamp upper bound.
    pub time_to: Option<&'a str>,
    /// Event types.
    pub event_types: Option<&'a [String]>,
    /// Surfaces.
    pub surfaces: Option<&'a [SessionEventSurface]>,
}

/// Exact source schemas for the five tools.
pub fn session_search_parameters() -> Map<String, Value> {
    object_properties(json!({
        "query": {"type":"string","required":true,"description":"Literal full-text query over prior session history."},
        "session_ids": {"type":"array","items":{"type":"string"},"description":"Optional session ids to include."},
        "created_at_from": {"type":"string","description":"Inclusive timezone-qualified ISO 8601 creation-time lower bound."},
        "created_at_to": {"type":"string","description":"Inclusive timezone-qualified ISO 8601 creation-time upper bound."},
        "parent_session_ids": {"type":"array","items":{"type":"string"},"description":"Optional direct parent session ids."},
        "include_root_sessions": {"type":"boolean","description":"Include sessions with no parent in the parent filter."},
        "availability": {"type":"array","items":{"type":"string","enum":["live","persisted"]},"description":"Require at least one selected source availability."},
        "event_seq_from": {"type":"integer","description":"Inclusive event sequence lower bound."},
        "event_seq_to": {"type":"integer","description":"Inclusive event sequence upper bound."},
        "event_time_from": {"type":"string","description":"Inclusive timezone-qualified ISO 8601 event-time lower bound."},
        "event_time_to": {"type":"string","description":"Inclusive timezone-qualified ISO 8601 event-time upper bound."},
        "event_types": {"type":"array","items":{"type":"string"},"description":"Event types to include."},
        "event_surfaces": {"type":"array","items":{"type":"string","enum":["current","shadowed","log-only"]},"description":"Event surfaces to include."}
    }))
}

/// Exact `session_event_search` parameter schema.
pub fn event_search_parameters() -> Map<String, Value> {
    object_properties(json!({
        "session_id": {"type":"string","description":"Target session id. Omit for the current session."},
        "query": {"type":"string","required":true,"description":"Literal full-text query over the target session."},
        "seq_from": {"type":"integer","description":"Inclusive event sequence lower bound."},
        "seq_to": {"type":"integer","description":"Inclusive event sequence upper bound."},
        "time_from": {"type":"string","description":"Inclusive timezone-qualified ISO 8601 event-time lower bound."},
        "time_to": {"type":"string","description":"Inclusive timezone-qualified ISO 8601 event-time upper bound."},
        "event_types": {"type":"array","items":{"type":"string"},"description":"Event types to include."},
        "surfaces": {"type":"array","items":{"type":"string","enum":["current","shadowed","log-only"]},"description":"Event surfaces to include."}
    }))
}

/// Shared optional target parameter.
pub fn target_session_parameters() -> Map<String, Value> {
    object_properties(json!({
        "session_id": {"type":"string","description":"Target session id. Omit for the current session."}
    }))
}

fn object_properties(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(properties) => properties,
        _ => unreachable!("schema literal is an object"),
    }
}

/// Builds session filters except workspace and parent authority clauses.
///
/// # Errors
///
/// Returns typed invalid-filter failures for empty arrays or timestamp ranges.
pub fn build_session_filters(args: &SessionSearchArgs) -> anyhow::Result<Vec<SessionResultFilter>> {
    let mut filters = Vec::new();
    if let Some(values) = &args.session_ids {
        assert_non_empty_array("session_ids", values)?;
        filters.push(SessionResultFilter::Id {
            values: values.iter().map(SessionId::new).collect(),
        });
    }
    if let Some((from, to)) = timestamp_range(
        "created_at",
        args.created_at_from.as_deref(),
        args.created_at_to.as_deref(),
    )? {
        filters.push(SessionResultFilter::CreatedAt { from, to });
    }
    if let Some(values) = &args.availability {
        assert_non_empty_array("availability", values)?;
        filters.push(SessionResultFilter::Availability {
            values: values.clone(),
        });
    }
    Ok(filters)
}

/// Deduplicates requested parent identities after rejecting an empty clause.
///
/// # Errors
///
/// Returns a typed invalid-filter failure for a supplied empty list.
pub fn materialize_parent_session_ids(
    values: Option<&[String]>,
) -> anyhow::Result<Option<Vec<SessionId>>> {
    let Some(values) = values else {
        return Ok(None);
    };
    assert_non_empty_array("parent_session_ids", values)?;
    let mut seen = std::collections::HashSet::new();
    Ok(Some(
        values
            .iter()
            .map(SessionId::new)
            .filter(|id| seen.insert(id.clone()))
            .collect(),
    ))
}

/// Builds provider metadata filters from model arguments.
///
/// # Errors
///
/// Returns typed invalid-filter failures for invalid ranges or empty lists.
pub fn build_event_filters(
    input: EventFilterInput<'_>,
) -> anyhow::Result<Vec<SessionEventMetadataFilter>> {
    let mut filters = Vec::new();
    let (from, to) = sequence_range(input.seq_from, input.seq_to)?;
    if from.is_some() || to.is_some() {
        filters.push(SessionEventMetadataFilter::Seq { from, to });
    }
    if let Some((from, to)) = timestamp_range("time", input.time_from, input.time_to)? {
        filters.push(SessionEventMetadataFilter::Time { from, to });
    }
    if let Some(values) = input.event_types {
        assert_non_empty_array("event_types", values)?;
        filters.push(SessionEventMetadataFilter::Type {
            values: values.to_vec(),
        });
    }
    if let Some(values) = input.surfaces {
        assert_non_empty_array("surfaces", values)?;
        filters.push(SessionEventMetadataFilter::Surface {
            values: values.to_vec(),
        });
    }
    Ok(filters)
}

/// Normalizes one literal query before any asynchronous authority work.
///
/// # Errors
///
/// Returns a typed invalid-query failure for blank or NUL-bearing text.
pub fn normalize_query(value: &str) -> anyhow::Result<String> {
    let query = normalize_session_query_whitespace(value);
    if query.is_empty() {
        return Err(query_error(
            "session-search query must contain non-whitespace text",
            SessionQueryErrorCode::SessionQueryInvalidQuery,
        ));
    }
    if query.contains('\0') {
        return Err(query_error(
            "session-search query must not contain NUL",
            SessionQueryErrorCode::SessionQueryInvalidQuery,
        ));
    }
    Ok(query)
}

/// Validates and materializes inclusive sequence bounds.
///
/// # Errors
///
/// Returns a typed invalid-filter failure for unsafe, negative, or inverted bounds.
pub fn sequence_range(
    from: Option<i64>,
    to: Option<i64>,
) -> anyhow::Result<(Option<SessionResultBound>, Option<SessionResultBound>)> {
    let from = from
        .map(|value| non_negative_safe("sequence lower bound", value))
        .transpose()?;
    let to = to
        .map(|value| non_negative_safe("sequence upper bound", value))
        .transpose()?;
    if from.zip(to).is_some_and(|(from, to)| from > to) {
        return Err(invalid_range(
            "sequence",
            "from must be less than or equal to to",
        ));
    }
    Ok((
        from.map(SessionResultBound::from),
        to.map(SessionResultBound::from),
    ))
}

/// Converts one model integer to a safe unsigned value.
///
/// # Errors
///
/// Returns a typed invalid-filter failure outside the non-negative safe range.
pub fn non_negative_safe(name: &str, value: i64) -> anyhow::Result<u64> {
    if !(0..=MAX_SAFE_INTEGER).contains(&value) {
        return Err(query_error(
            format!("{name} must be a non-negative safe integer"),
            SessionQueryErrorCode::SessionQueryInvalidFilter,
        ));
    }
    u64::try_from(value).map_err(|_| {
        query_error(
            format!("{name} must be a non-negative safe integer"),
            SessionQueryErrorCode::SessionQueryInvalidFilter,
        )
    })
}

fn timestamp_range(
    name: &str,
    from: Option<&str>,
    to: Option<&str>,
) -> anyhow::Result<Option<(Option<SessionResultBound>, Option<SessionResultBound>)>> {
    if from.is_none() && to.is_none() {
        return Ok(None);
    }
    let from = from
        .map(|value| parse_iso_timestamp(&format!("{name}_from"), value))
        .transpose()?;
    let to = to
        .map(|value| parse_iso_timestamp(&format!("{name}_to"), value))
        .transpose()?;
    if from
        .as_ref()
        .zip(to.as_ref())
        .is_some_and(|(from, to)| compare_timestamps(from, to).is_gt())
    {
        return Err(invalid_range(name, "from must be less than or equal to to"));
    }
    Ok(Some((
        from.map(|value| {
            SessionResultBound::new(timestamp_lower_bound(&value)).expect("finite timestamp")
        }),
        to.map(|value| {
            SessionResultBound::new(timestamp_upper_bound(&value)).expect("finite timestamp")
        }),
    )))
}

#[derive(Clone, Debug)]
struct ExactTimestamp {
    millisecond: i64,
    remainder: String,
}

fn parse_iso_timestamp(name: &str, value: &str) -> anyhow::Result<ExactTimestamp> {
    let (head, zone) = split_zone(value).ok_or_else(|| {
        invalid_iso(
            name,
            "must be an ISO 8601 timestamp with Z or a numeric offset",
        )
    })?;
    let fraction_split = head.split_once('.');
    let (date_time, fraction) =
        fraction_split.map_or((head, ""), |(base, fraction)| (base, fraction));
    if fraction_split.is_some() && fraction.is_empty()
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid_iso(
            name,
            "must be an ISO 8601 timestamp with Z or a numeric offset",
        ));
    }
    let (date, clock) = date_time.split_once('T').ok_or_else(|| {
        invalid_iso(
            name,
            "must be an ISO 8601 timestamp with Z or a numeric offset",
        )
    })?;
    let mut date_parts = date.split('-');
    let component = |value, width| {
        parse_component(value, width)
            .ok_or_else(|| invalid_iso(name, "must be a valid ISO 8601 timestamp"))
    };
    let year = component(date_parts.next(), 4)?;
    let month = component(date_parts.next(), 2)?;
    let day = component(date_parts.next(), 2)?;
    if date_parts.next().is_some() {
        return Err(invalid_iso(name, "must be a valid ISO 8601 timestamp"));
    }
    let time_parts = clock.split(':').collect::<Vec<_>>();
    if !(2..=3).contains(&time_parts.len()) || !fraction.is_empty() && time_parts.len() != 3 {
        return Err(invalid_iso(name, "must be a valid ISO 8601 timestamp"));
    }
    let hour = component(time_parts.first().copied(), 2)?;
    let minute = component(time_parts.get(1).copied(), 2)?;
    let second = time_parts
        .get(2)
        .map_or(Ok(0), |part| component(Some(part), 2))?;
    let date = chrono::NaiveDate::from_ymd_opt(
        year,
        u32::try_from(month).expect("numeric month"),
        u32::try_from(day).expect("numeric day"),
    )
    .ok_or_else(|| invalid_iso(name, "must be a valid ISO 8601 timestamp"))?;
    let time = chrono::NaiveTime::from_hms_milli_opt(
        u32::try_from(hour).expect("numeric hour"),
        u32::try_from(minute).expect("numeric minute"),
        u32::try_from(second).expect("numeric second"),
        fraction
            .get(..fraction.len().min(3))
            .unwrap_or_default()
            .parse::<u32>()
            .unwrap_or(0)
            * 10_u32
                .pow(u32::try_from(3_usize.saturating_sub(fraction.len().min(3))).expect("power")),
    )
    .ok_or_else(|| invalid_iso(name, "must be a valid ISO 8601 timestamp"))?;
    let offset = parse_offset(zone)
        .ok_or_else(|| invalid_iso(name, "must be a valid ISO 8601 timestamp"))?;
    let local = chrono::NaiveDateTime::new(date, time);
    let millisecond = local
        .and_utc()
        .timestamp_millis()
        .checked_sub(i64::from(offset) * 1_000)
        .ok_or_else(|| invalid_iso(name, "must be a valid ISO 8601 timestamp"))?;
    if millisecond.unsigned_abs() > u64::try_from(MAX_SAFE_INTEGER).expect("safe maximum") {
        return Err(invalid_iso(name, "must be a valid ISO 8601 timestamp"));
    }
    Ok(ExactTimestamp {
        millisecond,
        remainder: fraction
            .get(3..)
            .unwrap_or_default()
            .trim_end_matches('0')
            .to_owned(),
    })
}

fn split_zone(value: &str) -> Option<(&str, &str)> {
    if let Some(head) = value.strip_suffix('Z') {
        return Some((head, "Z"));
    }
    let position = value.rfind(['+', '-'])?;
    (position > 10).then(|| value.split_at(position))
}

fn parse_offset(zone: &str) -> Option<i32> {
    if zone == "Z" {
        return Some(0);
    }
    let sign = match zone.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let (hours, minutes) = zone.get(1..)?.split_once(':')?;
    if hours.len() != 2 || minutes.len() != 2 {
        return None;
    }
    let hours = hours.parse::<i32>().ok()?;
    let minutes = minutes.parse::<i32>().ok()?;
    (hours <= 23 && minutes <= 59).then_some(sign * (hours * 3_600 + minutes * 60))
}

fn parse_component(value: Option<&str>, width: usize) -> Option<i32> {
    let value = value
        .filter(|value| value.len() == width && value.bytes().all(|byte| byte.is_ascii_digit()))?;
    value.parse().ok()
}

fn compare_timestamps(left: &ExactTimestamp, right: &ExactTimestamp) -> std::cmp::Ordering {
    left.millisecond.cmp(&right.millisecond).then_with(|| {
        let length = left.remainder.len().max(right.remainder.len());
        (0..length)
            .map(|index| {
                (
                    left.remainder
                        .as_bytes()
                        .get(index)
                        .copied()
                        .unwrap_or(b'0'),
                    right
                        .remainder
                        .as_bytes()
                        .get(index)
                        .copied()
                        .unwrap_or(b'0'),
                )
            })
            .find_map(|(left, right)| (left != right).then(|| left.cmp(&right)))
            .unwrap_or(std::cmp::Ordering::Equal)
    })
}

fn timestamp_lower_bound(value: &ExactTimestamp) -> f64 {
    let millisecond = SessionResultBound::from(value.millisecond).value();
    if value.remainder.is_empty() {
        millisecond
    } else {
        next_up(millisecond)
    }
}

fn timestamp_upper_bound(value: &ExactTimestamp) -> f64 {
    let millisecond = SessionResultBound::from(value.millisecond).value();
    if value.remainder.is_empty() {
        millisecond
    } else {
        next_down(millisecond + 1.0)
    }
}

fn next_up(value: f64) -> f64 {
    if value == 0.0 {
        return f64::from_bits(1);
    }
    f64::from_bits(if value > 0.0 {
        value.to_bits() + 1
    } else {
        value.to_bits() - 1
    })
}

fn next_down(value: f64) -> f64 {
    if value == 0.0 {
        return -f64::from_bits(1);
    }
    f64::from_bits(if value > 0.0 {
        value.to_bits() - 1
    } else {
        value.to_bits() + 1
    })
}

fn invalid_iso(name: &str, detail: &str) -> anyhow::Error {
    invalid_range(name, detail)
}

fn invalid_range(name: &str, detail: &str) -> anyhow::Error {
    query_error(
        format!("session {name} range {detail}"),
        SessionQueryErrorCode::SessionQueryInvalidFilter,
    )
}

fn assert_non_empty_array<T>(name: &str, values: &[T]) -> anyhow::Result<()> {
    if values.is_empty() {
        return Err(query_error(
            format!("{name} must contain at least one value when supplied"),
            SessionQueryErrorCode::SessionQueryInvalidFilter,
        ));
    }
    Ok(())
}

fn query_error(message: impl Into<String>, code: SessionQueryErrorCode) -> anyhow::Error {
    SessionQueryError::new(message, code).into()
}
