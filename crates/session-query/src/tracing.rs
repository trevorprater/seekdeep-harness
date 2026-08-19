//! One-shot session-lineage and event-relationship tracing helpers.

use std::collections::{HashMap, HashSet};

use seekdeep_core::session::{SessionEvent, SessionId, fold_surface, is_surface_event};

use crate::{
    config::{SessionQueryError, SessionQueryErrorCode},
    types::{
        SessionEventRecord, SessionEventSurface, SessionEventTrace, SessionLineageNode,
        SessionLineageTrace, SessionRecord,
    },
};

struct EventLogAnalysis {
    records: Vec<SessionEventRecord>,
    replaced_by: HashMap<u64, u64>,
    replaced_event_seqs: HashMap<u64, Vec<u64>>,
    current_seqs: Vec<u64>,
}

/// Classifies a raw event log with one canonical surface fold.
///
/// # Errors
///
/// Returns an invalid-surface failure when the log cannot be folded.
pub fn event_records(
    session_id: &SessionId,
    events: &[SessionEvent],
) -> Result<Vec<SessionEventRecord>, SessionQueryError> {
    Ok(analyze_event_log(session_id, events)?.records)
}

/// Folds and returns the current model surface after validating the whole log.
///
/// # Errors
///
/// Returns an invalid-surface failure when the log cannot be folded.
pub fn current_surface_events(
    session_id: &SessionId,
    events: &[SessionEvent],
) -> Result<Vec<SessionEvent>, SessionQueryError> {
    let analysis = analyze_event_log(session_id, events)?;
    analysis
        .current_seqs
        .iter()
        .map(|seq| {
            let index = usize::try_from(*seq).map_err(|_| invalid_surface_node(*seq))?;
            let event = events.get(index).filter(|event| event.seq == *seq);
            match event {
                Some(event) if is_surface_event(event) => Ok(event.clone()),
                _ => Err(invalid_surface_node(*seq)),
            }
        })
        .collect()
}

/// Traces one target after one canonical surface fold and whole-log validation.
///
/// # Errors
///
/// Returns an event-not-found or invalid-surface failure.
pub fn trace_event(
    session_id: &SessionId,
    events: &[SessionEvent],
    seq: u64,
) -> Result<SessionEventTrace, SessionQueryError> {
    let index = usize::try_from(seq).map_err(|_| event_not_found(session_id, seq))?;
    let target = events
        .get(index)
        .filter(|event| event.seq == seq)
        .ok_or_else(|| event_not_found(session_id, seq))?;

    let analysis = analyze_event_log(session_id, events)?;

    let mut replacement_chain = Vec::new();
    let mut replacement = analysis.replaced_by.get(&seq).copied();
    while let Some(current) = replacement {
        replacement_chain.push(current);
        replacement = analysis.replaced_by.get(&current).copied();
    }

    let derived_event_seqs = events
        .iter()
        .filter(|event| event.seq > seq && event_sources(event).contains(&seq))
        .map(|event| event.seq)
        .collect::<Vec<_>>();

    // The target check above proves the parallel record exists at this index.
    let target_record = analysis.records[index].clone();

    Ok(SessionEventTrace {
        target: target_record,
        replaced_by: analysis.replaced_by.get(&seq).copied(),
        replacement_chain,
        replaced_event_seqs: analysis
            .replaced_event_seqs
            .get(&seq)
            .cloned()
            .unwrap_or_default(),
        source_event_seqs: event_sources(target).to_vec(),
        derived_event_seqs,
    })
}

