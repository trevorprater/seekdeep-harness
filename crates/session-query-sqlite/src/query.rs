//! Search request normalization, parameterized predicates, cursor identity, and snippets.

use seekdeep_core::session::{SessionId, SessionId as CoreSessionId};
use seekdeep_session_query::{
    SessionAvailability, SessionEventSurface, SessionQueryError, SessionQueryErrorCode,
    SessionResultFilter, SessionSearchCursor, materialize_session_result_filters,
    types::{SessionEventMetadataFilter, SessionEventSearchRequest, SessionSearchRequest},
};
use serde_json::{Value, json};

/// Collision-free marker inserted before an FTS5 match by `highlight()`.
pub const FTS_HIGHLIGHT_START: char = '\u{fdd0}';
/// Collision-free marker inserted after an FTS5 match by `highlight()`.
pub const FTS_HIGHLIGHT_END: char = '\u{fdd1}';
/// Largest page size whose one-row lookahead remains exactly representable by the source.
pub const SQLITE_MAX_PAGE_LIMIT: u64 = 9_007_199_254_740_990;
/// Portable `SQLite` host-parameter ceiling.
pub const SQLITE_PORTABLE_VARIABLE_LIMIT: usize = 32_766;
/// Supported FTS5 outer-predicate budget.
pub const SQLITE_FTS5_OUTER_PREDICATE_LIMIT: usize = 14;

/// Limit defaults used during request normalization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryLimits {
    /// Page size used when omitted.
    pub default_limit: u64,
    /// Largest accepted page size.
    pub max_limit: u64,
}

/// Normalized cross-session search request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedSessionRequest {
    /// Sanitized phrase query.
    pub query: String,
    /// Owned logical-session clauses.
    pub session_filters: Vec<SessionResultFilter>,
    /// Owned event metadata clauses.
    pub event_filters: Vec<SessionEventMetadataFilter>,
    /// Explicit page size.
    pub limit: u64,
    /// Optional opaque cursor.
    pub cursor: Option<SessionSearchCursor>,
}

/// Normalized within-session search request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedEventRequest {
    /// Target session.
    pub session_id: SessionId,
    /// Sanitized phrase query.
    pub query: String,
    /// Owned event metadata clauses.
    pub filters: Vec<SessionEventMetadataFilter>,
    /// Explicit page size.
    pub limit: u64,
    /// Optional opaque cursor.
    pub cursor: Option<SessionSearchCursor>,
}

/// One `SQLite` binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SqlParam {
    /// Text binding.
    Text(String),
    /// Non-negative integer binding.
    Integer(u64),
}

/// Parameterized SQL predicate fragment without a leading `WHERE`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SqlWhere {
    /// SQL fragment.
    pub sql: String,
    /// Bindings in placeholder order.
    pub params: Vec<SqlParam>,
    /// Compiled predicate count.
    pub predicate_count: usize,
}

/// Rejects prospective `SQLite` binding growth beyond the portable ceiling.
///
/// # Errors
///
/// Returns a typed invalid-filter failure above the ceiling.
pub fn assert_portable_binding_count(count: usize) -> anyhow::Result<()> {
    if count > SQLITE_PORTABLE_VARIABLE_LIMIT {
        return Err(query_error(
            format!(
                "session-search request exceeds SQLite's portable {SQLITE_PORTABLE_VARIABLE_LIMIT}-variable limit; reduce filter values"
            ),
            SessionQueryErrorCode::SessionQueryInvalidFilter,
        ));
    }
    Ok(())
}

/// Rejects compiled predicates beyond the supported FTS5 planner budget.
///
/// # Errors
///
/// Returns a typed invalid-filter failure above the ceiling.
pub fn assert_fts5_outer_predicate_count(count: usize) -> anyhow::Result<()> {
    if count > SQLITE_FTS5_OUTER_PREDICATE_LIMIT {
        return Err(query_error(
            format!(
                "session-search request exceeds the supported SQLite FTS5 outer-predicate budget of {SQLITE_FTS5_OUTER_PREDICATE_LIMIT}; reduce filters"
            ),
            SessionQueryErrorCode::SessionQueryInvalidFilter,
        ));
    }
    Ok(())
}

/// Validates and canonicalizes a cross-session request.
///
/// # Errors
///
/// Returns typed invalid-query, invalid-filter, or invalid-limit failures.
pub fn normalize_session_request(
    request: SessionSearchRequest,
    limits: QueryLimits,
) -> anyhow::Result<NormalizedSessionRequest> {
    Ok(NormalizedSessionRequest {
        query: normalize_query(&request.query)?,
        session_filters: materialize_session_result_filters(
            request.session_filters.as_deref().unwrap_or_default(),
        )?,
        event_filters: request.event_filters.unwrap_or_default(),
        limit: normalize_limit(request.limit, limits)?,
        cursor: request.cursor,
    })
}

