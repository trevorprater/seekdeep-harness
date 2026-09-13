//! Browser-safe Session-backed subagent wire contracts.

use seekdeep_core::session::SessionId;
use seekdeep_llm::MessageId;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{
    rpc::ContractError,
    sessions::{
        AcceptedValue, SessionHistoryValue, optional_nonnegative_integer,
        optional_positive_integer, optional_string, parse_array, require_array, require_bool,
        require_nonempty_string, require_object, require_string, validate_content_block,
    },
};

/// Healthy child activity state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentActivity {
    /// Child driver is running.
    Running,
    /// Child is inactive.
    Inactive,
}

/// Durable child mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubagentMode {
    /// One-shot child.
    OneShot,
    /// Human-continuable child.
    Continuable,
}

/// Diagnostic reason for an unreadable catalog child.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentDiagnosticReason {
    /// Durable record is corrupt.
    Corrupt,
    /// Durable format is unsupported.
    Unsupported,
    /// Storage is temporarily unavailable.
    Unavailable,
}

/// Complete durable direct-child catalog row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SubagentListEntry {
    /// Healthy child.
    #[serde(rename = "child", rename_all = "camelCase")]
    Child {
        /// Child Session id.
        id: SessionId,
        /// One-shot or continuable.
        mode: SubagentMode,
        /// Current sampled activity.
        activity: SubagentActivity,
        /// Whether a durable direct descendant exists.
        has_children: bool,
        /// Optional for one-shot; required for continuable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Unreadable durable child.
    #[serde(rename = "diagnostic")]
    Diagnostic {
        /// Child Session id.
        id: SessionId,
        /// Stable diagnostic class.
        reason: SubagentDiagnosticReason,
    },
}

impl SubagentListEntry {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let id = SessionId::new(require_nonempty_string(object, "id", "$.id")?);
        match require_string(object, "kind", "$.kind", false)? {
            "child" => {
                let mode = parse_mode(object, false)?;
                let activity = match require_string(object, "activity", "$.activity", false)? {
                    "running" => SubagentActivity::Running,
                    "inactive" => SubagentActivity::Inactive,
                    _ => return Err(ContractError::new("$.activity", "unknown activity")),
                };
                let has_children = require_bool(object, "hasChildren", "$.hasChildren")?;
                optional_string(object, "label", "$.label", false)?;
                let label = object
                    .get("label")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                if mode == SubagentMode::Continuable && label.is_none() {
                    return Err(ContractError::new(
                        "$.label",
                        "continuable child requires label",
                    ));
                }
                Ok(Self::Child {
                    id,
                    mode,
                    activity,
                    has_children,
                    label,
                })
            }
            "diagnostic" => Ok(Self::Diagnostic {
                id,
                reason: match require_string(object, "reason", "$.reason", false)? {
                    "corrupt" => SubagentDiagnosticReason::Corrupt,
                    "unsupported" => SubagentDiagnosticReason::Unsupported,
                    "unavailable" => SubagentDiagnosticReason::Unavailable,
                    _ => {
                        return Err(ContractError::new("$.reason", "unknown diagnostic reason"));
                    }
                },
            }),
            _ => Err(ContractError::new("$.kind", "unknown subagent row kind")),
        }
    }
}

/// `subagent.list` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentListRequest {
    /// Direct parent Session.
    pub parent_session_id: SessionId,
}

impl SubagentListRequest {
    /// Parses a `subagent.list` request.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty parent Session id.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "parentSessionId", "$.parentSessionId")?;
        serde_json::from_value(value.clone())
            .map_err(|error| ContractError::new("$", error.to_string()))
    }
}

/// `subagent.list` response value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentListValue {
    /// Complete direct-child catalog.
    pub entries: Vec<SubagentListEntry>,
    /// Delivery-time parent availability hint.
    pub parent_available: bool,
}

impl SubagentListValue {
    /// Parses a `subagent.list` response value.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed catalog rows or missing availability state.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            entries: parse_array(
                require_array(object, "entries", "$.entries")?,
                SubagentListEntry::parse,
                "$.entries",
            )?,
            parent_available: require_bool(object, "parentAvailable", "$.parentAvailable")?,
        })
    }
}

