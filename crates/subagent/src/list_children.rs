//! Read-only enumeration of durable subagent children and descendant trees.

use std::{collections::HashMap, sync::Arc};

use seekdeep_core::session::{SessionHeader, SessionId};
use seekdeep_core::session_store::SESSIONS;
use seekdeep_llm::AbortSignal;
use seekdeep_session_persistence::SESSION_PERSISTENCE;
use seekdeep_session_projection::SESSION_PROJECTIONS;
use serde::{Deserialize, Serialize};

use crate::error::SubagentError;
use crate::projection_types::SubagentIdentityProjection;

/// How one entry is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentActivity {
    /// Live in the session store.
    Running,
    /// Present only in persistence.
    Inactive,
}

/// Why a candidate has no child row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentDiagnosticReason {
    /// Deterministic data damage.
    Corrupt,
    /// Never produced; reserved for consumers routing on it.
    Unsupported,
    /// A persistence read failed.
    Unavailable,
}

/// One entry of a child listing result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SubagentListEntry {
    /// A served child.
    Child {
        /// The durable child session id.
        id: SessionId,
        /// Store snapshot activity.
        activity: SubagentActivity,
        /// Whether a direct descendant is also a subagent.
        has_children: bool,
        /// Durable mode and creation label.
        #[serde(flatten)]
        mode: SubagentListMode,
    },
    /// A candidate without a child row.
    Diagnostic {
        /// The candidate's session id.
        id: SessionId,
        /// Why no child row.
        reason: SubagentDiagnosticReason,
    },
}

/// Durable mode and creation label for a served child.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum SubagentListMode {
    /// A terminal one-shot child.
    #[serde(rename = "one-shot")]
    OneShot {
        /// Optional durable creation label.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// A resumable conversation.
    Continuable {
        /// Durable creation label.
        label: String,
    },
}

/// One entry of a descendant listing, with tree position.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentDescendantListEntry {
    /// The interpreted subagent facts.
    #[serde(flatten)]
    pub entry: SubagentListEntry,
    /// Durable direct parent.
    #[serde(rename = "parentId")]
    pub parent_id: SessionId,
    /// Edge distance from the requested root.
    pub depth: u64,
}

struct CorpusRecord {
    header: SessionHeader,
    live: Option<Arc<seekdeep_core::session::Session>>,
}

fn assert_not_cancelled(signal: Option<&AbortSignal>) -> anyhow::Result<()> {
    if signal.is_some_and(AbortSignal::is_aborted) {
        return Err(SubagentError::new("subagent listing was cancelled", "CANCELLED").into());
    }
    Ok(())
}

fn compare_records(a: &CorpusRecord, b: &CorpusRecord) -> std::cmp::Ordering {
    a.header
        .created_at
        .cmp(&b.header.created_at)
        .then_with(|| a.header.id.as_str().cmp(b.header.id.as_str()))
}

fn child_row(
    id: &SessionId,
    identity: SubagentIdentityProjection,
    activity: SubagentActivity,
    has_children: bool,
) -> SubagentListEntry {
    let mode = match identity {
        SubagentIdentityProjection::OneShot { label, .. } => SubagentListMode::OneShot { label },
        SubagentIdentityProjection::Continuable { label, .. } => {
            SubagentListMode::Continuable { label }
        }
    };
    SubagentListEntry::Child {
        id: id.clone(),
        activity,
        has_children,
        mode,
    }
}

fn identity_of(value: Option<&serde_json::Value>) -> Option<SubagentIdentityProjection> {
    serde_json::from_value(value?.clone()).ok()
}

