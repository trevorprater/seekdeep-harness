//! Goal mutation wire contracts.

use seekdeep_core::session::SessionId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    rpc::ContractError,
    sessions::{
        decode, optional_positive_integer, optional_string, require_field, require_literal_true,
        require_object, require_positive_integer, require_string,
    },
};

seekdeep_util::string_brand!(
    /// Stable Goal identity across durable revisions.
    pub struct GoalId;
);

/// Compare-and-set identity for one exact Goal revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalRef {
    /// Goal identity; the schema permits the empty string.
    pub id: GoalId,
    /// Positive revision.
    pub revision: u64,
}

impl GoalRef {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_string(object, "id", "$.id", false)?;
        require_positive_integer(object, "revision", "$.revision")?;
        decode(value)
    }
}

/// `goal.create` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalCreateRequest {
    /// Session id; this schema deliberately permits empty.
    pub session_id: SessionId,
    /// Non-empty objective.
    pub objective: String,
    /// Optional positive automatic-round cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_goal_rounds: Option<u64>,
}

impl GoalCreateRequest {
    /// Parses a `goal.create` request.
    ///
    /// # Errors
    ///
    /// Returns an error for non-string ids, empty objectives, or a non-positive cap.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_string(object, "sessionId", "$.sessionId", false)?;
        require_string(object, "objective", "$.objective", true)?;
        optional_positive_integer(object, "maxGoalRounds", "$.maxGoalRounds")?;
        decode(value)
    }
}

/// `goal.edit` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalEditRequest {
    /// Session id; this schema deliberately permits empty.
    pub session_id: SessionId,
    /// Expected current Goal revision.
    pub r#ref: GoalRef,
    /// Optional replacement objective.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    /// Optional replacement round cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_goal_rounds: Option<u64>,
}

impl GoalEditRequest {
    /// Parses a `goal.edit` request and requires at least one replacement field.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed fields or an edit with no replacement.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_string(object, "sessionId", "$.sessionId", false)?;
        GoalRef::parse(require_field(object, "ref", "$.ref")?)?;
        optional_string(object, "objective", "$.objective", true)?;
        optional_positive_integer(object, "maxGoalRounds", "$.maxGoalRounds")?;
        if !object.contains_key("objective") && !object.contains_key("maxGoalRounds") {
            return Err(ContractError::new(
                "$",
                "goal.edit requires objective or maxGoalRounds",
            ));
        }
        decode(value)
    }
}

/// Shared request for pause/resume/complete/clear.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalRefRequest {
    /// Session id; this schema deliberately permits empty.
    #[serde(rename = "sessionId")]
    pub session_id: SessionId,
    /// Expected current Goal revision.
    pub r#ref: GoalRef,
}

impl GoalRefRequest {
    /// Parses a Goal CAS mutation request.
    ///
    /// # Errors
    ///
    /// Returns an error for a malformed Session id or Goal ref.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_string(object, "sessionId", "$.sessionId", false)?;
        GoalRef::parse(require_field(object, "ref", "$.ref")?)?;
        decode(value)
    }
}

/// `goal.pause` request.
pub type GoalPauseRequest = GoalRefRequest;
/// `goal.resume` request.
pub type GoalResumeRequest = GoalRefRequest;
/// `goal.complete` request.
pub type GoalCompleteRequest = GoalRefRequest;
/// `goal.clear` request.
pub type GoalClearRequest = GoalRefRequest;

/// Shared acknowledgement for every non-clear Goal mutation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalRefValue {
    /// New committed Goal revision.
    pub r#ref: GoalRef,
}

impl GoalRefValue {
    /// Parses a Goal-ref acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing or malformed ref.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            r#ref: GoalRef::parse(require_field(object, "ref", "$.ref")?)?,
        })
    }
}

/// `goal.create` response.
pub type GoalCreateValue = GoalRefValue;
/// `goal.edit` response.
pub type GoalEditValue = GoalRefValue;
/// `goal.pause` response.
pub type GoalPauseValue = GoalRefValue;
/// `goal.resume` response.
pub type GoalResumeValue = GoalRefValue;
/// `goal.complete` response.
pub type GoalCompleteValue = GoalRefValue;

/// `goal.clear` response.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalClearValue {
    /// Must be literal true.
    pub cleared: bool,
}

impl GoalClearValue {
    /// Parses a Goal-clear acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns an error unless `cleared` is literal true.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_literal_true(object, "cleared", "$.cleared")?;
        Ok(Self { cleared: true })
    }
}
