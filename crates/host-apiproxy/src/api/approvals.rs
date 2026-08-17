//! Approval response wire contracts.

use seekdeep_core::session::SessionId;
use seekdeep_user_approval::{ApprovalOutcome, ApprovalRequestId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    rpc::ContractError,
    sessions::{require_nonempty_string, require_object, require_string},
};

/// Client-answerable approval outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalResponseOutcome {
    /// Allow this call once.
    AllowedOnce,
    /// Explicit rejection.
    Rejected,
}

impl From<ApprovalResponseOutcome> for ApprovalOutcome {
    fn from(value: ApprovalResponseOutcome) -> Self {
        match value {
            ApprovalResponseOutcome::AllowedOnce => Self::AllowedOnce,
            ApprovalResponseOutcome::Rejected => Self::Rejected,
        }
    }
}

/// Result value of an approval Client response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalResponsePayload {
    /// Owning Session.
    pub session_id: SessionId,
    /// Core audit correlation identity.
    pub approval_id: ApprovalRequestId,
    /// One of the two outcomes a Client may supply.
    pub outcome: ApprovalResponseOutcome,
}

impl ApprovalResponsePayload {
    /// Parses an approval response payload.
    ///
    /// # Errors
    ///
    /// Returns an error for empty ids or a Host-only/unknown outcome.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let session_id = require_nonempty_string(object, "sessionId", "$.sessionId")?;
        let approval_id = require_nonempty_string(object, "approvalId", "$.approvalId")?;
        let outcome = match require_string(object, "outcome", "$.outcome", false)? {
            "allowed-once" => ApprovalResponseOutcome::AllowedOnce,
            "rejected" => ApprovalResponseOutcome::Rejected,
            _ => return Err(ContractError::new("$.outcome", "unknown approval response")),
        };
        Ok(Self {
            session_id: SessionId::new(session_id),
            approval_id: ApprovalRequestId::new(approval_id),
            outcome,
        })
    }
}
