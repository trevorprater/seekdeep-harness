//! Deterministic Workspace-browser ordering, query, and drop resolution.

use std::collections::{BTreeMap, BTreeSet};

use indexmap::IndexMap;
use seekdeep_identity::{SessionId, WorkspaceId};

use crate::SessionOrderBy;

/// `session.search` wire bound in JavaScript UTF-16 code units.
pub const SEARCH_QUERY_MAX_CODE_UNITS: usize = 500;

/// Insert-marker half of one rendered row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropHalf {
    /// Insert immediately before the target row.
    Before,
    /// Insert immediately after the target row.
    After,
}

/// Resolved local Session order and optional Host insert-before anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionDrop {
    /// Full browser-local order after the drop.
    pub order: Vec<SessionId>,
    /// Host insert-before anchor; absence appends.
    pub before: Option<SessionId>,
}

/// Resolved Host Workspace insert-before anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceDrop {
    /// Host insert-before anchor; absence appends.
    pub before: Option<WorkspaceId>,
}

/// One reconciled editable Session-order account and timestamp baseline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionOrderAccount {
    /// Reconciled or promoted order.
    pub order: Vec<SessionId>,
    /// Current timestamps for summaries that have landed.
    pub updated_at: IndexMap<String, i64>,
    /// Whether either persisted projection differs.
    pub changed: bool,
}

/// Removes NULs and truncates without splitting one Unicode scalar's UTF-16 pair.
#[must_use]
pub fn sanitize_search_query(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut units = 0;
    for character in value.chars().filter(|character| *character != '\0') {
        let width = character.len_utf16();
        if units + width > SEARCH_QUERY_MAX_CODE_UNITS {
            break;
        }
        output.push(character);
        units += width;
    }
    output
}

/// Reconciles one persisted order with the current account, deduplicating stale keys.
#[must_use]
pub fn reconciled_session_order(
    session_ids: &[SessionId],
    stored: Option<&[String]>,
) -> Vec<SessionId> {
    let Some(stored) = stored else {
        return session_ids.to_vec();
    };
    let by_id = session_ids
        .iter()
        .map(|id| (id.as_str(), id))
        .collect::<BTreeMap<_, _>>();
    let mut included = BTreeSet::new();
    let mut ordered = Vec::with_capacity(session_ids.len());
    for key in stored {
        let Some(id) = by_id.get(key.as_str()) else {
            continue;
        };
        if included.insert(key.as_str()) {
            ordered.push((*id).clone());
        }
    }
    for id in session_ids {
        if included.insert(id.as_str()) {
            ordered.push(id.clone());
        }
    }
    ordered
}

fn by_recency(
    left: &SessionId,
    right: &SessionId,
    timestamps: &IndexMap<SessionId, i64>,
) -> std::cmp::Ordering {
    timestamps
        .get(right)
        .copied()
        .unwrap_or(i64::MIN)
        .cmp(&timestamps.get(left).copied().unwrap_or(i64::MIN))
        .then_with(|| left.cmp(right))
}

/// Reconciles one editable account and applies the exact activity-promotion policy.
#[must_use]
pub fn next_session_order_account(
    session_ids: &[SessionId],
    previous_order: Option<&[String]>,
    previous_updated_at: &IndexMap<String, i64>,
    timestamps: &IndexMap<SessionId, i64>,
    order_by: SessionOrderBy,
    sort_by_recency: bool,
) -> SessionOrderAccount {
    let mut order = reconciled_session_order(session_ids, previous_order);
    if sort_by_recency {
        order.sort_by(|left, right| by_recency(left, right, timestamps));
    } else if order_by == SessionOrderBy::Updated {
        let mut promoted = session_ids
            .iter()
            .filter(|id| {
                timestamps.get(*id).is_some_and(|timestamp| {
                    previous_updated_at
                        .get(id.as_str())
                        .is_none_or(|previous| timestamp > previous)
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        promoted.sort_by(|left, right| by_recency(left, right, timestamps));
        if !promoted.is_empty() {
            let promoted_ids = promoted.iter().cloned().collect::<BTreeSet<_>>();
            promoted.extend(order.into_iter().filter(|id| !promoted_ids.contains(id)));
            order = promoted;
        }
    }
    let updated_at = session_ids
        .iter()
        .filter_map(|id| {
            timestamps
                .get(id)
                .map(|timestamp| (id.as_str().to_owned(), *timestamp))
        })
        .collect::<IndexMap<_, _>>();
    let order_changed = previous_order.is_none_or(|previous| {
        order.len() != previous.len()
            || order
                .iter()
                .zip(previous)
                .any(|(id, previous)| id.as_str() != previous)
    });
    let timestamps_changed = updated_at.len() != previous_updated_at.len()
        || updated_at
            .iter()
            .any(|(id, timestamp)| previous_updated_at.get(id) != Some(timestamp));
    SessionOrderAccount {
        order,
        updated_at,
        changed: order_changed || timestamps_changed,
    }
}

/// Resolves one Session drop, rejecting missing targets and visual no-ops.
#[must_use]
pub fn resolve_session_drop(
    visible: &[SessionId],
    account: &[SessionId],
    source: &SessionId,
    target: &SessionId,
    half: DropHalf,
) -> Option<SessionDrop> {
    let target_index = visible.iter().position(|id| id == target)?;
    let before = match half {
        DropHalf::Before => Some(target.clone()),
        DropHalf::After => visible.get(target_index + 1).cloned(),
    };
    if before.as_ref() == Some(source) {
        return None;
    }
    let source_index = visible.iter().position(|id| id == source);
    let anchor_index = before
        .as_ref()
        .and_then(|anchor| visible.iter().position(|id| id == anchor))
        .unwrap_or(visible.len());
    if source_index.is_some_and(|source_index| {
        anchor_index == source_index || anchor_index == source_index + 1
    }) {
        return None;
    }
    let mut order = account
        .iter()
        .filter(|id| *id != source)
        .cloned()
        .collect::<Vec<_>>();
    let insert_at = before
        .as_ref()
        .and_then(|anchor| order.iter().position(|id| id == anchor))
        .unwrap_or(order.len());
    order.insert(insert_at, source.clone());
    Some(SessionDrop { order, before })
}

/// Resolves one Workspace drop, rejecting missing targets and visual no-ops.
#[must_use]
pub fn resolve_workspace_drop(
    workspaces: &[WorkspaceId],
    source: &WorkspaceId,
    target: &WorkspaceId,
    half: DropHalf,
) -> Option<WorkspaceDrop> {
    let target_index = workspaces.iter().position(|id| id == target)?;
    let before = match half {
        DropHalf::Before => Some(target.clone()),
        DropHalf::After => workspaces.get(target_index + 1).cloned(),
    };
    if before.as_ref() == Some(source) {
        return None;
    }
    let source_index = workspaces.iter().position(|id| id == source);
    let anchor_index = before
        .as_ref()
        .and_then(|anchor| workspaces.iter().position(|id| id == anchor))
        .unwrap_or(workspaces.len());
    if source_index.is_some_and(|source_index| {
        anchor_index == source_index || anchor_index == source_index + 1
    }) {
        return None;
    }
    Some(WorkspaceDrop { before })
}
