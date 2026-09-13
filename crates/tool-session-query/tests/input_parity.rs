//! Model input normalization and exact timestamp-bound parity.

use seekdeep_session_query::{
    SessionEventSurface, SessionQueryError, SessionQueryErrorCode, SessionResultFilter,
    types::SessionEventMetadataFilter,
};
use seekdeep_tool_session_query::input::{
    EventFilterInput, SessionSearchArgs, build_event_filters, build_session_filters,
    materialize_parent_session_ids, non_negative_safe, normalize_query, sequence_range,
};

fn args() -> SessionSearchArgs {
    SessionSearchArgs {
        query: " needle ".to_owned(),
        session_ids: None,
        created_at_from: None,
        created_at_to: None,
        parent_session_ids: None,
        include_root_sessions: None,
        availability: None,
        event_seq_from: None,
        event_seq_to: None,
        event_time_from: None,
        event_time_to: None,
        event_types: None,
        event_surfaces: None,
    }
}

fn code(error: &anyhow::Error) -> SessionQueryErrorCode {
    error.downcast_ref::<SessionQueryError>().unwrap().code
}

#[test]
fn normalizes_queries_sequences_lists_and_parent_identity() {
    assert_eq!(normalize_query("  alpha\n beta  ").unwrap(), "alpha beta");
    assert_eq!(
        normalize_query("\u{feff}alpha\u{2003}beta\u{feff}").unwrap(),
        "alpha beta"
    );
    assert_eq!(normalize_query("\u{0085}").unwrap(), "\u{0085}");
    for invalid in [" ", "bad\0query"] {
        assert_eq!(
            code(&normalize_query(invalid).unwrap_err()),
            SessionQueryErrorCode::SessionQueryInvalidQuery
        );
    }
    let (from, to) = sequence_range(Some(1), Some(2)).unwrap();
    assert_eq!(from.unwrap().value().to_bits(), 1.0_f64.to_bits());
    assert_eq!(to.unwrap().value().to_bits(), 2.0_f64.to_bits());
    for value in [-1, 9_007_199_254_740_992] {
        assert_eq!(
            code(&non_negative_safe("seq", value).unwrap_err()),
            SessionQueryErrorCode::SessionQueryInvalidFilter
        );
    }
    assert_eq!(
        materialize_parent_session_ids(Some(&["a".to_owned(), "a".to_owned(), "b".to_owned(),]))
            .unwrap()
            .unwrap()
            .iter()
            .map(seekdeep_core::session::SessionId::as_str)
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
}

#[test]
fn maps_exact_fractional_bounds_to_adjacent_finite_numbers() {
    let mut value = args();
    value.created_at_from = Some("2026-07-24T00:00:00.12300001Z".to_owned());
    value.created_at_to = Some("2026-07-24T08:00:00.1239999+08:00".to_owned());
    let filters = build_session_filters(&value).unwrap();
    let SessionResultFilter::CreatedAt { from, to } = &filters[0] else {
        panic!("created-at filter");
    };
    let exact = 1_784_851_200_123_f64;
    assert_eq!(from.unwrap().value().to_bits(), (exact.to_bits() + 1));
    assert_eq!(to.unwrap().value().to_bits(), (exact + 1.0).to_bits() - 1);

    let filters = build_event_filters(EventFilterInput {
        seq_from: None,
        seq_to: None,
        time_from: Some("1969-12-31T23:59:59.87600001Z"),
        time_to: Some("1969-12-31T19:59:59.8769999-04:00"),
        event_types: None,
        surfaces: None,
    })
    .unwrap();
    let SessionEventMetadataFilter::Time { from, to } = &filters[0] else {
        panic!("time filter");
    };
    assert_eq!(from.unwrap().value().to_bits(), (-124_f64).to_bits() - 1);
    assert_eq!(to.unwrap().value().to_bits(), (-123_f64).to_bits() + 1);
}

#[test]
fn validates_exact_order_calendar_offsets_and_nonempty_filter_arrays() {
    let mut value = args();
    value.created_at_from = Some("2026-01-01T00:00:00.0002Z".to_owned());
    value.created_at_to = Some("2026-01-01T00:00:00.0001Z".to_owned());
    assert_eq!(
        code(&build_session_filters(&value).unwrap_err()),
        SessionQueryErrorCode::SessionQueryInvalidFilter
    );
    for timestamp in [
        "2026-02-30T00:00:00Z",
        "2026-01-01T24:00:00Z",
        "2026-01-01T00:00:00",
        "2026-01-01T00:00.1Z",
        "2026-01-01T00:00:00+24:00",
    ] {
        let error = build_event_filters(EventFilterInput {
            seq_from: None,
            seq_to: None,
            time_from: Some(timestamp),
            time_to: None,
            event_types: None,
            surfaces: None,
        })
        .unwrap_err();
        assert_eq!(
            code(&error),
            SessionQueryErrorCode::SessionQueryInvalidFilter
        );
    }
    assert!(
        build_event_filters(EventFilterInput {
            seq_from: None,
            seq_to: None,
            time_from: None,
            time_to: None,
            event_types: Some(&[]),
            surfaces: None,
        })
        .is_err()
    );
    let types = vec!["user/message".to_owned()];
    let surfaces = vec![SessionEventSurface::Current];
    assert_eq!(
        build_event_filters(EventFilterInput {
            seq_from: Some(1),
            seq_to: Some(2),
            time_from: None,
            time_to: None,
            event_types: Some(&types),
            surfaces: Some(&surfaces),
        })
        .unwrap()
        .len(),
        3
    );
}
