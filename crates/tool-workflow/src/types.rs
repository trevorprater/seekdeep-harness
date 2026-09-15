//! Durable workflow-record events written by the workflow tool.

use seekdeep_core::session::SessionId;
use seekdeep_workflow::{WorkflowAgentOutcome, WorkflowRunId, WorkflowStopReason};
use serde::{Deserialize, Serialize};

/// Opens one durable top-level workflow run record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolWorkflowRunStartData {
    /// Stable run identity.
    pub run_id: WorkflowRunId,
    /// Display name.
    pub name: String,
}

/// Records one workflow member after its child session is published.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolWorkflowAgentStartData {
    /// Stable run identity.
    pub run_id: WorkflowRunId,
    /// 1-based member sequence.
    pub seq: u64,
    /// Display label.
    pub label: String,
    /// Optional phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// Child session id.
    pub child_id: SessionId,
}

/// Settles one previously started workflow member.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolWorkflowAgentEndData {
    /// Stable run identity.
    pub run_id: WorkflowRunId,
    /// Paired member sequence.
    pub seq: u64,
    /// How the member settled.
    pub outcome: WorkflowAgentOutcome,
}

/// Settles one workflow run after its live resources reach quiescence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolWorkflowRunEndData {
    /// Stable run identity.
    pub run_id: WorkflowRunId,
    /// Terminal reason.
    pub stop_reason: WorkflowStopReason,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn agent_start_round_trips_optional_phase() {
        let data = ToolWorkflowAgentStartData {
            run_id: WorkflowRunId::new("r1"),
            seq: 2,
            label: "first".to_owned(),
            phase: Some("build".to_owned()),
            child_id: SessionId::new("c1"),
        };
        let value = serde_json::to_value(&data).expect("serialize");
        assert_eq!(value["runId"], json!("r1"));
        assert_eq!(value["seq"], json!(2));
        assert_eq!(value["phase"], json!("build"));
        let decoded: ToolWorkflowAgentStartData =
            serde_json::from_value(value).expect("deserialize");
        assert_eq!(decoded.child_id.as_str(), "c1");
    }
}
