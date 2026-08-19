//! Pure provider-independent predicates for logical sessions and event text.

use regex::Regex;

use crate::{
    config::{SessionQueryError, SessionQueryErrorCode},
    types::{
        SessionAvailability, SessionEventResultFilter, SessionEventSearchDocument, SessionRecord,
        SessionResultFilter, SessionResultRange,
    },
};

type SessionPredicate = Box<dyn Fn(&SessionRecord) -> bool>;
type EventPredicate = Box<dyn Fn(&SessionEventSearchDocument) -> bool>;

/// Applies logical-session filters combined with `AND` while preserving input order.
///
/// # Errors
///
/// Returns the first invalid clause, range, or text filter encountered.
pub fn filter_session_results(
    records: &[SessionRecord],
    filters: &[SessionResultFilter],
) -> Result<Vec<SessionRecord>, SessionQueryError> {
    let predicates = filters
        .iter()
        .map(session_predicate)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(records
        .iter()
        .filter(|record| predicates.iter().all(|predicate| predicate(record)))
        .cloned()
        .collect())
}

/// Applies event filters combined with `AND` to extracted semantic documents.
///
/// # Errors
///
/// Returns the first invalid clause, range, or text filter encountered.
pub fn filter_session_event_documents(
    documents: &[SessionEventSearchDocument],
    filters: &[SessionEventResultFilter],
) -> Result<Vec<SessionEventSearchDocument>, SessionQueryError> {
    let predicates = filters
        .iter()
        .map(event_predicate)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(documents
        .iter()
        .filter(|document| predicates.iter().all(|predicate| predicate(document)))
        .cloned()
        .collect())
}

/// Copies and validates logical-session filters before an asynchronous boundary.
///
/// The Rust filter vocabulary is already statically typed, so this retains the
/// source's range-ordering validation while cloning each clause into caller-
/// owned storage.
///
/// # Errors
///
/// Returns when a `created-at` clause has an inverted range.
pub fn materialize_session_result_filters(
    filters: &[SessionResultFilter],
) -> Result<Vec<SessionResultFilter>, SessionQueryError> {
    filters
        .iter()
        .map(|filter| {
            if let SessionResultFilter::CreatedAt { from, to } = filter {
                validate_range("created-at", *from, *to)?;
            }
            Ok(filter.clone())
        })
        .collect()
}

/// Copies and validates event filters before an asynchronous boundary.
///
/// # Errors
///
/// Returns when a `seq` or `time` clause has an inverted range.
pub fn materialize_session_event_result_filters(
    filters: &[SessionEventResultFilter],
) -> Result<Vec<SessionEventResultFilter>, SessionQueryError> {
    filters
        .iter()
        .map(|filter| {
            match filter {
                SessionEventResultFilter::Seq { from, to } => validate_range("seq", *from, *to)?,
                SessionEventResultFilter::Time { from, to } => {
                    validate_range("time", *from, *to)?;
                }
                _ => {}
            }
            Ok(filter.clone())
        })
        .collect()
}

/// Compiles a literal case-insensitive, whitespace-flexible semantic-text match.
///
/// # Errors
///
/// Returns an invalid-filter failure when the input is all whitespace.
pub fn compile_session_text_filter(text: &str) -> Result<Regex, SessionQueryError> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(SessionQueryError::new(
            "session text filter must contain non-whitespace text",
            SessionQueryErrorCode::SessionQueryInvalidFilter,
        ));
    }
    let pattern = trimmed
        .split_whitespace()
        .map(regex::escape)
        .collect::<Vec<_>>()
        .join("\\s+");
    Regex::new(&format!("(?i){pattern}")).map_err(|error| {
        SessionQueryError::new(
            format!("session text filter is invalid: {error}"),
            SessionQueryErrorCode::SessionQueryInvalidFilter,
        )
    })
}

fn session_predicate(filter: &SessionResultFilter) -> Result<SessionPredicate, SessionQueryError> {
    match filter {
        SessionResultFilter::Id { values } => {
            let values = values.clone();
            Ok(Box::new(move |record| values.contains(&record.header.id)))
        }
        SessionResultFilter::Cwd { values } => {
            let values = values.clone();
            Ok(Box::new(move |record| values.contains(&record.header.cwd)))
        }
        SessionResultFilter::CreatedAt { from, to } => {
            let range = validated_range("created-at", *from, *to)?;
            Ok(Box::new(move |record| {
                matches_range(i128::from(record.header.created_at), &range)
            }))
        }
        SessionResultFilter::Parent { values } => {
            let values = values.clone();
            Ok(Box::new(move |record| {
                values.contains(&record.header.parent_session)
            }))
        }
        SessionResultFilter::Availability { values } => {
            let values = values.clone();
            Ok(Box::new(move |record| {
                values.iter().any(|value| match value {
                    SessionAvailability::Live => record.live,
                    SessionAvailability::Persisted => record.persisted,
                })
            }))
        }
    }
}

fn event_predicate(filter: &SessionEventResultFilter) -> Result<EventPredicate, SessionQueryError> {
    match filter {
        SessionEventResultFilter::Seq { from, to } => {
            let range = validated_range("seq", *from, *to)?;
            Ok(Box::new(move |document| {
                matches_range(i128::from(document.record.seq), &range)
            }))
        }
        SessionEventResultFilter::Time { from, to } => {
            let range = validated_range("time", *from, *to)?;
            Ok(Box::new(move |document| {
                matches_range(i128::from(document.record.time), &range)
            }))
        }
        SessionEventResultFilter::Type { values } => {
            let values = values.clone();
            Ok(Box::new(move |document| {
                values.contains(&document.record.event_type)
            }))
        }
        SessionEventResultFilter::Surface { values } => {
            let values = values.clone();
            Ok(Box::new(move |document| {
                values.contains(&document.record.surface)
            }))
        }
        SessionEventResultFilter::Text { text } => {
            let pattern = compile_session_text_filter(text)?;
            Ok(Box::new(move |document| pattern.is_match(&document.text)))
        }
    }
}

