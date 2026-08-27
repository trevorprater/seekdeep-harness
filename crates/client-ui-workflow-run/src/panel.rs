//! Pure renderer decisions for workflow-run disclosure and navigation.

use std::collections::{BTreeMap, BTreeSet};

use seekdeep_identity::SessionId;

use crate::{WorkflowRunPhaseData, WorkflowRunStatus};

/// State-dot semantic consumed by the primitive renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkflowDotState {
    /// Live work.
    Ongoing,
    /// Clean completion.
    Done,
    /// Failure.
    Error,
    /// Cancellation or interruption.
    Warning,
}

/// Maps workflow status to the primitive's exact dot state.
#[must_use]
pub const fn workflow_dot_state(status: WorkflowRunStatus) -> WorkflowDotState {
    match status {
        WorkflowRunStatus::Running => WorkflowDotState::Ongoing,
        WorkflowRunStatus::Completed => WorkflowDotState::Done,
        WorkflowRunStatus::Failed => WorkflowDotState::Error,
        WorkflowRunStatus::Cancelled | WorkflowRunStatus::Interrupted => WorkflowDotState::Warning,
    }
}

/// Whether a phase must remain force-open because any member is not complete.
#[must_use]
pub fn phase_requires_expansion(phase: &WorkflowRunPhaseData) -> bool {
    phase
        .members
        .iter()
        .any(|member| member.status != WorkflowRunStatus::Completed)
}

/// Ordered status/count tokens shown by one phase summary.
#[must_use]
pub fn phase_status_counts(phase: &WorkflowRunPhaseData) -> Vec<(WorkflowRunStatus, usize)> {
    let mut counts = BTreeMap::new();
    for member in &phase.members {
        *counts.entry(status_rank(member.status)).or_insert(0_usize) += 1;
    }
    let count = |status| counts.get(&status_rank(status)).copied().unwrap_or(0);
    let active = [
        WorkflowRunStatus::Running,
        WorkflowRunStatus::Failed,
        WorkflowRunStatus::Cancelled,
        WorkflowRunStatus::Interrupted,
    ]
    .into_iter()
    .filter(|status| count(*status) > 0)
    .collect::<Vec<_>>();
    if active.is_empty() {
        return vec![(
            WorkflowRunStatus::Completed,
            count(WorkflowRunStatus::Completed),
        )];
    }
    let mut visible = Vec::new();
    if active.contains(&WorkflowRunStatus::Interrupted) && count(WorkflowRunStatus::Completed) > 0 {
        visible.push(WorkflowRunStatus::Completed);
    }
    visible.extend(active);
    visible
        .into_iter()
        .map(|status| (status, count(status)))
        .collect()
}

const fn status_rank(status: WorkflowRunStatus) -> u8 {
    match status {
        WorkflowRunStatus::Running => 0,
        WorkflowRunStatus::Completed => 1,
        WorkflowRunStatus::Failed => 2,
        WorkflowRunStatus::Cancelled => 3,
        WorkflowRunStatus::Interrupted => 4,
    }
}

/// Minimal Session summary fields that authorize workflow-member navigation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkflowSessionSummary {
    /// Stable Session identity.
    pub id: SessionId,
    /// Whether the Session originated as a subagent.
    pub subagent: bool,
    /// Parent Session identity.
    pub parent_id: Option<SessionId>,
    /// Whether the Session is currently running.
    pub running: bool,
}

/// Returns running ordinary subagent members that belong to this exact parent.
#[must_use]
pub fn navigable_members(
    ordinary_ids: &[SessionId],
    summaries: &[WorkflowSessionSummary],
    phases: &[WorkflowRunPhaseData],
    parent_id: &SessionId,
) -> Vec<SessionId> {
    let ordinary = ordinary_ids.iter().collect::<BTreeSet<_>>();
    let by_id = summaries
        .iter()
        .map(|summary| (&summary.id, summary))
        .collect::<BTreeMap<_, _>>();
    phases
        .iter()
        .flat_map(|phase| phase.members.iter())
        .filter(|member| member.status == WorkflowRunStatus::Running)
        .filter_map(|member| {
            let summary = by_id.get(&member.child_id)?;
            (ordinary.contains(&member.child_id)
                && summary.subagent
                && summary.parent_id.as_ref() == Some(parent_id)
                && summary.running)
                .then(|| member.child_id.clone())
        })
        .collect()
}

/// Whether the run must stay force-open instead of becoming manually collapsible.
#[must_use]
pub fn run_requires_expansion(status: WorkflowRunStatus, phases: &[WorkflowRunPhaseData]) -> bool {
    status != WorkflowRunStatus::Completed || phases.iter().any(phase_requires_expansion)
}
