//! Workflow seam vocabulary: request/run/result types and event payloads.

use seekdeep_core::session::SessionId;
use serde::{Deserialize, Serialize};

seekdeep_util::string_brand!(
    /// Identifies one workflow run.
    pub struct WorkflowRunId;
);

/// One phase declared in a script meta.phases.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowPhase {
    /// Phase title.
    pub title: String,
    /// Optional one-line description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Optional provider override (informational).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Optional model override (informational).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// The script's identity block.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkflowMeta {
    /// Short kebab-case workflow name.
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Optional guidance on when this workflow applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    /// Optional phase declarations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phases: Option<Vec<WorkflowPhase>>,
}

/// Why a run settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowStopReason {
    /// The script ran to its final return.
    Completed,
    /// The run was cancelled.
    Cancelled,
    /// The script threw or failed materialization.
    Error,
}

/// The outcome resolved by a live workflow run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowResult {
    /// The script's materialized return value (host JSON data).
    pub value: serde_json::Value,
    /// Why the run settled.
    pub stop_reason: WorkflowStopReason,
    /// The failure message (present iff not completed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// How many `agent()` calls the run accepted.
    pub agents_started: u64,
}

/// Identifying detail for a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunInfo {
    /// The run's id.
    pub id: WorkflowRunId,
    /// The run's validated meta block.
    pub meta: WorkflowMeta,
}

/// One `agent()` call's identity within a run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowAgentInfo {
    /// 1-based sequence number.
    pub seq: u64,
    /// The display label.
    pub label: String,
    /// The phase this agent belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// The child agent's session id.
    pub child_id: SessionId,
}

/// How one `agent()` call settled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowAgentOutcome {
    /// Clean result.
    Completed,
    /// Child failure.
    Failed,
    /// Run cancellation.
    Cancelled,
}

/// One `agent()` call's settlement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowAgentEndInfo {
    /// The call identity.
    #[serde(flatten)]
    pub info: WorkflowAgentInfo,
    /// How the call settled.
    pub outcome: WorkflowAgentOutcome,
}

/// A settled run's outcome as event data (minus the value).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowResultInfo {
    /// Why the run settled.
    pub stop_reason: WorkflowStopReason,
    /// The failure message (present iff not completed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// How many `agent()` calls the run accepted.
    pub agents_started: u64,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn agent_end_info_flattens_the_identity_fields() {
        let end = WorkflowAgentEndInfo {
            info: WorkflowAgentInfo {
                seq: 1,
                label: "first".to_owned(),
                phase: Some("build".to_owned()),
                child_id: SessionId::new("c1"),
            },
            outcome: WorkflowAgentOutcome::Completed,
        };
        let value = serde_json::to_value(&end).expect("serialize");
        assert_eq!(value["seq"], json!(1));
        assert_eq!(value["label"], json!("first"));
        assert_eq!(value["childId"], json!("c1"));
        assert_eq!(value["outcome"], json!("completed"));
    }

    #[test]
    fn meta_round_trips_with_phases() {
        let meta = WorkflowMeta {
            name: "audit".to_owned(),
            description: "Audit files".to_owned(),
            when_to_use: None,
            phases: Some(vec![WorkflowPhase {
                title: "scan".to_owned(),
                detail: None,
                provider: None,
                model: None,
            }]),
        };
        let value = serde_json::to_value(&meta).expect("serialize");
        assert_eq!(value["name"], json!("audit"));
        assert_eq!(value["phases"][0]["title"], json!("scan"));
        let decoded: WorkflowMeta = serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded.name, "audit");
    }
}
