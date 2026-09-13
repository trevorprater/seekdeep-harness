//! Workspace-domain API contracts.

use seekdeep_core::session::SessionId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    host::EmptyRequest,
    rpc::ContractError,
    sessions::{
        decode, is_ecmascript_whitespace, optional_nonempty_string, parse_array, require_array,
        require_bool, require_field, require_literal_true, require_nonempty_string, require_object,
        require_string,
    },
};

seekdeep_util::string_brand!(
    /// Stable opaque Workspace identity.
    pub struct WorkspaceId;
);

/// One Workspace record projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceView {
    /// Stable Workspace identity.
    pub workspace_id: WorkspaceId,
    /// Canonical directory path.
    pub path: String,
    /// Display title.
    pub title: String,
    /// Manually ordered accounted Sessions.
    pub session_ids: Vec<SessionId>,
    /// ISO-8601 creation instant.
    pub created_at: String,
    /// ISO-8601 last-mutation instant.
    pub updated_at: String,
}

impl WorkspaceView {
    /// Parses and normalizes one Workspace wire row.
    ///
    /// # Errors
    ///
    /// Returns an error for empty identities or malformed required fields.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "workspaceId", "$.workspaceId")?;
        require_string(object, "path", "$.path", false)?;
        require_string(object, "title", "$.title", false)?;
        let session_ids = require_array(object, "sessionIds", "$.sessionIds")?;
        for (index, id) in session_ids.iter().enumerate() {
            if id.as_str().is_none_or(str::is_empty) {
                return Err(ContractError::new(
                    format!("$.sessionIds[{index}]"),
                    "expected non-empty string",
                ));
            }
        }
        require_string(object, "createdAt", "$.createdAt", false)?;
        require_string(object, "updatedAt", "$.updatedAt", false)?;
        decode(value)
    }
}

/// `workspace.list` request.
pub type WorkspaceListRequest = EmptyRequest;

/// `workspace.list` response value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceListValue {
    /// Registry rows in durable display order.
    pub items: Vec<WorkspaceView>,
    /// Registry-global archive set.
    pub archived_session_ids: Vec<SessionId>,
}

impl WorkspaceListValue {
    /// Parses a `workspace.list` response value.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed Workspace rows or archived Session ids.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let items = parse_array(
            require_array(object, "items", "$.items")?,
            WorkspaceView::parse,
            "$.items",
        )?;
        let archived = parse_session_ids(
            require_array(object, "archivedSessionIds", "$.archivedSessionIds")?,
            "$.archivedSessionIds",
        )?;
        Ok(Self {
            items,
            archived_session_ids: archived,
        })
    }
}

/// `workspace.create` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCreateRequest {
    /// Existing directory to adopt; the schema permits an empty string and the Host rejects it.
    pub path: String,
}

impl WorkspaceCreateRequest {
    /// Parses a `workspace.create` request.
    ///
    /// # Errors
    ///
    /// Returns an error unless `path` is present and string-valued.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_string(object, "path", "$.path", false)?;
        decode(value)
    }
}

/// `workspace.create` response value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCreateValue {
    /// Created or idempotently resolved Workspace.
    pub workspace: WorkspaceView,
    /// Whether a new durable record was created.
    pub created: bool,
}

impl WorkspaceCreateValue {
    /// Parses a `workspace.create` response value.
    ///
    /// # Errors
    ///
    /// Returns an error for a malformed Workspace row or creation marker.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            workspace: WorkspaceView::parse(require_field(object, "workspace", "$.workspace")?)?,
            created: require_bool(object, "created", "$.created")?,
        })
    }
}

/// `workspace.rename` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRenameRequest {
    /// Workspace to rename.
    pub workspace_id: WorkspaceId,
    /// Raw non-blank title; the wire preserves surrounding whitespace.
    pub title: String,
}

impl WorkspaceRenameRequest {
    /// Parses a `workspace.rename` request and enforces non-blank title content.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty id, non-string title, or ECMAScript-blank title.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "workspaceId", "$.workspaceId")?;
        let title = require_string(object, "title", "$.title", false)?;
        if title.trim_matches(is_ecmascript_whitespace).is_empty() {
            return Err(ContractError::new(
                "$",
                "workspace.rename requires a non-blank title",
            ));
        }
        decode(value)
    }
}

/// Response carrying one complete Workspace row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceValue {
    /// Updated Workspace.
    pub workspace: WorkspaceView,
}