/// Validates and canonicalizes a within-session request.
///
/// # Errors
///
/// Returns typed invalid-query, invalid-filter, or invalid-limit failures.
pub fn normalize_event_request(
    request: SessionEventSearchRequest,
    limits: QueryLimits,
) -> anyhow::Result<NormalizedEventRequest> {
    validate_metadata_ranges(request.filters.as_deref().unwrap_or_default())?;
    Ok(NormalizedEventRequest {
        session_id: request.session_id,
        query: normalize_query(&request.query)?,
        filters: request.filters.unwrap_or_default(),
        limit: normalize_limit(request.limit, limits)?,
        cursor: request.cursor,
    })
}

/// Compiles logical-session clauses against selected-document columns.
///
/// # Errors
///
/// Returns binding-budget, range, or predicate-budget failures.
pub fn build_session_where(filters: &[SessionResultFilter]) -> anyhow::Result<SqlWhere> {
    let filters = materialize_session_result_filters(filters)?;
    let mut clauses = Vec::new();
    let mut params = Vec::new();
    for filter in filters {
        match filter {
            SessionResultFilter::Id { values } => add_list(
                &mut clauses,
                &mut params,
                "session_id",
                values
                    .into_iter()
                    .map(|value| SqlParam::Text(value.as_str().to_owned()))
                    .collect(),
            )?,
            SessionResultFilter::Cwd { values } => {
                add_nullable_text_list(&mut clauses, &mut params, "cwd", &values)?;
            }
            SessionResultFilter::CreatedAt { from, to } => {
                add_range(&mut clauses, &mut params, "created_at", from, to)?;
            }
            SessionResultFilter::Parent { values } => {
                let values = values
                    .into_iter()
                    .map(|value| value.map(|value| value.as_str().to_owned()))
                    .collect::<Vec<_>>();
                add_nullable_text_list(&mut clauses, &mut params, "parent_session", &values)?;
            }
            SessionResultFilter::Availability { values } => {
                let live = values.contains(&SessionAvailability::Live);
                let persisted = values.contains(&SessionAvailability::Persisted);
                match (live, persisted) {
                    (false, false) => clauses.push("0".to_owned()),
                    (true, false) => clauses.push("live = 1".to_owned()),
                    (false, true) => clauses.push("persisted = 1".to_owned()),
                    (true, true) => {}
                }
            }
        }
    }
    assert_fts5_outer_predicate_count(clauses.len())?;
    Ok(SqlWhere {
        predicate_count: clauses.len(),
        sql: clauses.join(" AND "),
        params,
    })
}

/// Compiles event metadata clauses against selected-document columns.
///
/// # Errors
///
/// Returns binding-budget, range, or predicate-budget failures.
pub fn build_event_where(filters: &[SessionEventMetadataFilter]) -> anyhow::Result<SqlWhere> {
    validate_metadata_ranges(filters)?;
    let mut clauses = Vec::new();
    let mut params = Vec::new();
    for filter in filters {
        match filter {
            SessionEventMetadataFilter::Seq { from, to } => {
                add_range(&mut clauses, &mut params, "seq", *from, *to)?;
            }
            SessionEventMetadataFilter::Time { from, to } => {
                add_range(&mut clauses, &mut params, "time", *from, *to)?;
            }
            SessionEventMetadataFilter::Type { values } => add_list(
                &mut clauses,
                &mut params,
                "type",
                values.iter().cloned().map(SqlParam::Text).collect(),
            )?,
            SessionEventMetadataFilter::Surface { values } => add_list(
                &mut clauses,
                &mut params,
                "surface",
                values
                    .iter()
                    .map(|value| {
                        SqlParam::Text(match value {
                            SessionEventSurface::Current => "current".to_owned(),
                            SessionEventSurface::Shadowed => "shadowed".to_owned(),
                            SessionEventSurface::LogOnly => "log-only".to_owned(),
                        })
                    })
                    .collect(),
            )?,
        }
    }
    assert_fts5_outer_predicate_count(clauses.len())?;
    Ok(SqlWhere {
        predicate_count: clauses.len(),
        sql: clauses.join(" AND "),
        params,
    })
}

/// Quotes caller text as one FTS5 phrase so query syntax remains inert data.
#[must_use]
pub fn quote_fts_data(query: &str) -> String {
    format!("\"{}\"", query.replace('"', "\"\""))
}

