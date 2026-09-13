//! Browser-safe background-job wire contracts.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    rpc::ContractError,
    sessions::{
        optional_nonnegative_integer, optional_string, require_nonempty_string,
        require_nonnegative_integer, require_object, require_string,
    },
};

seekdeep_util::string_brand!(
    /// Stable registry-issued background Job identity.
    pub struct JobId;
);

/// Closed Job lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    /// Producer is running.
    Running,
    /// Stop was requested but settlement has not arrived.
    Stopping,
    /// Producer completed normally.
    Completed,
    /// Producer was killed.
    Killed,
    /// Producer failed.
    Failed,
}

/// One background Job as the Client sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    /// Non-empty registry-issued identity.
    pub id: JobId,
    /// Open producer kind.
    pub kind: String,
    /// Non-empty producer label.
    pub label: String,
    /// Current closed lifecycle state.
    pub status: JobStatus,
    /// Optional producer detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Non-negative registration epoch milliseconds.
    pub started_at: u64,
    /// Optional non-negative settlement epoch milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
}

impl JobView {
    /// Parses one wire Job view.
    ///
    /// # Errors
    ///
    /// Returns an error for empty identity/kind/label, unknown status, or invalid timestamps.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "id", "$.id")?;
        require_nonempty_string(object, "kind", "$.kind")?;
        require_nonempty_string(object, "label", "$.label")?;
        match require_string(object, "status", "$.status", false)? {
            "running" | "stopping" | "completed" | "killed" | "failed" => {}
            _ => return Err(ContractError::new("$.status", "unknown Job status")),
        }
        optional_string(object, "detail", "$.detail", false)?;
        require_nonnegative_integer(object, "startedAt", "$.startedAt")?;
        optional_nonnegative_integer(object, "finishedAt", "$.finishedAt")?;
        serde_json::from_value(value.clone())
            .map_err(|error| ContractError::new("$", error.to_string()))
    }
}
