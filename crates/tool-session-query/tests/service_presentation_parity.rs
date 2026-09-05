//! Model-safe service failures and exact presentation parity.

use std::{collections::BTreeMap, sync::Arc};

use parking_lot::Mutex;
use seekdeep_core::session::{SessionEvent, SessionHeader, SessionId, SurfaceOp};
use seekdeep_llm::HarnessError;
use seekdeep_session_query::{
    SessionEventRecord, SessionEventSurface, SessionEventTrace, SessionEventTraceObservation,
    SessionEventWindow, SessionQueryError, SessionQueryErrorCode,
};
use seekdeep_tool_session_query::{presentation, service_boundary, workspace_access::TitleView};
use serde_json::json;

fn harness(error: &anyhow::Error) -> &HarnessError {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<HarnessError>())
        .unwrap()
}

#[test]
fn sanitizes_every_service_code_without_exposing_diagnostics() {
    let context = seekdeep_cordis::Context::new();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let sink = captured.clone();
    let mut exporter = seekdeep_cordis::LogExporter::new(move |message| sink.lock().push(message));
    exporter.levels = BTreeMap::from([("default".to_owned(), 3)]);
    let _registration = context
        .logger_service()
        .exporter(&context, exporter)
        .unwrap();
    let safe_codes = [
        SessionQueryErrorCode::SessionQueryAborted,
        SessionQueryErrorCode::SessionQueryCorruptSession,
        SessionQueryErrorCode::SessionQueryEventNotFound,
        SessionQueryErrorCode::SessionQueryIndexFailed,
        SessionQueryErrorCode::SessionQueryInvalidCursor,
        SessionQueryErrorCode::SessionQueryInvalidFilter,
        SessionQueryErrorCode::SessionQueryInvalidLimit,
        SessionQueryErrorCode::SessionQueryInvalidQuery,
        SessionQueryErrorCode::SessionQueryInvalidLineage,
        SessionQueryErrorCode::SessionQueryInvalidSurface,
        SessionQueryErrorCode::SessionQueryInvalidWindow,
        SessionQueryErrorCode::SessionQueryPersistenceFailed,
        SessionQueryErrorCode::SessionQuerySearchDisabled,
        SessionQueryErrorCode::SessionQuerySessionNotFound,
        SessionQueryErrorCode::SessionQueryStaleCursor,
    ];
    for code in safe_codes {
        let diagnostic = format!("secret diagnostic for {}", code.as_str());
        let source: anyhow::Error = SessionQueryError::new(&diagnostic, code).into();
        let sanitized = service_boundary::sanitize_error(&context, "test", &source);
        let visible = harness(&sanitized);
        assert_eq!(visible.code(), code.as_str());
        assert!(!visible.message().contains("secret"));
    }
    for code in [
        SessionQueryErrorCode::SessionQueryInvalidConfig,
        SessionQueryErrorCode::SessionQuerySourceConflict,
    ] {
        let source: anyhow::Error = SessionQueryError::new("secret diagnostic", code).into();
        let sanitized = service_boundary::sanitize_error(&context, "test", &source);
        assert_eq!(harness(&sanitized).code(), "SESSION_QUERY_TOOL_FAILED");
        assert_eq!(
            harness(&sanitized).message(),
            "session query operation failed"
        );
    }
    let unknown = anyhow::anyhow!("private backend failure");
    let sanitized = service_boundary::sanitize_error(&context, "test", &unknown);
    assert_eq!(harness(&sanitized).code(), "SESSION_QUERY_TOOL_FAILED");
    let logs = captured.lock();
    assert!(logs.iter().any(|entry| {
        entry.args.iter().any(|value| {
            value
                .as_str()
                .is_some_and(|text| text.contains("private backend failure"))
        })
    }));
}

#[test]
fn preserves_only_the_fixed_unauthorized_target_failure() {
    let context = seekdeep_cordis::Context::new();
    let unauthorized = service_boundary::unauthorized_target();
    let sanitized = service_boundary::sanitize_error(&context, "authority", &unauthorized);
    assert_eq!(
        harness(&sanitized).code(),
        "SESSION_QUERY_TOOL_UNAUTHORIZED"
    );
    assert_eq!(
        harness(&sanitized).message(),
        "session target is outside the caller workspace"
    );
}

fn header() -> SessionHeader {
    SessionHeader {
        version: 0,
        id: SessionId::new("session"),
        created_at: 0,
        cwd: Some("/work".to_owned()),
        parent_session: None,
        seed_length: None,
        origin: None,
        delegation_depth: None,
        agent_preset: None,
    }
}

fn event(seq: u64, event_type: &str, data: serde_json::Value) -> SessionEvent {
    SessionEvent {
        event_type: event_type.to_owned(),
        seq,
        time: i64::try_from(seq).unwrap(),
        data,
        source_event_seqs: None,
        surface_op: Some(SurfaceOp::append()),
        ignorable: None,
    }
}

#[test]
fn renders_relationships_unabridged_json_and_semantic_neighbors() {
    let title = TitleView {
        text: "A title".to_owned(),
        unavailable_code: None,
    };
    let trace = SessionEventTraceObservation {
        trace: SessionEventTrace {
            target: SessionEventRecord {
                session_id: SessionId::new("session"),
                seq: 2,
                event_type: "assistant/message".to_owned(),
                time: 0,
                surface: SessionEventSurface::Shadowed,
            },
            replaced_by: Some(5),
            replacement_chain: vec![5, 9],
            replaced_event_seqs: vec![3, 4],
            source_event_seqs: vec![0, 1],
            derived_event_seqs: vec![6, 7],
        },
        session: header(),
    };
    let rendered = presentation::format_event_trace(&SessionId::new("session"), &title, &trace);
    for expected in [
        "Replaced by: 5",
        "Replacement chain: 5, 9",
        "Events replaced by target: 3, 4",
        "Events cited directly as sources: 0, 1",
        "Direct derived events: 6, 7",
        "1970-01-01T00:00:00.000Z",
    ] {
        assert!(rendered.contains(expected), "{rendered}");
    }

    let before = event(
        0,
        "user/message",
        json!({"id":"u","role":"user","source":{"kind":"user"},"content":[{"type":"text","text":"before text"}]}),
    );
    let target = event(1, "custom/event", json!({"secret":{"nested":true}}));
    let after = event(2, "step/end", json!({"turn":1,"step":1}));
    let window = SessionEventWindow {
        session: header(),
        target: target.clone(),
        events: vec![before, target, after],
        start_seq: 0,
        end_seq: 2,
    };
    let rendered =
        presentation::format_event_read(&SessionId::new("session"), &title, &window).unwrap();
    assert!(rendered.contains("```json"));
    assert!(rendered.contains("\"nested\": true"));
    assert!(rendered.contains("before text"));
    assert!(rendered.contains("(no semantic text)"));
}