/// `subagent.history` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentHistoryRequest {
    /// Durable direct parent.
    pub parent_session_id: SessionId,
    /// Durable child.
    pub child_session_id: SessionId,
    /// Child mode.
    pub mode: SubagentMode,
    /// Optional backwards-page cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_seq: Option<u64>,
    /// Optional positive page size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_messages: Option<u64>,
}

impl SubagentHistoryRequest {
    /// Parses a `subagent.history` request.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed address, mode, cursor, or page size.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_address(object)?;
        parse_mode(object, false)?;
        optional_nonnegative_integer(object, "beforeSeq", "$.beforeSeq")?;
        optional_positive_integer(object, "maxMessages", "$.maxMessages")?;
        serde_json::from_value(value.clone())
            .map_err(|error| ContractError::new("$", error.to_string()))
    }
}

/// `subagent.history` response value, identical to an ordinary Session history page.
pub type SubagentHistoryValue = SessionHistoryValue;

/// `subagent.prompt` request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentPromptRequest {
    /// Durable direct parent.
    pub parent_session_id: SessionId,
    /// Durable child.
    pub child_session_id: SessionId,
    /// Must be continuable.
    pub mode: SubagentMode,
    /// Merge-extensible core content.
    pub content: Vec<Map<String, Value>>,
    /// Optional browser-local IANA zone candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_time_zone: Option<String>,
}

impl SubagentPromptRequest {
    /// Parses a `subagent.prompt` request.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed address, non-continuable mode, content, or zone.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let (parent_session_id, child_session_id) = require_address(object)?;
        let mode = parse_mode(object, true)?;
        let content = parse_array(
            require_array(object, "content", "$.content")?,
            validate_content_block,
            "$.content",
        )?;
        optional_string(object, "clientTimeZone", "$.clientTimeZone", false)?;
        Ok(Self {
            parent_session_id,
            child_session_id,
            mode,
            content,
            client_time_zone: object
                .get("clientTimeZone")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        })
    }
}

/// `subagent.interrupt` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentInterruptRequest {
    /// Durable direct parent.
    pub parent_session_id: SessionId,
    /// Durable child.
    pub child_session_id: SessionId,
    /// Must be continuable.
    pub mode: SubagentMode,
}

impl SubagentInterruptRequest {
    /// Parses a `subagent.interrupt` request.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed address or non-continuable mode.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_address(object)?;
        parse_mode(object, true)?;
        serde_json::from_value(value.clone())
            .map_err(|error| ContractError::new("$", error.to_string()))
    }
}

/// `subagent.interrupt` response.
pub type SubagentInterruptValue = AcceptedValue;

/// `subagent.prompt` response value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentPromptValue {
    /// Accepted inbox message identity; this schema permits empty.
    pub message_id: MessageId,
}

impl SubagentPromptValue {
    /// Parses a `subagent.prompt` response value.
    ///
    /// # Errors
    ///
    /// Returns an error unless `messageId` is string-valued.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            message_id: MessageId::new(require_string(object, "messageId", "$.messageId", false)?),
        })
    }
}

fn require_address(
    object: &serde_json::Map<String, Value>,
) -> Result<(SessionId, SessionId), ContractError> {
    Ok((
        SessionId::new(require_nonempty_string(
            object,
            "parentSessionId",
            "$.parentSessionId",
        )?),
        SessionId::new(require_nonempty_string(
            object,
            "childSessionId",
            "$.childSessionId",
        )?),
    ))
}

fn parse_mode(
    object: &serde_json::Map<String, Value>,
    continuable_only: bool,
) -> Result<SubagentMode, ContractError> {
    match require_string(object, "mode", "$.mode", false)? {
        "one-shot" if !continuable_only => Ok(SubagentMode::OneShot),
        "continuable" => Ok(SubagentMode::Continuable),
        _ => Err(ContractError::new("$.mode", "invalid subagent mode")),
    }
}
