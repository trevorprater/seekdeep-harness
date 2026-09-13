//! Stable DFS Session-lineage projection with orphan and cycle degradation.

use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use seekdeep_identity::SessionId;
use serde_json::Value;

/// Host Session summary enriched with Client list projections.
#[derive(Clone, Debug, PartialEq)]
pub struct TitledSessionSummary {
    /// Stable Session identity.
    pub session_id: SessionId,
    /// Durable or projected title.
    pub title: Option<String>,
    /// Last update timestamp.
    pub updated_at: i64,
    /// Whether the Agent is running.
    pub running: bool,
    /// Whether the durable log is empty.
    pub blank: bool,
    /// Parent Session for subagent lineage.
    pub parent_session_id: Option<SessionId>,
    /// Coarse durable origin.
    pub origin: Option<String>,
    /// Working directory.
    pub cwd: Option<String>,
    /// Agent preset identity.
    pub agent_preset: Option<String>,
    /// Current Host-computed projections.
    pub projection_values: Option<Value>,
}

/// One flattened Session-list row.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionListEntry {
    /// Source summary.
    pub summary: TitledSessionSummary,
    /// Current blocking interaction.
    pub pending_interaction: Option<Value>,
    /// Unread completion reminder.
    pub completed: bool,
    /// Root-zero lineage depth.
    pub depth: usize,
}

/// Cycle warning sink.
pub type LineageLogger = Rc<dyn Fn(&SessionId)>;

/// Flattens authoritative Session order while making each child adjacent to its parent.
#[must_use]
pub fn flatten_lineage(
    summaries: &[TitledSessionSummary],
    pending_interactions: &BTreeMap<SessionId, Value>,
    completed: &BTreeSet<SessionId>,
    warn: &LineageLogger,
) -> Vec<SessionListEntry> {
    let by_id = summaries
        .iter()
        .map(|summary| (summary.session_id.clone(), summary))
        .collect::<BTreeMap<_, _>>();
    let mut children = BTreeMap::<SessionId, Vec<&TitledSessionSummary>>::new();
    let mut roots = Vec::new();
    for summary in summaries {
        match &summary.parent_session_id {
            Some(parent) if by_id.contains_key(parent) => {
                children.entry(parent.clone()).or_default().push(summary);
            }
            Some(_) | None => roots.push(summary),
        }
    }
    let mut output = Vec::new();
    let mut visited = BTreeSet::new();
    for root in roots {
        walk(
            root,
            0,
            &children,
            pending_interactions,
            completed,
            &mut visited,
            &mut output,
            warn,
        );
    }
    for summary in summaries {
        if !visited.contains(&summary.session_id) {
            walk(
                summary,
                0,
                &children,
                pending_interactions,
                completed,
                &mut visited,
                &mut output,
                warn,
            );
        }
    }
    output
}

#[allow(clippy::too_many_arguments)]
fn walk(
    summary: &TitledSessionSummary,
    depth: usize,
    children: &BTreeMap<SessionId, Vec<&TitledSessionSummary>>,
    pending: &BTreeMap<SessionId, Value>,
    completed: &BTreeSet<SessionId>,
    visited: &mut BTreeSet<SessionId>,
    output: &mut Vec<SessionListEntry>,
    warn: &LineageLogger,
) {
    if !visited.insert(summary.session_id.clone()) {
        warn(&summary.session_id);
        return;
    }
    output.push(SessionListEntry {
        summary: summary.clone(),
        pending_interaction: pending.get(&summary.session_id).cloned(),
        completed: completed.contains(&summary.session_id),
        depth,
    });
    if let Some(descendants) = children.get(&summary.session_id) {
        for child in descendants {
            walk(
                child,
                depth + 1,
                children,
                pending,
                completed,
                visited,
                output,
                warn,
            );
        }
    }
}