/// Removes reserved marker collisions before text enters FTS5 or MATCH.
#[must_use]
pub fn sanitize_fts_text(text: &str) -> String {
    text.chars()
        .map(|character| {
            if matches!(character, '\0' | FTS_HIGHLIGHT_START | FTS_HIGHLIGHT_END) {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

/// Builds the stable normalized request identity stored in opaque cursors.
#[must_use]
pub fn request_fingerprint(request: &NormalizedRequest<'_>) -> String {
    let value = match request {
        NormalizedRequest::Sessions(request) => json!({
            "scope": "sessions",
            "query": request.query,
            "sessionFilters": canonical_session_filters(&request.session_filters),
            "eventFilters": canonical_event_filters(&request.event_filters),
            "limit": request.limit,
        }),
        NormalizedRequest::Events(request) => json!({
            "scope": "events",
            "sessionId": request.session_id.as_str(),
            "query": request.query,
            "filters": canonical_event_filters(&request.filters),
            "limit": request.limit,
        }),
    };
    value.to_string()
}

/// Borrowed normalized request accepted by [`request_fingerprint`].
#[derive(Clone, Copy, Debug)]
pub enum NormalizedRequest<'a> {
    /// Cross-session scope.
    Sessions(&'a NormalizedSessionRequest),
    /// Within-session scope.
    Events(&'a NormalizedEventRequest),
}

/// Builds a whitespace-normalized excerpt bounded by Unicode code points.
#[must_use]
pub fn make_snippet(marked_text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let (clean, match_start) = normalize_marked_text(marked_text);
    let characters = clean.chars().collect::<Vec<_>>();
    if characters.len() <= max_chars {
        return clean;
    }
    if max_chars == 1 {
        return "…".to_owned();
    }
    let matched = match_start.min(characters.len() - 1);
    let mut start = matched.saturating_sub(max_chars / 3);
    let prefix = if start > 0 { "…" } else { "" };
    let mut suffix = "…";
    let mut content_length = max_chars.saturating_sub(prefix.chars().count() + 1);
    if content_length < 1 {
        start = matched;
        suffix = "";
        content_length = max_chars.saturating_sub(prefix.chars().count());
    } else if matched >= start + content_length {
        start = matched - content_length + 1;
    }
    let mut end = characters.len().min(start + content_length);
    if end == characters.len() {
        suffix = "";
        content_length = max_chars.saturating_sub(prefix.chars().count());
        start = end.saturating_sub(content_length);
    }
    end = characters.len().min(start + content_length);
    format!(
        "{prefix}{}{suffix}",
        characters[start..end].iter().collect::<String>()
    )
}

fn normalize_query(value: &str) -> anyhow::Result<String> {
    let query = value.split_whitespace().collect::<Vec<_>>().join(" ");
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
    Ok(sanitize_fts_text(&query))
}

fn normalize_limit(value: Option<u64>, limits: QueryLimits) -> anyhow::Result<u64> {
    let limit = value.unwrap_or(limits.default_limit);
    let maximum = limits.max_limit.min(SQLITE_MAX_PAGE_LIMIT);
    if !(1..=maximum).contains(&limit) {
        return Err(query_error(
            format!("session-search limit must be an integer between 1 and {maximum}"),
            SessionQueryErrorCode::SessionQueryInvalidLimit,
        ));
    }
    Ok(limit)
}

fn validate_metadata_ranges(filters: &[SessionEventMetadataFilter]) -> anyhow::Result<()> {
    for filter in filters {
        let range = match filter {
            SessionEventMetadataFilter::Seq { from, to }
            | SessionEventMetadataFilter::Time { from, to } => Some((*from, *to)),
            SessionEventMetadataFilter::Type { .. }
            | SessionEventMetadataFilter::Surface { .. } => None,
        };
        if let Some((Some(from), Some(to))) = range
            && from > to
        {
            return Err(query_error(
                "session event range filter from must be less than or equal to to",
                SessionQueryErrorCode::SessionQueryInvalidFilter,
            ));
        }
    }
    Ok(())
}

fn add_list(
    clauses: &mut Vec<String>,
    params: &mut Vec<SqlParam>,
    column: &str,
    values: Vec<SqlParam>,
) -> anyhow::Result<()> {
    if values.is_empty() {
        clauses.push("0".to_owned());
        return Ok(());
    }
    assert_portable_binding_count(params.len() + values.len())?;
    clauses.push(format!(
        "{column} IN ({})",
        vec!["?"; values.len()].join(", ")
    ));
    params.extend(values);
    Ok(())
}

fn add_nullable_text_list(
    clauses: &mut Vec<String>,
    params: &mut Vec<SqlParam>,
    column: &str,
    values: &[Option<String>],
) -> anyhow::Result<()> {
    if values.is_empty() {
        clauses.push("0".to_owned());
        return Ok(());
    }
    let concrete = values.iter().filter_map(Clone::clone).collect::<Vec<_>>();
    let mut parts = Vec::new();
    if !concrete.is_empty() {
        assert_portable_binding_count(params.len() + concrete.len())?;
        parts.push(format!(
            "{column} IN ({})",
            vec!["?"; concrete.len()].join(", ")
        ));
        params.extend(concrete.into_iter().map(SqlParam::Text));
    }
    if values.iter().any(Option::is_none) {
        parts.push(format!("{column} IS NULL"));
    }
    clauses.push(format!("({})", parts.join(" OR ")));
    Ok(())
}

fn add_range(
    clauses: &mut Vec<String>,
    params: &mut Vec<SqlParam>,
    column: &str,
    from: Option<u64>,
    to: Option<u64>,
) -> anyhow::Result<()> {
    if let Some(from) = from {
        assert_portable_binding_count(params.len() + 1)?;
        clauses.push(format!("CAST({column} AS INTEGER) >= ?"));
        params.push(SqlParam::Integer(from));
    }
    if let Some(to) = to {
        assert_portable_binding_count(params.len() + 1)?;
        clauses.push(format!("CAST({column} AS INTEGER) <= ?"));
        params.push(SqlParam::Integer(to));
    }
    Ok(())
}

fn canonical_session_filters(filters: &[SessionResultFilter]) -> Vec<Value> {
    let mut values = filters
        .iter()
        .map(|filter| match filter {
            SessionResultFilter::Id { values } => {
                let mut values = values
                    .iter()
                    .map(CoreSessionId::as_str)
                    .collect::<Vec<_>>();
                values.sort_unstable();
                json!({"kind":"id","values":values})
            }
            SessionResultFilter::Cwd { values } => {
                json!({"kind":"cwd","values":sorted_nullable(values.iter().cloned())})
            }
            SessionResultFilter::CreatedAt { from, to } => {
                json!({"kind":"created-at","from":from,"to":to})
            }
            SessionResultFilter::Parent { values } => json!({
                "kind":"parent",
                "values":sorted_nullable(values.iter().map(|value| value.as_ref().map(|value| value.as_str().to_owned())))
            }),
            SessionResultFilter::Availability { values } => {
                let mut values = values
                    .iter()
                    .map(|value| match value {
                        SessionAvailability::Live => "live",
                        SessionAvailability::Persisted => "persisted",
                    })
                    .collect::<Vec<_>>();
                values.sort_unstable();
                json!({"kind":"availability","values":values})
            }
        })
        .collect::<Vec<_>>();
    sort_json(&mut values);
    values
}

fn canonical_event_filters(filters: &[SessionEventMetadataFilter]) -> Vec<Value> {
    let mut values = filters
        .iter()
        .map(|filter| match filter {
            SessionEventMetadataFilter::Seq { from, to } => {
                json!({"kind":"seq","from":from,"to":to})
            }
            SessionEventMetadataFilter::Time { from, to } => {
                json!({"kind":"time","from":from,"to":to})
            }
            SessionEventMetadataFilter::Type { values } => {
                let mut values = values.clone();
                values.sort();
                json!({"kind":"type","values":values})
            }
            SessionEventMetadataFilter::Surface { values } => {
                let mut values = values
                    .iter()
                    .map(|value| match value {
                        SessionEventSurface::Current => "current",
                        SessionEventSurface::Shadowed => "shadowed",
                        SessionEventSurface::LogOnly => "log-only",
                    })
                    .collect::<Vec<_>>();
                values.sort_unstable();
                json!({"kind":"surface","values":values})
            }
        })
        .collect::<Vec<_>>();
    sort_json(&mut values);
    values
}

fn sorted_nullable(values: impl Iterator<Item = Option<String>>) -> Vec<Option<String>> {
    let mut values = values.collect::<Vec<_>>();
    values.sort();
    values
}

fn sort_json(values: &mut [Value]) {
    values.sort_by_cached_key(|value| serde_json::to_string(value).expect("filter JSON"));
}

fn normalize_marked_text(marked_text: &str) -> (String, usize) {
    let mut characters = Vec::new();
    let mut match_start = None;
    for character in marked_text.chars() {
        if character == FTS_HIGHLIGHT_START {
            match_start.get_or_insert(characters.len());
        } else if character == FTS_HIGHLIGHT_END {
        } else if character.is_whitespace() {
            if !characters.is_empty() && characters.last() != Some(&' ') {
                characters.push(' ');
            }
        } else {
            characters.push(character);
        }
    }
    if characters.last() == Some(&' ') {
        characters.pop();
    }
    (characters.iter().collect(), match_start.unwrap_or(0))
}

fn query_error(message: impl Into<String>, code: SessionQueryErrorCode) -> anyhow::Error {
    SessionQueryError::new(message, code).into()
}
