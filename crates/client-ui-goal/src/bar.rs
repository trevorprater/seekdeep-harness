//! Deterministic local state for the Goal composer dock.

use serde::{Deserialize, Serialize};

/// Browser-facing goal phase vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalBarPhase {
    /// Active continuation.
    Active,
    /// Paused continuation.
    Paused,
    /// Blocked continuation.
    Blocked,
    /// Completed goals have no bar.
    Complete,
}

/// Minimal projection consumed by the Goal bar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalBarSnapshot {
    /// Stable identity.
    pub id: String,
    /// Positive CAS revision.
    pub revision: u64,
    /// Human objective.
    pub objective: String,
    /// Durable phase.
    pub phase: GoalBarPhase,
    /// Human block explanation, when blocked.
    pub blocked_reason: Option<GoalBarBlockReason>,
}

/// Blocked-goal explanation used for the bar tooltip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalBarBlockReason {
    /// Stable machine code.
    pub code: String,
    /// Human message.
    pub message: String,
}

/// One local action whose success changes immediate UI state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalBarAction {
    /// Replace the objective.
    Edit,
    /// Pause.
    Pause,
    /// Resume.
    Resume,
    /// Clear.
    Clear,
}

/// Local Goal bar state with a same-render single-flight gate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GoalBarController {
    goal_id: Option<String>,
    editing: bool,
    draft: String,
    pending: bool,
    action_error: Option<String>,
    cleared_goal_id: Option<String>,
}

impl GoalBarController {
    /// Creates the source initial state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reconciles identity changes and invalidates stale edit/clear state.
    pub fn reconcile(&mut self, goal: Option<&GoalBarSnapshot>) {
        let next = goal.map(|goal| goal.id.as_str());
        if self.goal_id.as_deref() == next {
            return;
        }
        self.goal_id = next.map(ToOwned::to_owned);
        self.editing = false;
        self.action_error = None;
        self.cleared_goal_id = None;
    }

    /// Whether this projection currently renders a bar.
    #[must_use]
    pub fn visible(&self, goal: Option<&GoalBarSnapshot>) -> bool {
        goal.is_some_and(|goal| {
            goal.phase != GoalBarPhase::Complete
                && self.cleared_goal_id.as_deref() != Some(goal.id.as_str())
        })
    }

    /// Whether the inline edit form is active.
    #[must_use]
    pub const fn editing(&self) -> bool {
        self.editing
    }

    /// Current edit draft.
    #[must_use]
    pub fn draft(&self) -> &str {
        &self.draft
    }

    /// Whether an action is in flight.
    #[must_use]
    pub const fn pending(&self) -> bool {
        self.pending
    }

    /// Rendered action failure.
    #[must_use]
    pub fn action_error(&self) -> Option<&str> {
        self.action_error.as_deref()
    }

    /// Opens an objective edit prefilled from the live goal.
    pub fn begin_edit(&mut self, objective: &str) {
        if self.pending {
            return;
        }
        objective.clone_into(&mut self.draft);
        self.editing = true;
    }

    /// Replaces the verbatim edit draft.
    pub fn set_draft(&mut self, draft: impl Into<String>) {
        self.draft = draft.into();
    }

    /// Cancels editing without retaining the stale draft on a later edit.
    pub fn cancel_edit(&mut self) {
        if !self.pending {
            self.editing = false;
        }
    }

    /// Begins one action unless a same-render or rendered pending gate is active.
    #[must_use]
    pub fn begin_action(&mut self) -> bool {
        if self.pending {
            return false;
        }
        self.pending = true;
        self.action_error = None;
        true
    }

    /// Settles one remote action and applies immediate source-local state.
    pub fn settle_action(
        &mut self,
        action: GoalBarAction,
        goal_id: Option<&str>,
        result: Result<(), (&str, &str)>,
    ) {
        self.pending = false;
        match result {
            Ok(()) => match action {
                GoalBarAction::Edit => self.editing = false,
                GoalBarAction::Clear => {
                    self.cleared_goal_id = goal_id.map(ToOwned::to_owned);
                }
                GoalBarAction::Pause | GoalBarAction::Resume => {}
            },
            Err((code, message)) => {
                self.action_error = Some(format!("{message} ({code})"));
            }
        }
    }
}
