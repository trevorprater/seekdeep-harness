//! Subagent descendant aggregation over the retained Session-list mirror.

use std::collections::{BTreeMap, BTreeSet};

use seekdeep_identity::SessionId;

/// Session summary fields used by subagent-lineage aggregation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentSessionSummary {
    /// Stable Session identity.
    pub id: SessionId,
    /// Parent Session identity.
    pub parent_id: Option<SessionId>,
    /// Whether the durable origin is subagent.
    pub subagent_origin: bool,
    /// Current exact Session running state.
    pub running: bool,
}

/// Descendant totals for one possible parent Session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SubagentDescendantSummary {
    /// All descendants connected by uninterrupted subagent-origin lineage.
    pub count: usize,
    /// Descendants whose exact summary is running.
    pub running_count: usize,
}

/// Indexes each subagent descendant under every uninterrupted subagent-origin ancestor.
#[must_use]
pub fn index_subagent_descendants(
    summaries: &BTreeMap<SessionId, SubagentSessionSummary>,
) -> BTreeMap<SessionId, SubagentDescendantSummary> {
    let mut indexed = BTreeMap::<SessionId, SubagentDescendantSummary>::new();
    for descendant in summaries.values().filter(|summary| summary.subagent_origin) {
        let mut seen = BTreeSet::new();
        let mut current = Some(descendant);
        while let Some(summary) = current.filter(|summary| summary.subagent_origin) {
            let Some(parent) = &summary.parent_id else {
                break;
            };
            if !seen.insert(summary.id.clone()) {
                break;
            }
            let aggregate = indexed.entry(parent.clone()).or_default();
            aggregate.count += 1;
            if descendant.running {
                aggregate.running_count += 1;
            }
            current = summaries.get(parent);
        }
    }
    indexed
}
