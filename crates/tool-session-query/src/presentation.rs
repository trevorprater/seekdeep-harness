//! Model text rendering and generic pending-call presentation.

use std::collections::{BTreeMap, BTreeSet};

use chrono::SecondsFormat;
use seekdeep_core::session::{SessionEvent, SessionId};
use seekdeep_session_query::{
    SessionEventSearchHit, SessionEventTraceObservation, SessionEventWindow, SessionLineageTrace,
    SessionRecord, SessionSearchHit, extract_session_event_text,
};
use seekdeep_tools::{GenericCallView, ToolCallKind};
use serde_json::json;

use crate::{
    input::{EventSearchArgs, EventTargetArgs, SessionSearchArgs, SessionTargetArgs},
    operations::SearchCollection,
    workspace_access::{self, AuthorizedDescendant, TitleView},
};

/// Formats cross-session search hits.
pub fn format_session_search(
    collected: &SearchCollection<SessionSearchHit>,
    titles: &BTreeMap<SessionId, TitleView>,
    authorized_parents: &BTreeSet<SessionId>,
) -> String {
    if collected.items.is_empty() {
        return format_empty_session_search().to_owned();
    }
    let mut lines = vec![format!(
        "Session search results ({}):",
        collected.items.len()
    )];
    for (index, hit) in collected.items.iter().enumerate() {
        let parent = hit.record.header.parent_session.as_ref().map_or_else(
            || "root".to_owned(),
            |parent| {
                if authorized_parents.contains(parent) {
                    parent.to_string()
                } else {
                    "[outside workspace]".to_owned()
                }
            },
        );
        lines.extend([
            String::new(),
            format!(
                "{}. Session {} — {}",
                index + 1,
                hit.record.header.id,
                workspace_access::title_text(titles.get(&hit.record.header.id))
            ),
            format!(
                "   Created: {}",
                format_time_u64(hit.record.header.created_at)
            ),
            format!("   Parent: {parent}"),
            format!("   Availability: {}", availability_text(&hit.record)),
            format!(
                "   Best match: seq {} | {} | {} | {}",
                hit.best_match.record.seq,
                hit.best_match.record.event_type,
                surface_text(hit.best_match.record.surface),
                format_time(hit.best_match.record.time)
            ),
            format!("   Snippet: {}", hit.best_match.snippet),
        ]);
    }
    if collected.capped {
        lines.extend([
            String::new(),
            "Result cap reached. Narrow the query or add filters to find additional matches."
                .to_owned(),
        ]);
    }
    lines.join("\n")
}

/// Fixed empty cross-session result.
pub const fn format_empty_session_search() -> &'static str {
    "No prior session matches found."
}

/// Formats within-session search hits.
pub fn format_event_search(
    session_id: &SessionId,
    title: &TitleView,
    collected: &SearchCollection<SessionEventSearchHit>,
) -> String {
    let mut lines = vec![format!(
        "Session {session_id} — {}",
        workspace_access::title_text(Some(title))
    )];
    if collected.items.is_empty() {
        lines.extend([String::new(), "No prior event matches found.".to_owned()]);
        return lines.join("\n");
    }
    lines.extend([
        String::new(),
        format!("Event search results ({}):", collected.items.len()),
    ]);
    for (index, hit) in collected.items.iter().enumerate() {
        lines.extend([
            format!(
                "{}. seq {} | {} | {} | {}",
                index + 1,
                hit.record.seq,
                hit.record.event_type,
                surface_text(hit.record.surface),
                format_time(hit.record.time)
            ),
            format!("   Snippet: {}", hit.snippet),
        ]);
    }
    if collected.capped {
        lines.extend([
            String::new(),
            "Result cap reached. Narrow the query or add filters to find additional matches."
                .to_owned(),
        ]);
    }
    lines.join("\n")
}

/// Formats authorized lineage with explicit workspace boundaries.
pub fn format_session_trace(
    trace: &SessionLineageTrace,
    ancestors: &[SessionRecord],
    ancestor_boundary: bool,
    descendants: &[Option<AuthorizedDescendant>],
    titles: &BTreeMap<SessionId, TitleView>,
) -> String {
    let mut lines = vec![
        format!(
            "Session {} — {}",
            trace.target.header.id,
            workspace_access::title_text(titles.get(&trace.target.header.id))
        ),
        format!(
            "Created: {}",
            format_time_u64(trace.target.header.created_at)
        ),
        format!("Availability: {}", availability_text(&trace.target)),
        String::new(),
        "Ancestors (nearest first):".to_owned(),
    ];
    if ancestors.is_empty() && !ancestor_boundary {
        lines.push("- none (target is a root session)".to_owned());
    }
    for record in ancestors {
        lines.push(format!(
            "- {} — {} | {} | {}",
            record.header.id,
            workspace_access::title_text(titles.get(&record.header.id)),
            format_time_u64(record.header.created_at),
            availability_text(record)
        ));
    }
    if ancestor_boundary {
        lines.push("- [outside workspace boundary]".to_owned());
    }
    lines.extend([String::new(), "Descendants:".to_owned()]);
    if descendants.is_empty() {
        lines.push("- none".to_owned());
    } else {
        for visit in workspace_access::visit_descendants(descendants) {
            let indent = "  ".repeat(visit.depth);
            if let Some(node) = visit.node {
                lines.push(format!(
                    "{indent}- {} — {} | {} | {}",
                    node.record.header.id,
                    workspace_access::title_text(titles.get(&node.record.header.id)),
                    format_time_u64(node.record.header.created_at),
                    availability_text(&node.record)
                ));
            } else {
                lines.push(format!("{indent}- [outside workspace subtree]"));
            }
        }
    }
    lines.join("\n")
}

