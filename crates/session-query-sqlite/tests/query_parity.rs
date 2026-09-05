//! Differential request, SQL, fingerprint, and snippet parity.

use seekdeep_core::session::SessionId;
use seekdeep_session_query::{
    SessionAvailability, SessionEventSurface, SessionQueryError, SessionQueryErrorCode,
    SessionResultFilter, SessionSearchCursor,
    types::{SessionEventMetadataFilter, SessionEventSearchRequest, SessionSearchRequest},
};
use seekdeep_session_query_sqlite::query::{
    FTS_HIGHLIGHT_END, FTS_HIGHLIGHT_START, NormalizedEventRequest, NormalizedRequest,
    NormalizedSessionRequest, QueryLimits, SQLITE_FTS5_OUTER_PREDICATE_LIMIT,
    SQLITE_MAX_PAGE_LIMIT, SQLITE_PORTABLE_VARIABLE_LIMIT, SqlParam, build_event_where,
    build_session_where, make_snippet, normalize_event_request, normalize_session_request,
    quote_fts_data, request_fingerprint, sanitize_fts_text,
};

const LIMITS: QueryLimits = QueryLimits {
    default_limit: 2,
    max_limit: 3,
};

fn code(error: &anyhow::Error) -> SessionQueryErrorCode {
    error
        .downcast_ref::<SessionQueryError>()
        .expect("typed query error")
        .code
}

#[test]
fn normalizes_both_scopes_defaults_owned_filters_limits_and_cursors() {
    assert_eq!(
        normalize_session_request(
            SessionSearchRequest {
                query: "  alpha\n beta  ".to_owned(),
                session_filters: None,
                event_filters: None,
                limit: None,
                cursor: None,
            },
            LIMITS,
        )
        .unwrap(),
        NormalizedSessionRequest {
            query: "alpha beta".to_owned(),
            session_filters: Vec::new(),
            event_filters: Vec::new(),
            limit: 2,
            cursor: None,
        }
    );
    let cursor = SessionSearchCursor::new("next");
    let normalized = normalize_session_request(
        SessionSearchRequest {
            query: "needle".to_owned(),
            session_filters: Some(vec![SessionResultFilter::Availability {
                values: vec![SessionAvailability::Live],
            }]),
            event_filters: Some(vec![SessionEventMetadataFilter::Surface {
                values: vec![SessionEventSurface::Current],
            }]),
            limit: Some(3),
            cursor: Some(cursor.clone()),
        },
        LIMITS,
    )
    .unwrap();
    assert_eq!(normalized.limit, 3);
    assert_eq!(normalized.cursor, Some(cursor));
    assert_eq!(normalized.session_filters.len(), 1);
    assert_eq!(normalized.event_filters.len(), 1);

    let event = normalize_event_request(
        SessionEventSearchRequest {
            session_id: SessionId::new("s"),
            query: "needle".to_owned(),
            filters: Some(vec![SessionEventMetadataFilter::Seq {
                from: Some(1.into()),
                to: None,
            }]),
            limit: None,
            cursor: None,
        },
        LIMITS,
    )
    .unwrap();
    assert_eq!(event.limit, 2);
    assert_eq!(event.filters.len(), 1);
}

#[test]
fn rejects_blank_nul_inverted_and_out_of_range_requests() {
    for query in [" \n ", "bad\0query"] {
        let error = normalize_session_request(
            SessionSearchRequest {
                query: query.to_owned(),
                session_filters: None,
                event_filters: None,
                limit: None,
                cursor: None,
            },
            LIMITS,
        )
        .unwrap_err();
        assert_eq!(
            code(&error),
            SessionQueryErrorCode::SessionQueryInvalidQuery
        );
    }
    for limit in [0, 4, SQLITE_MAX_PAGE_LIMIT + 1] {
        let maximum = if limit > SQLITE_MAX_PAGE_LIMIT {
            limit
        } else {
            LIMITS.max_limit
        };
        let error = normalize_event_request(
            SessionEventSearchRequest {
                session_id: SessionId::new("s"),
                query: "x".to_owned(),
                filters: None,
                limit: Some(limit),
                cursor: None,
            },
            QueryLimits {
                default_limit: 1,
                max_limit: maximum,
            },
        )
        .unwrap_err();
        assert_eq!(
            code(&error),
            SessionQueryErrorCode::SessionQueryInvalidLimit
        );
    }
    let error = normalize_event_request(
        SessionEventSearchRequest {
            session_id: SessionId::new("s"),
            query: "x".to_owned(),
            filters: Some(vec![SessionEventMetadataFilter::Time {
                from: Some(9.into()),
                to: Some(1.into()),
            }]),
            limit: None,
            cursor: None,
        },
        LIMITS,
    )
    .unwrap_err();
    assert_eq!(
        code(&error),
        SessionQueryErrorCode::SessionQueryInvalidFilter
    );
}

