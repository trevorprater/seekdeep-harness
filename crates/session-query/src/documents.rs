//! Shared event metadata and semantic-document projection.

use std::collections::HashMap;

use seekdeep_core::session::{SessionEvent, SessionId, fold_surface};

use crate::{
    config::{SessionQueryError, SessionQueryErrorCode},
    extraction::extract_session_event_text,
    types::{SessionEventRecord, SessionEventSearchDocument, SessionEventSurface},
};

/// Projects a raw log into lightweight surface-aware event records.
///
/// # Errors
///
/// Returns an invalid-surface failure when the raw log cannot be folded.
pub fn build_session_event_records(
    session_id: &SessionId,
    events: &[SessionEvent],
) -> Result<Vec<SessionEventRecord>, SessionQueryError> {
    let surface_by_seq = classify_surface(events)?;
    Ok(events
        .iter()
        .map(|event| SessionEventRecord {
            session_id: session_id.clone(),
            seq: event.seq,
            event_type: event.event_type.clone(),
            time: event.time,
            surface: surface_by_seq
                .get(&event.seq)
                .copied()
                .unwrap_or(SessionEventSurface::LogOnly),
        })
        .collect())
}

/// Builds first-party semantic documents for one complete raw event log.
///
/// Structural events that contribute no semantic text are omitted.
///
/// # Errors
///
/// Returns an invalid-surface failure when the raw log cannot be folded.
pub fn build_session_event_search_documents(
    session_id: &SessionId,
    events: &[SessionEvent],
) -> Result<Vec<SessionEventSearchDocument>, SessionQueryError> {
    let surface_by_seq = classify_surface(events)?;
    let mut documents = Vec::new();
    for event in events {
        let text = extract_session_event_text(event);
        if text.is_empty() {
            continue;
        }
        documents.push(SessionEventSearchDocument {
            record: SessionEventRecord {
                session_id: session_id.clone(),
                seq: event.seq,
                event_type: event.event_type.clone(),
                time: event.time,
                surface: surface_by_seq
                    .get(&event.seq)
                    .copied()
                    .unwrap_or(SessionEventSurface::LogOnly),
            },
            text,
        });
    }
    Ok(documents)
}

fn classify_surface(
    events: &[SessionEvent],
) -> Result<HashMap<u64, SessionEventSurface>, SessionQueryError> {
    let folded = fold_surface(events).map_err(|error| {
        SessionQueryError::new(
            format!("invalid session surface: {error}"),
            SessionQueryErrorCode::SessionQueryInvalidSurface,
        )
    })?;
    let mut result = HashMap::new();
    for seq in folded.nodes {
        result.insert(seq, SessionEventSurface::Current);
    }
    for replacement in folded.replacements {
        for seq in replacement.shadowed_seqs {
            result.insert(seq, SessionEventSurface::Shadowed);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use seekdeep_core::session::{SurfaceOp, SurfaceReplace};
    use serde_json::json;

    use super::*;

    fn append(event_type: &str, seq: u64, data: serde_json::Value) -> SessionEvent {
        SessionEvent {
            event_type: event_type.to_owned(),
            seq,
            time: 1,
            data,
            source_event_seqs: None,
            surface_op: Some(SurfaceOp::append()),
            ignorable: None,
        }
    }

    fn text_block(text: &str) -> serde_json::Value {
        json!([{ "type": "text", "text": text }])
    }

    #[test]
    fn classifies_surfaces_and_omits_structural_documents() {
        let events = vec![
            append(
                "user/message",
                0,
                json!({ "source": { "kind": "user" }, "content": text_block("Hello") }),
            ),
            SessionEvent {
                event_type: "assistant/chunk".to_owned(),
                seq: 1,
                time: 1,
                data: json!({}),
                source_event_seqs: None,
                surface_op: None,
                ignorable: None,
            },
            SessionEvent {
                event_type: "assistant/message".to_owned(),
                seq: 2,
                time: 1,
                data: json!({
                    "message": {
                        "id": "m", "role": "assistant",
                        "source": { "kind": "model", "provider": "mock", "model": "mock" },
                        "content": text_block("replacement")
                    }
                }),
                source_event_seqs: Some(vec![0]),
                surface_op: Some(SurfaceOp::Replace(SurfaceReplace {
                    op: "replace".to_owned(),
                    start: 0,
                    end: 0,
                })),
                ignorable: None,
            },
            SessionEvent {
                event_type: "turn/end".to_owned(),
                seq: 3,
                time: 1,
                data: json!({ "reason": { "kind": "interrupted" } }),
                source_event_seqs: None,
                surface_op: None,
                ignorable: None,
            },
        ];
        let id = SessionId::new("s");
        let surfaces = build_session_event_records(&id, &events)
            .expect("records")
            .into_iter()
            .map(|record| record.surface)
            .collect::<Vec<_>>();
        assert_eq!(
            surfaces,
            [
                SessionEventSurface::Shadowed,
                SessionEventSurface::LogOnly,
                SessionEventSurface::Current,
                SessionEventSurface::LogOnly,
            ]
        );
        let documents = build_session_event_search_documents(&id, &events).expect("documents");
        assert_eq!(
            documents
                .iter()
                .map(|document| (
                    document.record.seq,
                    document.text.as_str(),
                    document.record.surface
                ))
                .collect::<Vec<_>>(),
            [
                (0, "Hello", SessionEventSurface::Shadowed),
                (2, "replacement", SessionEventSurface::Current),
                (3, "interrupted", SessionEventSurface::LogOnly),
            ]
        );
    }

    #[test]
    fn malformed_surface_becomes_an_invalid_surface_failure() {
        let events = vec![SessionEvent {
            event_type: "assistant/message".to_owned(),
            seq: 0,
            time: 1,
            data: json!({}),
            source_event_seqs: None,
            surface_op: Some(SurfaceOp::Replace(SurfaceReplace {
                op: "replace".to_owned(),
                start: 9,
                end: 9,
            })),
            ignorable: None,
        }];
        let error = build_session_event_records(&SessionId::new("s"), &events)
            .expect_err("malformed surface");
        assert_eq!(
            error.code,
            SessionQueryErrorCode::SessionQueryInvalidSurface
        );
    }
}