/// Formats direct event relationships.
pub fn format_event_trace(
    session_id: &SessionId,
    title: &TitleView,
    trace: &SessionEventTraceObservation,
) -> String {
    [
        format!(
            "Session {session_id} — {}",
            workspace_access::title_text(Some(title))
        ),
        format!(
            "Target: seq {} | {} | {} | {}",
            trace.trace.target.seq,
            trace.trace.target.event_type,
            surface_text(trace.trace.target.surface),
            format_time(trace.trace.target.time)
        ),
        format!(
            "Replaced by: {}",
            trace
                .trace
                .replaced_by
                .map_or_else(|| "none".to_owned(), |seq| seq.to_string())
        ),
        format!(
            "Replacement chain: {}",
            seq_list(&trace.trace.replacement_chain)
        ),
        format!(
            "Events replaced by target: {}",
            seq_list(&trace.trace.replaced_event_seqs)
        ),
        format!(
            "Events cited directly as sources: {}",
            seq_list(&trace.trace.source_event_seqs)
        ),
        format!(
            "Direct derived events: {}",
            seq_list(&trace.trace.derived_event_seqs)
        ),
    ]
    .join("\n")
}

/// Formats an unabridged target event and semantic neighbor summaries.
///
/// # Errors
///
/// Returns only JSON serialization failures for the canonical target event.
pub fn format_event_read(
    session_id: &SessionId,
    title: &TitleView,
    window: &SessionEventWindow,
) -> anyhow::Result<String> {
    let mut lines = vec![
        format!(
            "Session {session_id} — {}",
            workspace_access::title_text(Some(title))
        ),
        format!("Target event seq {}:", window.target.seq),
        "```json".to_owned(),
        serde_json::to_string_pretty(&window.target)?,
        "```".to_owned(),
    ];
    let before = window
        .events
        .iter()
        .filter(|event| event.seq < window.target.seq)
        .collect::<Vec<_>>();
    let after = window
        .events
        .iter()
        .filter(|event| event.seq > window.target.seq)
        .collect::<Vec<_>>();
    if !before.is_empty() {
        lines.extend([String::new(), "Before:".to_owned()]);
        lines.extend(before.into_iter().map(format_neighbor));
    }
    if !after.is_empty() {
        lines.extend([String::new(), "After:".to_owned()]);
        lines.extend(after.into_iter().map(format_neighbor));
    }
    Ok(lines.join("\n"))
}

fn format_neighbor(event: &SessionEvent) -> String {
    let text = extract_session_event_text(event);
    format!(
        "- seq {} | {} | {}{}",
        event.seq,
        event.event_type,
        format_time(event.time),
        if text.is_empty() {
            " | (no semantic text)".to_owned()
        } else {
            format!("\n  {}", text.replace('\n', "\n  "))
        }
    )
}

fn availability_text(record: &SessionRecord) -> String {
    let value = [
        record.live.then_some("live"),
        record.persisted.then_some("persisted"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(", ");
    if value.is_empty() {
        "unavailable".to_owned()
    } else {
        value
    }
}

fn seq_list(values: &[u64]) -> String {
    if values.is_empty() {
        "none".to_owned()
    } else {
        values
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn format_time(value: i64) -> String {
    chrono::DateTime::from_timestamp_millis(value).map_or_else(
        || "Invalid Date".to_owned(),
        |time| time.to_rfc3339_opts(SecondsFormat::Millis, true),
    )
}

fn format_time_u64(value: u64) -> String {
    i64::try_from(value).map_or_else(|_| "Invalid Date".to_owned(), format_time)
}

fn surface_text(surface: seekdeep_session_query::SessionEventSurface) -> &'static str {
    match surface {
        seekdeep_session_query::SessionEventSurface::Current => "current",
        seekdeep_session_query::SessionEventSurface::Shadowed => "shadowed",
        seekdeep_session_query::SessionEventSurface::LogOnly => "log-only",
    }
}

/// Pending-call presenter for session search.
pub fn present_session_search_call(args: &SessionSearchArgs) -> GenericCallView {
    generic(
        "Search prior sessions",
        ToolCallKind::Search,
        Some(json!(args.query)),
    )
}

/// Pending-call presenter for event search.
pub fn present_event_search_call(args: &EventSearchArgs) -> GenericCallView {
    generic(
        "Search session events",
        ToolCallKind::Search,
        Some(json!(args.query)),
    )
}

/// Pending-call presenter for lineage trace.
pub fn present_session_trace_call(args: &SessionTargetArgs) -> GenericCallView {
    args.session_id.as_ref().map_or_else(
        || generic("Trace current session", ToolCallKind::Read, None),
        |id| {
            generic(
                &format!("Trace session {id}"),
                ToolCallKind::Read,
                Some(json!(id)),
            )
        },
    )
}

/// Pending-call presenter for event trace/read.
pub fn present_event_target_call(action: &str, args: &EventTargetArgs) -> GenericCallView {
    let raw_input = args.session_id.as_ref().map_or_else(
        || json!({"seq": args.seq}),
        |session_id| json!({"session_id": session_id, "seq": args.seq}),
    );
    generic(
        &format!("{action} {}", args.seq),
        ToolCallKind::Read,
        Some(raw_input),
    )
}

fn generic(
    title: &str,
    kind: ToolCallKind,
    raw_input: Option<serde_json::Value>,
) -> GenericCallView {
    GenericCallView {
        title: title.to_owned(),
        kind: Some(kind),
        raw_input,
        content: None,
        locations: None,
    }
}