/// Traces one target's known ancestry and recursively known descendants.
///
/// # Errors
///
/// Returns a session-not-found or invalid-lineage failure.
pub fn trace_session(
    records: &[SessionRecord],
    session_id: &SessionId,
) -> Result<SessionLineageTrace, SessionQueryError> {
    let target_id = session_id.clone();
    let by_id: HashMap<SessionId, SessionRecord> = records
        .iter()
        .map(|record| (record.header.id.clone(), record.clone()))
        .collect();
    let target = by_id
        .get(&target_id)
        .ok_or_else(|| session_not_found(session_id))?;

    let mut ancestors: Vec<SessionRecord> = Vec::new();
    let mut ancestry_seen: HashSet<SessionId> = HashSet::new();
    ancestry_seen.insert(target_id.clone());
    let mut unresolved_parent_id: Option<SessionId> = None;
    let mut parent_id = target.header.parent_session.clone();
    while let Some(current) = parent_id.take() {
        if !ancestry_seen.insert(current.clone()) {
            return Err(SessionQueryError::new(
                format!("session lineage contains a cycle at \"{current}\""),
                SessionQueryErrorCode::SessionQueryInvalidLineage,
            ));
        }
        if let Some(parent) = by_id.get(&current) {
            ancestors.push(parent.clone());
            parent_id.clone_from(&parent.header.parent_session);
        } else {
            unresolved_parent_id = Some(current);
            break;
        }
    }

    let mut children_by_parent: HashMap<SessionId, Vec<SessionRecord>> = HashMap::new();
    for record in records {
        if let Some(parent) = &record.header.parent_session {
            children_by_parent
                .entry(parent.clone())
                .or_default()
                .push(record.clone());
        }
    }
    for children in children_by_parent.values_mut() {
        children.sort_by(|left, right| {
            left.header
                .created_at
                .cmp(&right.header.created_at)
                .then_with(|| left.header.id.as_str().cmp(right.header.id.as_str()))
        });
    }

    let descendants = build_descendants(&children_by_parent, &target_id);
    let ancestors = ancestors.into_iter().collect::<Vec<_>>();
    let root = ancestors.last().cloned().unwrap_or_else(|| target.clone());

    match unresolved_parent_id {
        Some(unresolved_parent_id) => Ok(SessionLineageTrace {
            target: target.clone(),
            ancestors,
            descendants,
            complete: false,
            root: None,
            unresolved_parent_id: Some(unresolved_parent_id),
        }),
        None => Ok(SessionLineageTrace {
            target: target.clone(),
            ancestors,
            descendants,
            complete: true,
            root: Some(root),
            unresolved_parent_id: None,
        }),
    }
}

fn analyze_event_log(
    session_id: &SessionId,
    events: &[SessionEvent],
) -> Result<EventLogAnalysis, SessionQueryError> {
    let folded = fold_surface(events).map_err(|error| {
        SessionQueryError::new(
            format!("invalid session surface: {error}"),
            SessionQueryErrorCode::SessionQueryInvalidSurface,
        )
    })?;
    let current: HashSet<u64> = folded.nodes.iter().copied().collect();
    let mut replaced_by = HashMap::new();
    let mut replaced_event_seqs = HashMap::new();
    for replacement in &folded.replacements {
        replaced_event_seqs.insert(replacement.seq, replacement.shadowed_seqs.clone());
        for removed_seq in &replacement.shadowed_seqs {
            replaced_by.insert(*removed_seq, replacement.seq);
        }
    }
    let records = events
        .iter()
        .map(|event| SessionEventRecord {
            session_id: session_id.clone(),
            seq: event.seq,
            event_type: event.event_type.clone(),
            time: event.time,
            surface: if current.contains(&event.seq) {
                SessionEventSurface::Current
            } else if replaced_by.contains_key(&event.seq) {
                SessionEventSurface::Shadowed
            } else {
                SessionEventSurface::LogOnly
            },
        })
        .collect();
    Ok(EventLogAnalysis {
        records,
        replaced_by,
        replaced_event_seqs,
        current_seqs: folded.nodes,
    })
}

fn event_sources(event: &SessionEvent) -> &[u64] {
    event.source_event_seqs.as_deref().unwrap_or(&[])
}

fn build_descendants(
    children_by_parent: &HashMap<SessionId, Vec<SessionRecord>>,
    session_id: &SessionId,
) -> Vec<SessionLineageNode> {
    struct Flat {
        session: Option<SessionRecord>,
        children: Vec<usize>,
    }
    // Index 0 is a synthetic root; every other index is a real record.
    let mut arena: Vec<Flat> = vec![Flat {
        session: None,
        children: Vec::new(),
    }];
    let mut stack: Vec<usize> = Vec::new();

    let mut root_children = Vec::new();
    for child in children_by_parent
        .get(session_id)
        .into_iter()
        .flatten()
        .rev()
    {
        let index = arena.len();
        arena.push(Flat {
            session: Some(child.clone()),
            children: Vec::new(),
        });
        root_children.push(index);
        stack.push(index);
    }
    root_children.reverse();
    arena[0].children = root_children;

    while let Some(parent_index) = stack.pop() {
        let parent_id = arena[parent_index]
            .session
            .as_ref()
            .expect("stacked arena node has a session")
            .header
            .id
            .clone();
        let mut child_indices = Vec::new();
        for child in children_by_parent
            .get(&parent_id)
            .into_iter()
            .flatten()
            .rev()
        {
            let index = arena.len();
            arena.push(Flat {
                session: Some(child.clone()),
                children: Vec::new(),
            });
            child_indices.push(index);
            stack.push(index);
        }
        child_indices.reverse();
        arena[parent_index].children = child_indices;
    }

    // Post-order: children before parents, so each subtree moves out intact.
    let mut postorder = Vec::with_capacity(arena.len());
    let mut order_stack: Vec<(usize, bool)> = vec![(0, false)];
    while let Some((index, expanded)) = order_stack.pop() {
        if expanded {
            postorder.push(index);
        } else {
            order_stack.push((index, true));
            for child in arena[index].children.iter().rev() {
                order_stack.push((*child, false));
            }
        }
    }

    let mut nodes: Vec<Option<SessionLineageNode>> = (0..arena.len()).map(|_| None).collect();
    for index in postorder {
        if index == 0 {
            continue;
        }
        let descendants = std::mem::take(&mut arena[index].children)
            .into_iter()
            .map(|child| {
                nodes[child]
                    .take()
                    .expect("descendant is built before its parent")
            })
            .collect::<Vec<_>>();
        let session = arena[index]
            .session
            .take()
            .expect("real arena node has a session");
        nodes[index] = Some(SessionLineageNode {
            session,
            descendants,
        });
    }

    std::mem::take(&mut arena[0].children)
        .into_iter()
        .map(|child| nodes[child].take().expect("root child is built"))
        .collect()
}

