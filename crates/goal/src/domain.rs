//! Host-side vocabulary of the goal domain.

use serde::{Deserialize, Serialize};

use crate::types::{GoalId, GoalRef, GoalSnapshot, GoalView};

/// Goal state-changing verbs recorded in the durable source change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalOperation {
    /// Create.
    Create,
    /// Edit.
    Edit,
    /// Pause.
    Pause,
    /// Resume.
    Resume,
    /// Complete.
    Complete,
    /// Block.
    Block,
    /// Clear.
    Clear,
}

/// Full-snapshot goal mutation committed by a durable goal/change event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalSnapshotChangeMeta {
    /// Source kind.
    pub kind: GoalChangeKind,
    /// Wire version.
    pub version: u32,
    /// Mutation operation, never clear.
    pub operation: GoalOperation,
    /// Post-mutation snapshot.
    pub goal: GoalSnapshot,
    /// Highest admitted round.
    pub rounds_started: u64,
    /// Create epoch milliseconds.
    pub created_at: u64,
    /// Mutation epoch milliseconds.
    pub updated_at: u64,
}

/// Tombstone retained when the current goal is cleared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalClearChangeMeta {
    /// Source kind.
    pub kind: GoalChangeKind,
    /// Wire version.
    pub version: u32,
    /// Clear operation.
    pub operation: GoalClearOperation,
    /// Cleared reference.
    pub cleared: GoalRef,
    /// Clear epoch milliseconds.
    pub cleared_at: u64,
}

/// Closed source kind for goal/change messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoalChangeKind {
    /// The goal/change source kind.
    #[serde(rename = "goal/change")]
    GoalChange,
}

/// Closed clear operation marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "clear")]
pub enum GoalClearOperation {
    /// Clear.
    Clear,
}

/// Message attribution for admitted continuation rounds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalMessageSource {
    /// Source kind.
    pub kind: GoalSourceKind,
    /// Owning goal identity.
    pub goal_id: GoalId,
    /// Revision at admission.
    pub revision: u64,
    /// Positive admitted continuation round.
    pub round: u64,
}

/// Closed source kind for goal messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "goal")]
pub enum GoalSourceKind {
    /// Goal.
    Goal,
}

/// Pure replay fold of durable goal facts.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FoldedGoal {
    /// Current goal, absent after a clear or before the first create.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalSnapshot>,
    /// Highest admitted round for the current goal.
    pub rounds_started: u64,
    /// Current goal creation time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
    /// Current goal mutation time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,
    /// Latest mutation ref, including a clear tombstone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ref: Option<GoalRef>,
}

/// Decoded durable goal change union.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GoalChangeMeta {
    /// Full-snapshot mutation.
    Snapshot(GoalSnapshotChangeMeta),
    /// Clear tombstone.
    Clear(GoalClearChangeMeta),
}

/// Live notification after one durable goal mutation commits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalChanged {
    /// Mutation operation.
    pub operation: GoalOperation,
    /// Mutated reference.
    #[serde(rename = "ref")]
    pub goal_ref: GoalRef,
    /// Fresh view, absent for a clear tombstone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalView>,
}

/// Stable error codes for rejected goal reads and mutations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GoalErrorCode {
    /// The owning agent is not live.
    GoalAgentNotLive,
    /// The goal does not exist.
    GoalNotFound,
    /// A goal already exists.
    GoalAlreadyExists,
    /// The revision is stale.
    GoalStaleRevision,
    /// The objective is invalid.
    GoalInvalidObjective,
    /// The round cap is invalid.
    GoalInvalidMaxRounds,
    /// The block reason is invalid.
    GoalInvalidBlockReason,
    /// The edit is invalid.
    GoalInvalidEdit,
    /// The transition is invalid.
    GoalInvalidTransition,
}
