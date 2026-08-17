//! Agent-preset roster and authoring wire contracts.

use seekdeep_core::session::SessionId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    host::EmptyRequest,
    rpc::ContractError,
    sessions::{
        decode, optional_nonempty_string, optional_string, parse_array, require_array,
        require_bool, require_nonempty_string, require_object, require_string,
    },
};

/// Closed trust origin of one agent preset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentPresetTrust {
    /// Ships with the deployment.
    System,
    /// Authored locally.
    User,
}

/// One preset the deployment can compose an Agent from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPresetEntry {
    /// Stable non-empty id.
    pub id: String,
    /// System or user origin.
    pub trust: AgentPresetTrust,
    /// Whether omission selects this preset.
    pub is_default: bool,
    /// Optional published display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Optional published description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional non-empty composition failure reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broken: Option<String>,
}

impl AgentPresetEntry {
    /// Parses one agent-preset roster row.
    ///
    /// # Errors
    ///
    /// Returns an error for empty ids, unknown trust, missing defaults, or malformed metadata.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "id", "$.id")?;
        parse_trust(object)?;
        require_bool(object, "isDefault", "$.isDefault")?;
        optional_string(object, "name", "$.name", false)?;
        optional_string(object, "description", "$.description", false)?;
        optional_nonempty_string(object, "broken", "$.broken")?;
        decode(value)
    }
}

/// `agentPreset.list` request.
pub type AgentPresetListRequest = EmptyRequest;

/// `agentPreset.list` response value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPresetListValue {
    /// Roster in root-precedence order.
    pub presets: Vec<AgentPresetEntry>,
    /// Whether a writable authoring root exists.
    pub authorable: bool,
    /// Whether native document opening is available.
    pub has_document: bool,
}

impl AgentPresetListValue {
    /// Parses an `agentPreset.list` response value.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed rows or missing deployment capability flags.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            presets: parse_array(
                require_array(object, "presets", "$.presets")?,
                AgentPresetEntry::parse,
                "$.presets",
            )?,
            authorable: require_bool(object, "authorable", "$.authorable")?,
            has_document: require_bool(object, "hasDocument", "$.hasDocument")?,
        })
    }
}

/// `agentPreset.select` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPresetSelectRequest {
    /// Blank Session to recompose.
    pub session_id: SessionId,
    /// Non-empty preset id.
    pub agent_preset: String,
}

impl AgentPresetSelectRequest {
    /// Parses an `agentPreset.select` request.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty Session or preset id.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "sessionId", "$.sessionId")?;
        require_nonempty_string(object, "agentPreset", "$.agentPreset")?;
        decode(value)
    }
}

/// Value or request carrying one preset id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPresetIdValue {
    /// Preset id; response schemas permit empty while request parsers do not.
    pub agent_preset: String,
}

impl AgentPresetIdValue {
    /// Parses a response carrying a string preset id.
    ///
    /// # Errors
    ///
    /// Returns an error unless `agentPreset` is present and string-valued.
    pub fn parse_value(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_string(object, "agentPreset", "$.agentPreset", false)?;
        decode(value)
    }

    /// Parses a privileged request carrying a non-empty preset id.
    ///
    /// # Errors
    ///
    /// Returns an error unless `agentPreset` is a non-empty string.
    pub fn parse_request(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "agentPreset", "$.agentPreset")?;
        decode(value)
    }
}

/// `agentPreset.select` response.
pub type AgentPresetSelectValue = AgentPresetIdValue;
/// `agentPreset.read` request.
pub type AgentPresetReadRequest = AgentPresetIdValue;
/// `agentPreset.openDocument` request.
pub type AgentPresetOpenDocumentRequest = AgentPresetIdValue;
/// `agentPreset.remove` request.
pub type AgentPresetRemoveRequest = AgentPresetIdValue;
/// `agentPreset.copy` response.
pub type AgentPresetCopyValue = AgentPresetIdValue;

/// `agentPreset.read` response value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPresetReadValue {
    /// Preset id.
    pub agent_preset: String,
    /// Trust origin.
    pub trust: AgentPresetTrust,
    /// Complete composition text.
    pub content: String,
    /// Optional published name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Optional published description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl AgentPresetReadValue {
    /// Parses an `agentPreset.read` response value.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed required or optional members.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_string(object, "agentPreset", "$.agentPreset", false)?;
        parse_trust(object)?;
        require_string(object, "content", "$.content", false)?;
        optional_string(object, "name", "$.name", false)?;
        optional_string(object, "description", "$.description", false)?;
        decode(value)
    }
}

/// `agentPreset.copy` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPresetCopyRequest {
    /// Non-empty source id.
    pub from: String,
    /// Non-empty destination id.
    pub agent_preset: String,
    /// Optional display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl AgentPresetCopyRequest {
    /// Parses an `agentPreset.copy` request.
    ///
    /// # Errors
    ///
    /// Returns an error for empty source/destination ids or a malformed name.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "from", "$.from")?;
        require_nonempty_string(object, "agentPreset", "$.agentPreset")?;
        optional_string(object, "name", "$.name", false)?;
        decode(value)
    }
}

/// `agentPreset.openDocument` response value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentPresetOpenDocumentValue {
    /// Native opener accepted the directory.
    Opened {
        /// Must be true.
        opened: bool,
    },
    /// No native opener; show this resolved path.
    Path {
        /// Must be false.
        opened: bool,
        /// Host-resolved directory.
        path: String,
    },
}

impl AgentPresetOpenDocumentValue {
    /// Parses the closed native-open/path union.
    ///
    /// # Errors
    ///
    /// Returns an error unless true stands alone or false carries a string path.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        if require_bool(object, "opened", "$.opened")? {
            Ok(Self::Opened { opened: true })
        } else {
            Ok(Self::Path {
                opened: false,
                path: require_string(object, "path", "$.path", false)?.to_owned(),
            })
        }
    }
}

/// `agentPreset.remove` response.
pub type AgentPresetRemoveValue = EmptyRequest;

fn parse_trust(object: &serde_json::Map<String, Value>) -> Result<AgentPresetTrust, ContractError> {
    match require_string(object, "trust", "$.trust", false)? {
        "system" => Ok(AgentPresetTrust::System),
        "user" => Ok(AgentPresetTrust::User),
        _ => Err(ContractError::new("$.trust", "unknown preset trust")),
    }
}