fn invalid_surface_node(seq: u64) -> SessionQueryError {
    SessionQueryError::new(
        format!("invalid session surface: current node {seq} is not a surface event"),
        SessionQueryErrorCode::SessionQueryInvalidSurface,
    )
}

fn event_not_found(session_id: &SessionId, seq: u64) -> SessionQueryError {
    SessionQueryError::new(
        format!("session \"{session_id}\" has no event at seq {seq}"),
        SessionQueryErrorCode::SessionQueryEventNotFound,
    )
}

fn session_not_found(session_id: &SessionId) -> SessionQueryError {
    SessionQueryError::new(
        format!("session \"{session_id}\" not found"),
        SessionQueryErrorCode::SessionQuerySessionNotFound,
    )
}

#[cfg(test)]
mod tests {
    use seekdeep_core::session::{SessionHeader, SurfaceOp, SurfaceReplace};
    use serde_json::json;

    use super::*;

    fn event(
        event_type: &str,
        seq: u64,
        surface_op: Option<SurfaceOp>,
        sources: Option<Vec<u64>>,
    ) -> SessionEvent {
        SessionEvent {
            event_type: event_type.to_owned(),
            seq,
            time: i64::try_from(seq + 1).expect("test seq fits i64"),
            data: json!({}),
            source_event_seqs: sources,
            surface_op,
            ignorable: None,
        }
    }

    fn replace(start: u64, end: u64) -> SurfaceOp {
        SurfaceOp::Replace(SurfaceReplace {
            op: "replace".to_owned(),
            start,
            end,
        })
    }

    fn append_trace_events() -> Vec<SessionEvent> {
        vec![
            event("turn/start", 0, None, None),
            event("step/start", 1, None, None),
            event("assistant/chunk", 2, None, None),
            event("user/message", 3, Some(SurfaceOp::append()), Some(vec![2])),
            event(
                "assistant/message",
                4,
                Some(replace(3, 3)),
                Some(vec![3, 2]),
            ),
            event("user/message", 5, Some(SurfaceOp::append()), None),
            event("step/end", 6, None, None),
            event("step/start", 7, None, None),
            event(
                "assistant/message",
                8,
                Some(replace(4, 4)),
                Some(vec![2, 4]),
            ),
        ]
    }

    fn header(id: &str, created_at: u64, parent: Option<&str>) -> SessionHeader {
        SessionHeader {
            version: 0,
            id: SessionId::new(id),
            created_at,
            cwd: None,
            parent_session: parent.map(SessionId::new),
            seed_length: None,
            origin: None,
            delegation_depth: None,
            agent_preset: None,
        }
    }

    fn record(id: &str, created_at: u64, parent: Option<&str>) -> SessionRecord {
        SessionRecord {
            header: header(id, created_at, parent),
            live: true,
            persisted: false,
        }
    }

    #[test]
    fn traces_replacement_chains_and_source_relationships() {
        let session_id = SessionId::new("trace");
        let events = append_trace_events();

        let original = trace_event(&session_id, &events, 3).expect("trace 3");
        assert_eq!(original.target.seq, 3);
        assert_eq!(original.target.surface, SessionEventSurface::Shadowed);
        assert_eq!(original.replaced_by, Some(4));
        assert_eq!(original.replacement_chain, [4, 8]);
        assert!(original.replaced_event_seqs.is_empty());
        assert_eq!(original.source_event_seqs, [2]);
        assert_eq!(original.derived_event_seqs, [4]);

        let replacer = trace_event(&session_id, &events, 4).expect("trace 4");
        assert_eq!(replacer.replaced_by, Some(8));
        assert_eq!(replacer.replacement_chain, [8]);
        assert_eq!(replacer.replaced_event_seqs, [3]);
        assert_eq!(replacer.source_event_seqs, [3, 2]);
        assert_eq!(replacer.derived_event_seqs, [8]);

        let chunk = trace_event(&session_id, &events, 2).expect("trace 2");
        assert_eq!(chunk.target.surface, SessionEventSurface::LogOnly);
        assert!(chunk.replacement_chain.is_empty());
        assert!(chunk.source_event_seqs.is_empty());
        assert_eq!(chunk.derived_event_seqs, [3, 4, 8]);
    }

