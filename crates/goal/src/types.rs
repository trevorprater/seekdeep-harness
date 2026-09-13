//! Pure types of the goal domain.

use serde::{Deserialize, Serialize};

seekdeep_util::string_brand!(
    /// Identifies one goal across its durable revisions.
    pub struct GoalId;
);

/// Compare-and-set identity for one exact goal revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalRef {
    /// Stable goal identity.
    pub id: GoalId,
    /// Positive revision; every durable mutation increments it.
    pub revision: u64,
}

/// Input whose omitted round cap is resolved by the service configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateGoalRequest {
    /// Human-requested completion objective.
    pub objective: String,
    /// Optional round cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_goal_rounds: Option<u64>,
}

/// Wire-safe acknowledgement of one created goal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateGoalResult {
    /// The created goal reference.
    #[serde(rename = "ref")]
    pub goal_ref: GoalRef,
}

/// Fields changed by an edit; at least one must be present.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditGoalRequest {
    /// Replacement objective.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    /// Replacement round cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_goal_rounds: Option<u64>,
}

/// Durable continuation phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalPhase {
    /// Actively continuing.
    Active,
    /// Paused.
    Paused,
    /// Blocked with a reason.
    Blocked,
    /// Complete.
    Complete,
}

/// Machine-routable and human-readable explanation for a blocked goal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalBlockReason {
    /// Stable lower-kebab-case classification.
    pub code: String,
    /// Non-empty explanation shown to humans and models.
    pub message: String,
}

/// Full durable state written by every non-clear goal mutation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalSnapshot {
    /// Stable goal identity.
    pub id: GoalId,
    /// Positive revision.
    pub revision: u64,
    /// Human-requested completion objective.
    pub objective: String,
    /// Durable lifecycle phase.
    pub phase: GoalPhase,
    /// Present exactly while phase is blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<GoalBlockReason>,
    /// Total admitted goal-round cap.
    pub max_goal_rounds: u64,
}

/// Whether this live process may automatically continue an active goal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalActivation {
    /// Continuation is armed.
    Armed,
    /// Continuation is disarmed.
    Disarmed,
}

/// Current goal projection, including values derived from the session log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalView {
    /// Stable goal identity.
    pub id: GoalId,
    /// Positive revision.
    pub revision: u64,
    /// Human-requested completion objective.
    pub objective: String,
    /// Durable lifecycle phase.
    pub phase: GoalPhase,
    /// Present exactly while phase is blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<GoalBlockReason>,
    /// Total admitted goal-round cap.
    pub max_goal_rounds: u64,
    /// Highest admitted round number for this goal.
    pub rounds_started: u64,
    /// Epoch milliseconds of the create mutation.
    pub created_at: u64,
    /// Epoch milliseconds of the latest mutation.
    pub updated_at: u64,
    /// Process-local continuation eligibility; never persisted.
    pub activation: GoalActivation,
}

/// The goal projection value: the current durable goal with its replay counters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalProjection {
    /// Current durable goal snapshot.
    pub goal: GoalSnapshot,
    /// Highest admitted round number for this goal.
    pub rounds_started: u64,
    /// Epoch milliseconds of the create mutation.
    pub created_at: u64,
    /// Epoch milliseconds of the latest mutation.
    pub updated_at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_and_activation_round_trip() {
        assert_eq!(
            serde_json::to_string(&GoalPhase::Active).expect("phase"),
            "\"active\""
        );
        assert_eq!(
            serde_json::to_string(&GoalActivation::Disarmed).expect("activation"),
            "\"disarmed\""
        );
    }

    #[test]
    fn snapshot_round_trips_blocked_reason() {
        let snapshot = GoalSnapshot {
            id: GoalId::new("g1"),
            revision: 3,
            objective: "port it".to_owned(),
            phase: GoalPhase::Blocked,
            blocked_reason: Some(GoalBlockReason {
                code: "needs-approval".to_owned(),
                message: "approval prompts disabled".to_owned(),
            }),
            max_goal_rounds: 256,
        };
        let value = serde_json::to_value(&snapshot).expect("serialize");
        assert_eq!(value["objective"], "port it");
        assert_eq!(value["blockedReason"]["code"], "needs-approval");
        let decoded: GoalSnapshot = serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded, snapshot);
    }
}