fn validated_range(
    name: &str,
    from: Option<u64>,
    to: Option<u64>,
) -> Result<SessionResultRange, SessionQueryError> {
    validate_range(name, from, to)?;
    Ok(SessionResultRange { from, to })
}

fn validate_range(name: &str, from: Option<u64>, to: Option<u64>) -> Result<(), SessionQueryError> {
    if let (Some(from), Some(to)) = (from, to)
        && from > to
    {
        return Err(invalid_range(name, "from must be less than or equal to to"));
    }
    Ok(())
}

fn matches_range(value: i128, range: &SessionResultRange) -> bool {
    range.from.is_none_or(|from| value >= i128::from(from))
        && range.to.is_none_or(|to| value <= i128::from(to))
}

fn invalid_range(name: &str, detail: &str) -> SessionQueryError {
    invalid_filter(&format!("{name} filter {detail}"))
}

fn invalid_filter(detail: &str) -> SessionQueryError {
    SessionQueryError::new(
        format!("session {detail}"),
        SessionQueryErrorCode::SessionQueryInvalidFilter,
    )
}

#[cfg(test)]
mod tests {
    use seekdeep_core::session::{SessionHeader, SessionId};

    use super::*;
    use crate::types::{SessionEventRecord, SessionEventSurface};

    fn header(id: &str, created_at: u64) -> SessionHeader {
        SessionHeader {
            version: 0,
            id: SessionId::new(id),
            created_at,
            cwd: None,
            parent_session: None,
            seed_length: None,
            origin: None,
            delegation_depth: None,
            agent_preset: None,
        }
    }

    fn record(id: &str, created_at: u64, live: bool, persisted: bool) -> SessionRecord {
        SessionRecord {
            header: header(id, created_at),
            live,
            persisted,
        }
    }

    fn document(
        seq: u64,
        event_type: &str,
        text: &str,
        surface: SessionEventSurface,
    ) -> SessionEventSearchDocument {
        SessionEventSearchDocument {
            record: SessionEventRecord {
                session_id: SessionId::new("s"),
                seq,
                event_type: event_type.to_owned(),
                time: i64::try_from(seq).expect("seq fits i64"),
                surface,
            },
            text: text.to_owned(),
        }
    }

    #[test]
    fn session_clauses_and_and_apply_with_or_values() {
        let records = vec![record("a", 10, true, false), record("b", 20, false, true)];
        let filters = [
            SessionResultFilter::Id {
                values: vec![SessionId::new("a"), SessionId::new("x")],
            },
            SessionResultFilter::CreatedAt {
                from: Some(5),
                to: Some(15),
            },
            SessionResultFilter::Availability {
                values: vec![SessionAvailability::Live],
            },
        ];
        assert_eq!(
            filter_session_results(&records, &filters).expect("filter"),
            vec![records[0].clone()]
        );
        assert_eq!(
            filter_session_results(&records, &[SessionResultFilter::Cwd { values: vec![None] }],)
                .expect("cwd"),
            records
        );
        assert_eq!(
            filter_session_results(&records, &[]).expect("empty filters"),
            records
        );
    }

    #[test]
    fn event_text_filter_is_case_insensitive_and_regex_safe() {
        let documents = vec![document(
            0,
            "user/message",
            "Hello\n(AI)+",
            SessionEventSurface::Current,
        )];
        let matches = filter_session_event_documents(
            &documents,
            &[SessionEventResultFilter::Text {
                text: "hello   (ai)+".to_owned(),
            }],
        )
        .expect("text filter");
        assert_eq!(matches, documents);
        assert!(
            compile_session_text_filter("CAFÉ")
                .expect("compile")
                .is_match("café")
        );
        assert!(
            compile_session_text_filter(" \n ")
                .expect_err("whitespace text")
                .code
                == SessionQueryErrorCode::SessionQueryInvalidFilter
        );
    }

    #[test]
    fn materializers_reject_inverted_ranges() {
        let error = materialize_session_result_filters(&[SessionResultFilter::CreatedAt {
            from: Some(10),
            to: Some(5),
        }])
        .expect_err("inverted created-at");
        assert_eq!(error.code, SessionQueryErrorCode::SessionQueryInvalidFilter);
        let error = materialize_session_event_result_filters(&[SessionEventResultFilter::Time {
            from: Some(2),
            to: Some(1),
        }])
        .expect_err("inverted time");
        assert_eq!(error.code, SessionQueryErrorCode::SessionQueryInvalidFilter);
        let kept = materialize_session_result_filters(&[SessionResultFilter::CreatedAt {
            from: None,
            to: Some(2),
        }])
        .expect("valid range");
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn surface_and_type_clauses_are_orred() {
        let documents = vec![
            document(0, "user/message", "a", SessionEventSurface::Shadowed),
            document(1, "tool/result", "b", SessionEventSurface::Current),
        ];
        let matches = filter_session_event_documents(
            &documents,
            &[
                SessionEventResultFilter::Type {
                    values: vec!["user/message".to_owned(), "tool/result".to_owned()],
                },
                SessionEventResultFilter::Surface {
                    values: vec![SessionEventSurface::Shadowed],
                },
            ],
        )
        .expect("filters");
        assert_eq!(matches, vec![documents[0].clone()]);
        let none = filter_session_event_documents(
            &documents,
            &[SessionEventResultFilter::Surface { values: vec![] }],
        )
        .expect("empty surface values");
        assert!(none.is_empty());
    }
}