#[test]
fn compiles_all_session_clauses_including_empty_nullable_and_availability() {
    assert_eq!(build_session_where(&[]).unwrap().sql, "");
    let empty = build_session_where(&[SessionResultFilter::Id { values: vec![] }]).unwrap();
    assert_eq!(empty.sql, "0");
    let ids = build_session_where(&[SessionResultFilter::Id {
        values: vec![SessionId::new("a"), SessionId::new("b")],
    }])
    .unwrap();
    assert_eq!(ids.sql, "session_id IN (?, ?)");
    assert_eq!(
        ids.params,
        [
            SqlParam::Text("a".to_owned()),
            SqlParam::Text("b".to_owned())
        ]
    );
    assert_eq!(
        build_session_where(&[SessionResultFilter::Cwd { values: vec![None] }])
            .unwrap()
            .sql,
        "(cwd IS NULL)"
    );
    let parent = build_session_where(&[SessionResultFilter::Parent {
        values: vec![Some(SessionId::new("p")), None],
    }])
    .unwrap();
    assert_eq!(
        parent.sql,
        "(parent_session IN (?) OR parent_session IS NULL)"
    );
    let combined = build_session_where(&[
        SessionResultFilter::CreatedAt {
            from: Some(1.into()),
            to: Some(2.into()),
        },
        SessionResultFilter::Availability { values: vec![] },
        SessionResultFilter::Availability {
            values: vec![SessionAvailability::Live, SessionAvailability::Live],
        },
        SessionResultFilter::Availability {
            values: vec![SessionAvailability::Live, SessionAvailability::Persisted],
        },
    ])
    .unwrap();
    assert_eq!(
        combined.sql,
        "CAST(created_at AS INTEGER) >= ? AND CAST(created_at AS INTEGER) <= ? AND 0 AND live = 1"
    );
    assert_eq!(combined.predicate_count, 4);
}

#[test]
fn compiles_every_event_clause_and_enforces_binding_and_predicate_budgets() {
    let compiled = build_event_where(&[
        SessionEventMetadataFilter::Seq {
            from: Some(1.into()),
            to: None,
        },
        SessionEventMetadataFilter::Time {
            from: None,
            to: Some(9.into()),
        },
        SessionEventMetadataFilter::Type {
            values: vec!["user/message".to_owned()],
        },
        SessionEventMetadataFilter::Surface {
            values: vec![SessionEventSurface::Current, SessionEventSurface::LogOnly],
        },
    ])
    .unwrap();
    assert_eq!(
        compiled.sql,
        "CAST(seq AS INTEGER) >= ? AND CAST(time AS INTEGER) <= ? AND type IN (?) AND surface IN (?, ?)"
    );
    assert_eq!(compiled.predicate_count, 4);
    let filters = (0..SQLITE_FTS5_OUTER_PREDICATE_LIMIT)
        .map(|_| SessionResultFilter::Id {
            values: vec![SessionId::new("safe")],
        })
        .collect::<Vec<_>>();
    assert_eq!(
        build_session_where(&filters).unwrap().predicate_count,
        SQLITE_FTS5_OUTER_PREDICATE_LIMIT
    );
    let mut over = filters;
    over.push(SessionResultFilter::Id {
        values: vec![SessionId::new("over")],
    });
    assert_eq!(
        code(&build_session_where(&over).unwrap_err()),
        SessionQueryErrorCode::SessionQueryInvalidFilter
    );
    let huge = vec![SessionId::new("x"); SQLITE_PORTABLE_VARIABLE_LIMIT + 1];
    assert_eq!(
        code(&build_session_where(&[SessionResultFilter::Id { values: huge }]).unwrap_err()),
        SessionQueryErrorCode::SessionQueryInvalidFilter
    );
}

