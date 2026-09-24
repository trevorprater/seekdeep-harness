//! Session-addressed read-only Skill catalog contracts.

use seekdeep_core::session::SessionId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    rpc::ContractError,
    sessions::{
        decode, optional_string, parse_array, require_array, require_bool, require_nonempty_string,
        require_object, require_string,
    },
};

/// One wire projection of a Host Skill summary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillEntry {
    /// Non-empty identifier referenced as `/name`.
    pub name: String,
    /// Short routing description.
    pub description: String,
    /// Optional extra routing guidance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    /// Whether the model may invoke this Skill.
    pub model_invocable: bool,
}

impl SkillEntry {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "name", "$.name")?;
        require_string(object, "description", "$.description", false)?;
        optional_string(object, "whenToUse", "$.whenToUse", false)?;
        require_bool(object, "modelInvocable", "$.modelInvocable")?;
        decode(value)
    }
}

/// `skill.list` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillListRequest {
    /// Session whose project root scopes discovery.
    pub session_id: SessionId,
}

impl SkillListRequest {
    /// Parses a `skill.list` request.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing, non-string, or empty Session id.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "sessionId", "$.sessionId")?;
        decode(value)
    }
}

/// `skill.list` response value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillListValue {
    /// User-invocable Skill catalog.
    pub skills: Vec<SkillEntry>,
}

impl SkillListValue {
    /// Parses a `skill.list` response value.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing list or any malformed Skill row.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            skills: parse_array(
                require_array(object, "skills", "$.skills")?,
                SkillEntry::parse,
                "$.skills",
            )?,
        })
    }
}