/// Enumerates one parent's origin-classified direct children.
///
/// # Errors
///
/// Returns when the projection registry or session store is missing, or the
/// caller cancels the listing.
pub async fn list_children(
    ctx: &seekdeep_cordis::Context,
    parent_session_id: &SessionId,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<Vec<SubagentListEntry>> {
    let corpus = build_corpus(ctx, signal).await?;
    let mut candidates: Vec<&CorpusRecord> = corpus
        .values()
        .filter(|record| {
            record.header.parent_session.as_ref() == Some(parent_session_id)
                && record.header.origin == Some(seekdeep_core::session::SessionOrigin::Subagent)
        })
        .collect();
    candidates.sort_by(|a, b| compare_records(a, b));
    resolve_rows(ctx, &candidates, signal)
        .await
        .map(|rows| rows.into_iter().flatten().collect())
}

/// Enumerates every session-backed subagent below one root in stable pre-order.
///
/// # Errors
///
/// Returns under the same conditions as `list_children`.
pub async fn list_descendants(
    ctx: &seekdeep_cordis::Context,
    root_session_id: &SessionId,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<Vec<SubagentDescendantListEntry>> {
    let corpus = build_corpus(ctx, signal).await?;
    let positioned = descendant_candidates(&corpus, root_session_id);
    let records: Vec<&CorpusRecord> = positioned.iter().map(|p| p.record).collect();
    let rows = resolve_rows(ctx, &records, signal).await?;
    let mut entries = Vec::new();
    for (index, position) in positioned.iter().enumerate() {
        if let Some(row) = rows.get(index).cloned().flatten() {
            entries.push(SubagentDescendantListEntry {
                entry: row,
                parent_id: position.parent_id.clone(),
                depth: position.depth,
            });
        }
    }
    Ok(entries)
}

async fn build_corpus(
    ctx: &seekdeep_cordis::Context,
    signal: Option<&AbortSignal>,
) -> anyhow::Result<HashMap<SessionId, CorpusRecord>> {
    if ctx.get(SESSION_PROJECTIONS).is_none() {
        return Err(SubagentError::new(
            "listing subagents requires the sessionProjections registry (load seekdeep-session-projection)",
            "SUBAGENT_CONTROL_PROJECTIONS_UNAVAILABLE",
        )
        .into());
    }
    let sessions = ctx.get(SESSIONS).ok_or_else(|| {
        SubagentError::new(
            "listing subagents requires the session store (load seekdeep-session)",
            "SUBAGENT_CONTROL_SESSION_STORE_UNAVAILABLE",
        )
    })?;
    assert_not_cancelled(signal)?;
    let persistence = ctx.get(SESSION_PERSISTENCE);
    let mut corpus: HashMap<SessionId, CorpusRecord> = HashMap::new();
    if let Some(persistence) = &persistence {
        let headers = persistence.persistence().list(signal.cloned()).await?;
        assert_not_cancelled(signal)?;
        for header in headers {
            corpus.insert(header.id.clone(), CorpusRecord { header, live: None });
        }
    }
    for session in sessions.list() {
        let header = session.header().clone();
        corpus.insert(
            header.id.clone(),
            CorpusRecord {
                header,
                live: Some(session),
            },
        );
    }
    Ok(corpus)
}

async fn resolve_rows(
    ctx: &seekdeep_cordis::Context,
    candidates: &[&CorpusRecord],
    signal: Option<&AbortSignal>,
) -> anyhow::Result<Vec<Option<SubagentListEntry>>> {
    let projections = ctx
        .get(SESSION_PROJECTIONS)
        .ok_or_else(|| anyhow::anyhow!("sessionProjections is missing"))?;
    let persistence = ctx.get(SESSION_PERSISTENCE);
    let mut rows: Vec<Option<SubagentListEntry>> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let child_id = candidate.header.id.clone();
        let has_children = candidates
            .iter()
            .any(|c| c.header.parent_session.as_ref() == Some(&child_id));
        if let Some(live) = &candidate.live {
            let identity = projections
                .snapshot(live)
                .ok()
                .and_then(|snapshot| identity_of(snapshot.values.get("subagent")));
            match identity {
                Some(identity) => rows.push(Some(child_row(
                    &child_id,
                    identity,
                    SubagentActivity::Running,
                    has_children,
                ))),
                None => rows.push(None),
            }
        } else if let Some(persistence) = &persistence {
            let row = resolve_cold(
                &persistence.persistence(),
                &projections,
                &candidate.header,
                has_children,
                signal,
            )
            .await;
            rows.push(row);
        } else {
            rows.push(None);
        }
    }
    assert_not_cancelled(signal)?;
    Ok(rows)
}

async fn resolve_cold(
    persistence: &Arc<dyn seekdeep_session_persistence::SessionPersistence>,
    projections: &seekdeep_session_projection::SessionProjectionRegistry,
    header: &SessionHeader,
    has_children: bool,
    signal: Option<&AbortSignal>,
) -> Option<SubagentListEntry> {
    let child_id = header.id.clone();
    assert_not_cancelled(signal).ok()?;
    let inspected = persistence.inspect(&child_id, signal.cloned()).await.ok()?;
    assert_not_cancelled(signal).ok()?;
    if !same_lifecycle(&inspected.meta, header) {
        return Some(SubagentListEntry::Diagnostic {
            id: child_id,
            reason: SubagentDiagnosticReason::Corrupt,
        });
    }
    let restored = projections
        .restore(&indexmap::IndexMap::new(), &inspected.events, 0)
        .ok();
    match restored.and_then(|restore| identity_of(restore.snapshot.values.get("subagent"))) {
        Some(identity) => Some(child_row(
            &child_id,
            identity,
            SubagentActivity::Inactive,
            has_children,
        )),
        None => Some(SubagentListEntry::Diagnostic {
            id: child_id,
            reason: SubagentDiagnosticReason::Corrupt,
        }),
    }
}

fn same_lifecycle(meta: &SessionHeader, expected: &SessionHeader) -> bool {
    meta.version == expected.version
        && meta.id == expected.id
        && meta.created_at == expected.created_at
        && meta.cwd == expected.cwd
        && meta.parent_session == expected.parent_session
        && meta.seed_length == expected.seed_length
        && meta.delegation_depth == expected.delegation_depth
}

fn descendant_candidates<'a>(
    corpus: &'a HashMap<SessionId, CorpusRecord>,
    root_session_id: &SessionId,
) -> Vec<PositionedCandidate<'a>> {
    let mut children: HashMap<SessionId, Vec<&CorpusRecord>> = HashMap::new();
    for record in corpus.values() {
        if let Some(parent_id) = &record.header.parent_session {
            children.entry(parent_id.clone()).or_default().push(record);
        }
    }
    for siblings in children.values_mut() {
        siblings.sort_by(|a, b| compare_records(a, b));
    }
    let mut positioned: Vec<PositionedCandidate> = Vec::new();
    let mut stack: Vec<PositionedCandidate> = children
        .get(root_session_id)
        .map(|siblings| {
            siblings
                .iter()
                .rev()
                .map(|record| PositionedCandidate {
                    record,
                    parent_id: root_session_id.clone(),
                    depth: 1,
                })
                .collect()
        })
        .unwrap_or_default();
    let mut visited = std::collections::HashSet::new();
    visited.insert(root_session_id.clone());
    while let Some(position) = stack.pop() {
        let id = position.record.header.id.clone();
        let depth = position.depth;
        if !visited.insert(id.clone()) {
            continue;
        }
        if position.record.header.origin == Some(seekdeep_core::session::SessionOrigin::Subagent) {
            positioned.push(position);
        }
        if let Some(descendants) = children.get(&id) {
            for record in descendants.iter().rev() {
                stack.push(PositionedCandidate {
                    record,
                    parent_id: id.clone(),
                    depth: depth + 1,
                });
            }
        }
    }
    positioned
}

struct PositionedCandidate<'a> {
    record: &'a CorpusRecord,
    parent_id: SessionId,
    depth: u64,
}