#[test]
fn preserves_negative_and_fractional_source_number_bounds() {
    let lower = seekdeep_session_query::SessionResultBound::new(-123.999_99).unwrap();
    let upper = seekdeep_session_query::SessionResultBound::new(123.000_01).unwrap();
    let compiled = build_event_where(&[
        SessionEventMetadataFilter::Time {
            from: Some(lower),
            to: Some(upper),
        },
        SessionEventMetadataFilter::Seq {
            from: Some(seekdeep_session_query::SessionResultBound::new(0.5).unwrap()),
            to: None,
        },
    ])
    .unwrap();
    assert_eq!(
        compiled.params,
        [
            SqlParam::Number(lower),
            SqlParam::Number(upper),
            SqlParam::Number(seekdeep_session_query::SessionResultBound::new(0.5).unwrap()),
        ]
    );
    let decoded: SessionEventMetadataFilter = serde_json::from_value(serde_json::json!({
        "kind": "time",
        "from": -123.99999,
        "to": 123.00001
    }))
    .unwrap();
    assert_eq!(
        decoded,
        SessionEventMetadataFilter::Time {
            from: Some(lower),
            to: Some(upper),
        }
    );
}

#[test]
fn quotes_sanitizes_and_canonicalizes_request_identity() {
    assert_eq!(
        quote_fts_data("say \"needle\" OR *"),
        "\"say \"\"needle\"\" OR *\""
    );
    assert_eq!(
        sanitize_fts_text(&format!("a\0{FTS_HIGHLIGHT_START}b{FTS_HIGHLIGHT_END}")),
        "a��b�"
    );
    let first = NormalizedSessionRequest {
        query: "needle".to_owned(),
        limit: 2,
        session_filters: vec![
            SessionResultFilter::Cwd {
                values: vec![Some("/b".to_owned()), Some("/a".to_owned())],
            },
            SessionResultFilter::Parent {
                values: vec![None, Some(SessionId::new("p"))],
            },
            SessionResultFilter::CreatedAt {
                from: Some(1.into()),
                to: None,
            },
        ],
        event_filters: vec![SessionEventMetadataFilter::Time {
            from: None,
            to: Some(9.into()),
        }],
        cursor: None,
    };
    let second = NormalizedSessionRequest {
        session_filters: first.session_filters.iter().cloned().rev().collect(),
        ..first.clone()
    };
    assert_eq!(
        request_fingerprint(&NormalizedRequest::Sessions(&first)),
        request_fingerprint(&NormalizedRequest::Sessions(&second))
    );
    let event = NormalizedEventRequest {
        session_id: SessionId::new("s"),
        query: "needle".to_owned(),
        filters: vec![
            SessionEventMetadataFilter::Seq {
                from: None,
                to: None,
            },
            SessionEventMetadataFilter::Surface {
                values: vec![SessionEventSurface::Shadowed, SessionEventSurface::Current],
            },
        ],
        limit: 2,
        cursor: None,
    };
    let mut reversed = event.clone();
    reversed.filters.reverse();
    assert_eq!(
        request_fingerprint(&NormalizedRequest::Events(&event)),
        request_fingerprint(&NormalizedRequest::Events(&reversed))
    );
    reversed.session_id = SessionId::new("other");
    assert_ne!(
        request_fingerprint(&NormalizedRequest::Events(&event)),
        request_fingerprint(&NormalizedRequest::Events(&reversed))
    );
}

#[test]
fn snippets_are_whitespace_normalized_and_bounded_by_code_point() {
    let marked = |text: &str| format!("{FTS_HIGHLIGHT_START}{text}{FTS_HIGHLIGHT_END}");
    assert_eq!(make_snippet("  short\ntext  ", 20), "short text");
    assert_eq!(make_snippet(&format!("abcde{}", marked("f")), 1), "…");
    assert_eq!(make_snippet("abcdefghij", 5), "abcd…");
    assert_eq!(
        make_snippet(&format!("ab{}defghij", marked("c")), 5),
        "…bcd…"
    );
    assert_eq!(make_snippet(&format!("ab{}defghij", marked("c")), 3), "…c…");
    assert_eq!(make_snippet(&format!("abcde{}", marked("f")), 2), "…f");
    assert_eq!(make_snippet(&format!("abcde{}", marked("f")), 5), "…cdef");
}