impl WorkspaceValue {
    /// Parses a response containing one Workspace.
    ///
    /// # Errors
    ///
    /// Returns an error when the required Workspace row is malformed.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            workspace: WorkspaceView::parse(require_field(object, "workspace", "$.workspace")?)?,
        })
    }
}

/// `workspace.rename` response.
pub type WorkspaceRenameValue = WorkspaceValue;
/// `workspace.insertSessionBefore` response.
pub type WorkspaceInsertSessionBeforeValue = WorkspaceValue;

/// Request carrying one Workspace id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceIdRequest {
    /// Workspace identity.
    pub workspace_id: WorkspaceId,
}

impl WorkspaceIdRequest {
    /// Parses a request with a non-empty `workspaceId`.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing, non-string, or empty identity.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "workspaceId", "$.workspaceId")?;
        decode(value)
    }
}

/// `workspace.delete` request.
pub type WorkspaceDeleteRequest = WorkspaceIdRequest;

/// `workspace.delete` response.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDeleteValue {
    /// Must be literal true.
    pub deleted: bool,
}

impl WorkspaceDeleteValue {
    /// Parses a successful deletion receipt.
    ///
    /// # Errors
    ///
    /// Returns an error unless `deleted` is literal true.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_literal_true(object, "deleted", "$.deleted")?;
        Ok(Self { deleted: true })
    }
}

/// `workspace.insertBefore` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInsertBeforeRequest {
    /// Workspace to move.
    pub workspace_id: WorkspaceId,
    /// Optional insertion anchor; absence appends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_workspace_id: Option<WorkspaceId>,
}

impl WorkspaceInsertBeforeRequest {
    /// Parses a `workspace.insertBefore` request.
    ///
    /// # Errors
    ///
    /// Returns an error for empty or malformed Workspace identities.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "workspaceId", "$.workspaceId")?;
        optional_nonempty_string(object, "beforeWorkspaceId", "$.beforeWorkspaceId")?;
        decode(value)
    }
}

/// `workspace.insertBefore` response with the complete durable order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInsertBeforeValue {
    /// Complete ordered Workspace identity set.
    pub workspace_ids: Vec<WorkspaceId>,
}

impl WorkspaceInsertBeforeValue {
    /// Parses a complete Workspace ordering.
    ///
    /// # Errors
    ///
    /// Returns an error for any empty or non-string identity.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let values = require_array(object, "workspaceIds", "$.workspaceIds")?;
        let workspace_ids = values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                value
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .map(WorkspaceId::new)
                    .ok_or_else(|| {
                        ContractError::new(
                            format!("$.workspaceIds[{index}]"),
                            "expected non-empty string",
                        )
                    })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { workspace_ids })
    }
}

/// `workspace.insertSessionBefore` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInsertSessionBeforeRequest {
    /// Owning Workspace.
    pub workspace_id: WorkspaceId,
    /// Session to move.
    pub session_id: SessionId,
    /// Optional insertion anchor; absence appends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_session_id: Option<SessionId>,
}

impl WorkspaceInsertSessionBeforeRequest {
    /// Parses a `workspace.insertSessionBefore` request.
    ///
    /// # Errors
    ///
    /// Returns an error for empty or malformed Workspace/Session identities.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "workspaceId", "$.workspaceId")?;
        require_nonempty_string(object, "sessionId", "$.sessionId")?;
        optional_nonempty_string(object, "beforeSessionId", "$.beforeSessionId")?;
        decode(value)
    }
}

/// `workspace.archiveSession` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceArchiveSessionRequest {
    /// Session to archive.
    pub session_id: SessionId,
}

impl WorkspaceArchiveSessionRequest {
    /// Parses a `workspace.archiveSession` request.
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

/// `workspace.archiveSession` response with the full updated archive set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceArchiveSessionValue {
    /// Complete archived Session identity set.
    pub archived_session_ids: Vec<SessionId>,
}

impl WorkspaceArchiveSessionValue {
    /// Parses the complete archive set.
    ///
    /// # Errors
    ///
    /// Returns an error for any empty or non-string Session id.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            archived_session_ids: parse_session_ids(
                require_array(object, "archivedSessionIds", "$.archivedSessionIds")?,
                "$.archivedSessionIds",
            )?,
        })
    }
}

fn parse_session_ids(values: &[Value], path: &str) -> Result<Vec<SessionId>, ContractError> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .filter(|id| !id.is_empty())
                .map(SessionId::new)
                .ok_or_else(|| {
                    ContractError::new(format!("{path}[{index}]"), "expected non-empty string")
                })
        })
        .collect()
}