    #[test]
    fn checks_target_existence_before_surface_analysis() {
        let session_id = SessionId::new("bad");
        let events = vec![event(
            "assistant/message",
            0,
            Some(replace(9, 9)),
            Some(Vec::new()),
        )];
        let error = trace_event(&session_id, &events, 9).expect_err("missing target");
        assert_eq!(error.code, SessionQueryErrorCode::SessionQueryEventNotFound);
        let error = trace_event(&session_id, &events, 0).expect_err("invalid surface");
        assert_eq!(
            error.code,
            SessionQueryErrorCode::SessionQueryInvalidSurface
        );
    }

    #[test]
    fn traces_complete_and_partial_lineages() {
        let records = vec![
            record("root", 0, None),
            record("parent", 1, Some("root")),
            record("target", 2, Some("parent")),
            record("b", 4, Some("target")),
            record("a", 4, Some("target")),
            record("older", 3, Some("target")),
            record("grandchild", 5, Some("a")),
        ];
        let trace = trace_session(&records, &SessionId::new("target")).expect("complete");
        assert!(trace.complete);
        assert_eq!(
            trace
                .ancestors
                .iter()
                .map(|record| record.header.id.as_str())
                .collect::<Vec<_>>(),
            ["parent", "root"]
        );
        assert_eq!(
            trace.root.as_ref().expect("root").header.id.as_str(),
            "root"
        );
        assert_eq!(
            trace
                .descendants
                .iter()
                .map(|node| node.session.header.id.as_str())
                .collect::<Vec<_>>(),
            ["older", "a", "b"]
        );
        assert_eq!(
            trace.descendants[1]
                .descendants
                .iter()
                .map(|node| node.session.header.id.as_str())
                .collect::<Vec<_>>(),
            ["grandchild"]
        );
    }

    #[test]
    fn rejects_cycles_and_missing_targets() {
        let records = vec![record("a", 1, Some("b")), record("b", 2, Some("a"))];
        let error = trace_session(&records, &SessionId::new("a")).expect_err("cycle");
        assert_eq!(
            error.code,
            SessionQueryErrorCode::SessionQueryInvalidLineage
        );
        let error = trace_session(&records, &SessionId::new("missing")).expect_err("missing");
        assert_eq!(
            error.code,
            SessionQueryErrorCode::SessionQuerySessionNotFound
        );
    }

    #[test]
    fn resolves_partial_lineages_with_an_unresolved_parent() {
        let records = vec![record("partial", 2, Some("outside"))];
        let trace = trace_session(&records, &SessionId::new("partial")).expect("partial");
        assert!(!trace.complete);
        assert_eq!(
            trace
                .unresolved_parent_id
                .as_ref()
                .expect("unresolved")
                .as_str(),
            "outside"
        );
        assert!(trace.ancestors.is_empty());
    }

    #[test]
    fn builds_deeply_nested_descendants_without_recursion() {
        let mut records = Vec::new();
        records.push(record("deep-0", 0, None));
        for depth in 1..3_000 {
            let parent = format!("deep-{}", depth - 1);
            records.push(record(
                &format!("deep-{depth}"),
                u64::try_from(depth).expect("depth"),
                Some(&parent),
            ));
        }
        let trace = trace_session(&records, &SessionId::new("deep-0")).expect("deep lineage");
        assert!(trace.complete);
        let mut node = &trace.descendants[0];
        for depth in 1..3_000 {
            assert_eq!(node.session.header.id.as_str(), format!("deep-{depth}"));
            node = if let Some(child) = node.descendants.first() {
                child
            } else {
                assert_eq!(depth, 2_999, "lineage ended before depth 2999");
                break;
            };
        }
        assert!(node.descendants.is_empty());
    }

    #[test]
    fn current_surface_events_return_cloned_surface_nodes() {
        let session_id = SessionId::new("surface");
        let events = append_trace_events();
        let surface = current_surface_events(&session_id, &events).expect("surface");
        assert_eq!(
            surface.iter().map(|event| event.seq).collect::<Vec<_>>(),
            [8, 5]
        );
    }
}
