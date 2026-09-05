//! Session-domain wire contracts and method schemas.

use std::collections::BTreeMap;

use seekdeep_attachment::{AttachmentId, ImageAttachmentRef, ImageMediaType};
use seekdeep_core::session::SessionId;
use seekdeep_llm::MessageId;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

use super::{rpc::ContractError, workspace::WorkspaceId};

/// Maximum number of sessions returned by one sidebar search.
pub const SESSION_SEARCH_RESULT_LIMIT: usize = 20;
/// Maximum snippet length in Unicode code points.
pub const SESSION_SEARCH_SNIPPET_MAX_CODE_POINTS: usize = 240;
/// Maximum query length under the source string schema's UTF-16 semantics.
pub const SESSION_SEARCH_QUERY_MAX_CODE_UNITS: usize = 500;

/// Returns the longest prefix containing at most `maximum` Unicode code points.
#[must_use]
pub fn truncate_unicode_code_points(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

/// Merge-extensible Session event envelope.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEvent {
    /// Open event type discriminant.
    #[serde(rename = "type")]
    pub kind: String,
    /// Non-negative integer event sequence.
    pub seq: u64,
    /// Event timestamp.
    pub time: f64,
    /// Event-specific data retained without interpretation.
    pub data: Value,
    /// Optional source-event provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_event_seqs: Option<Vec<f64>>,
    /// Optional surface operation retained without interpretation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_op: Option<Value>,
    /// Present only as literal `true` when an unknown consumer may ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignorable: Option<bool>,
}

impl SessionEvent {
    /// Parses and normalizes the strict Session event envelope.
    ///
    /// # Errors
    ///
    /// Returns an error if required envelope members are missing or malformed.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_string(object, "type", "$.type", false)?;
        require_nonnegative_integer(object, "seq", "$.seq")?;
        require_number(object, "time", "$.time")?;
        require_field(object, "data", "$.data")?;
        optional_number_array(object, "sourceEventSeqs", "$.sourceEventSeqs")?;
        if let Some(ignorable) = object.get("ignorable")
            && ignorable != &Value::Bool(true)
        {
            return Err(ContractError::new("$.ignorable", "expected literal true"));
        }
        decode(value)
    }
}

/// Projection baseline transported with a list row or history tail.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionProjectionsBlock {
    /// Last committed event represented by every value; `-1` means empty log.
    pub as_of_seq: i64,
    /// Whole current value per registered projection key.
    pub values: BTreeMap<String, Value>,
}

impl SessionProjectionsBlock {
    fn validate(&self) -> Result<(), ContractError> {
        if self.as_of_seq < -1 {
            return Err(ContractError::new(
                "$.asOfSeq",
                "expected integer at least -1",
            ));
        }
        Ok(())
    }
}

/// One Session list entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    /// Session identity.
    pub session_id: SessionId,
    /// Creation time or latest human-authored prompt time.
    pub updated_at: f64,
    /// Whether its attached agent is running.
    pub running: bool,
    /// Whether no turn has begun.
    pub blank: bool,
    /// Fork/spawn lineage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<SessionId>,
    /// Coarse durable origin; the only current wire literal is `subagent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<SessionSummaryOrigin>,
    /// Recorded working directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Persisted agent composition preset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_preset: Option<String>,
    /// Optional projection baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projections: Option<SessionProjectionsBlock>,
}

/// Closed coarse origin vocabulary for a Session summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionSummaryOrigin {
    /// Session was created as a delegated child.
    Subagent,
}

impl SessionSummary {
    /// Parses and normalizes a `session.list` summary row.
    ///
    /// # Errors
    ///
    /// Returns an error when any required or optional member violates its schema.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "sessionId", "$.sessionId")?;
        require_number(object, "updatedAt", "$.updatedAt")?;
        require_bool(object, "running", "$.running")?;
        require_bool(object, "blank", "$.blank")?;
        optional_nonempty_string(object, "parentSessionId", "$.parentSessionId")?;
        optional_literal(object, "origin", "subagent", "$.origin")?;
        optional_string(object, "cwd", "$.cwd", false)?;
        optional_string(object, "agentPreset", "$.agentPreset", false)?;
        optional_object(object, "projections", "$.projections")?;
        let parsed: Self = decode(value)?;
        if let Some(projections) = &parsed.projections {
            projections.validate()?;
        }
        Ok(parsed)
    }
}

/// `session.list` request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionListRequest {
    /// Reserved cursor seat; unimplemented in version one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

impl SessionListRequest {
    /// Parses and normalizes a `session.list` request.
    ///
    /// # Errors
    ///
    /// Returns an error unless the value is an object with an optional string cursor.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        optional_string(object, "cursor", "$.cursor", false)?;
        decode(value)
    }
}

/// `session.list` response value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionListValue {
    /// Summary rows.
    pub items: Vec<SessionSummary>,
}

impl SessionListValue {
    /// Parses and normalizes a `session.list` response value.
    ///
    /// # Errors
    ///
    /// Returns an error when `items` is absent or any row is malformed.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let items = require_array(object, "items", "$.items")?;
        let mut parsed = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            parsed.push(
                SessionSummary::parse(item)
                    .map_err(|error| prefix_error(&error, &format!("$.items[{index}]")))?,
            );
        }
        Ok(Self { items: parsed })
    }
}

/// `session.search` request after source-compatible trim normalization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSearchRequest {
    /// Non-empty normalized query.
    pub query: String,
}

impl SessionSearchRequest {
    /// Parses, trims, and validates a `session.search` request.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing/non-string, blank, overlong, or NUL-bearing query.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let raw = require_string(object, "query", "$.query", false)?;
        let query = raw.trim_matches(is_ecmascript_whitespace).to_owned();
        if query.is_empty() {
            return Err(ContractError::new("$.query", "expected non-empty string"));
        }
        if query.encode_utf16().count() > SESSION_SEARCH_QUERY_MAX_CODE_UNITS {
            return Err(ContractError::new("$.query", "string is too long"));
        }
        if query.contains('\0') {
            return Err(ContractError::new(
                "$.query",
                "search query must not contain NUL",
            ));
        }
        Ok(Self { query })
    }
}

/// One session-content search result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSearchItem {
    /// Matching Session identity.
    pub session_id: SessionId,
    /// Plain-text excerpt around the strongest match.
    pub snippet: String,
}

impl SessionSearchItem {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "sessionId", "$.sessionId")?;
        let snippet = require_string(object, "snippet", "$.snippet", false)?;
        if snippet.chars().count() > SESSION_SEARCH_SNIPPET_MAX_CODE_POINTS {
            return Err(ContractError::new(
                "$.snippet",
                format!(
                    "search snippet must contain at most {SESSION_SEARCH_SNIPPET_MAX_CODE_POINTS} Unicode code points"
                ),
            ));
        }
        decode(value)
    }
}

/// `session.search` response value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSearchValue {
    /// At most 20 matching sessions.
    pub items: Vec<SessionSearchItem>,
    /// Whether the user should refine the query.
    pub has_more: bool,
}

/// `session.create` request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCreateRequest {
    /// Existing Workspace to attach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<WorkspaceId>,
    /// Explicit working directory, mutually exclusive with `workspace_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Optional caller-preallocated Session identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Optional requested agent preset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_preset: Option<String>,
}

impl SessionCreateRequest {
    /// Parses a `session.create` request and enforces the project-source XOR.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed fields or simultaneous `workspaceId` and `cwd`.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        optional_nonempty_string(object, "workspaceId", "$.workspaceId")?;
        optional_string(object, "cwd", "$.cwd", false)?;
        optional_nonempty_string(object, "sessionId", "$.sessionId")?;
        optional_string(object, "agentPreset", "$.agentPreset", false)?;
        if object.contains_key("workspaceId") && object.contains_key("cwd") {
            return Err(ContractError::new(
                "$",
                "session.create accepts workspaceId or cwd, not both",
            ));
        }
        decode(value)
    }
}

/// `session.create` response value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCreateValue {
    /// Created or idempotently recovered Session identity.
    pub session_id: SessionId,
    /// Resolved agent preset when the deployment composes presets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_preset: Option<String>,
}

impl SessionCreateValue {
    /// Parses a `session.create` response value.
    ///
    /// # Errors
    ///
    /// Returns an error unless `sessionId` is non-empty and `agentPreset`, when present, is a string.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "sessionId", "$.sessionId")?;
        optional_string(object, "agentPreset", "$.agentPreset", false)?;
        decode(value)
    }
}

/// `session.rename` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRenameRequest {
    /// Session to rename.
    pub session_id: SessionId,
    /// Raw title; Host normalization decides whether it is acceptable.
    pub title: String,
}

impl SessionRenameRequest {
    /// Parses a `session.rename` request.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty Session id or non-string title.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "sessionId", "$.sessionId")?;
        require_string(object, "title", "$.title", false)?;
        decode(value)
    }
}

/// `session.rename` response value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRenameValue {
    /// Host-normalized non-empty title.
    pub title: String,
    /// Appended title-event sequence.
    pub seq: u64,
}

impl SessionRenameValue {
    /// Parses a `session.rename` response value.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty normalized title or invalid sequence.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "title", "$.title")?;
        require_nonnegative_integer(object, "seq", "$.seq")?;
        decode(value)
    }
}

/// `session.fork` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionForkRequest {
    /// Source Session.
    pub session_id: SessionId,
    /// Optional event-sequence anchor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at_seq: Option<u64>,
}

impl SessionForkRequest {
    /// Parses a `session.fork` request.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed identity or sequence fields.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "sessionId", "$.sessionId")?;
        optional_nonnegative_integer(object, "atSeq", "$.atSeq")?;
        decode(value)
    }
}

/// `session.fork` response value.
pub type SessionForkValue = SessionIdValue;

/// Common response carrying one Session id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIdValue {
    /// Session identity.
    pub session_id: SessionId,
}

impl SessionIdValue {
    /// Parses an object containing a non-empty `sessionId`.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing, non-string, or empty identity.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "sessionId", "$.sessionId")?;
        decode(value)
    }
}

/// `session.history` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHistoryRequest {
    /// Session to inspect.
    pub session_id: SessionId,
    /// Exclusive backwards-page cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_seq: Option<u64>,
    /// Positive message-boundary page size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_messages: Option<u64>,
}

impl SessionHistoryRequest {
    /// Parses a `session.history` request.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed ids, negative/fractional cursors, or a non-positive page size.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "sessionId", "$.sessionId")?;
        optional_nonnegative_integer(object, "beforeSeq", "$.beforeSeq")?;
        optional_positive_integer(object, "maxMessages", "$.maxMessages")?;
        decode(value)
    }
}

/// Complete model selection for one Session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelection {
    /// Registered provider route.
    pub provider: String,
    /// Provider-owned model id.
    pub model: String,
    /// Optional adapter-owned reasoning effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

impl ModelSelection {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "provider", "$.provider")?;
        require_nonempty_string(object, "model", "$.model")?;
        optional_nonempty_string(object, "reasoningEffort", "$.reasoningEffort")?;
        decode(value)
    }
}

/// One adapter-owned reasoning effort displayed for an exact model route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelReasoningEffort {
    /// Opaque submitted value.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Optional display description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl ModelReasoningEffort {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "id", "$.id")?;
        require_nonempty_string(object, "name", "$.name")?;
        optional_string(object, "description", "$.description", false)?;
        decode(value)
    }
}

/// Selectable reasoning metadata for one exact model route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelReasoning {
    /// Efforts in adapter-preferred order; never empty.
    pub efforts: Vec<ModelReasoningEffort>,
    /// Optional configured default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_effort: Option<String>,
}

impl ModelReasoning {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let efforts = require_array(object, "efforts", "$.efforts")?;
        if efforts.is_empty() {
            return Err(ContractError::new("$.efforts", "expected non-empty array"));
        }
        let efforts = parse_array(efforts, ModelReasoningEffort::parse, "$.efforts")?;
        optional_nonempty_string(object, "defaultEffort", "$.defaultEffort")?;
        Ok(Self {
            efforts,
            default_effort: optional_owned_string(object, "defaultEffort"),
        })
    }
}

/// One model displayed inside a provider group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalogModel {
    /// Provider-owned model id.
    pub id: String,
    /// Provider display name.
    pub name: String,
    /// Optional description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Exact-route reasoning metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ModelReasoning>,
}

impl ModelCatalogModel {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "id", "$.id")?;
        require_nonempty_string(object, "name", "$.name")?;
        optional_string(object, "description", "$.description", false)?;
        let reasoning = object
            .get("reasoning")
            .map(ModelReasoning::parse)
            .transpose()?;
        Ok(Self {
            id: require_string(object, "id", "$.id", true)?.to_owned(),
            name: require_string(object, "name", "$.name", true)?.to_owned(),
            description: optional_owned_string(object, "description"),
            reasoning,
        })
    }
}

/// One provider and its successfully advertised models.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelProviderGroup {
    /// Provider route id.
    pub id: String,
    /// Provider display name.
    pub name: String,
    /// Provider-preferred model order.
    pub models: Vec<ModelCatalogModel>,
}

impl ModelProviderGroup {
    /// Parses one successfully loaded provider group.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed provider identity or model rows.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let id = require_nonempty_string(object, "id", "$.id")?.to_owned();
        let name = require_nonempty_string(object, "name", "$.name")?.to_owned();
        let models = parse_array(
            require_array(object, "models", "$.models")?,
            ModelCatalogModel::parse,
            "$.models",
        )?;
        Ok(Self { id, name, models })
    }
}

/// One provider-local model-catalog failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalogFailure {
    /// Provider route id.
    pub id: String,
    /// Provider display name.
    pub name: String,
    /// Diagnostic message, which may be empty.
    pub message: String,
}

impl ModelCatalogFailure {
    /// Parses one provider-local model-catalog failure.
    ///
    /// # Errors
    ///
    /// Returns an error for empty provider identity/name or a non-string message.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "id", "$.id")?;
        require_nonempty_string(object, "name", "$.name")?;
        require_string(object, "message", "$.message", false)?;
        decode(value)
    }
}

/// Detached model-directory snapshot for one Session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionModelsValue {
    /// Model selection for the next assembled step.
    pub current: ModelSelection,
    /// Whether an adapter currently serves the selected route.
    pub routable: bool,
    /// Successfully loaded provider groups.
    pub groups: Vec<ModelProviderGroup>,
    /// Provider-local failures.
    pub failures: Vec<ModelCatalogFailure>,
}

impl SessionModelsValue {
    /// Parses a `session.models` response value.
    ///
    /// # Errors
    ///
    /// Returns an error when any directory member violates the wire contract.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            current: ModelSelection::parse(require_field(object, "current", "$.current")?)?,
            routable: require_bool(object, "routable", "$.routable")?,
            groups: parse_array(
                require_array(object, "groups", "$.groups")?,
                ModelProviderGroup::parse,
                "$.groups",
            )?,
            failures: parse_array(
                require_array(object, "failures", "$.failures")?,
                ModelCatalogFailure::parse,
                "$.failures",
            )?,
        })
    }
}

/// Request carrying a Session id for `session.models` or `session.cancel`.
pub type SessionModelsRequest = SessionIdValue;
/// `session.cancel` request.
pub type SessionCancelRequest = SessionIdValue;

/// `session.selectModel` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSelectModelRequest {
    /// Session to update.
    pub session_id: SessionId,
    /// Provider route.
    pub provider: String,
    /// Model id.
    pub model: String,
    /// Optional reasoning effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

impl SessionSelectModelRequest {
    /// Parses a `session.selectModel` request.
    ///
    /// # Errors
    ///
    /// Returns an error when any required id is empty or the optional effort is empty/non-string.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "sessionId", "$.sessionId")?;
        require_nonempty_string(object, "provider", "$.provider")?;
        require_nonempty_string(object, "model", "$.model")?;
        optional_nonempty_string(object, "reasoningEffort", "$.reasoningEffort")?;
        decode(value)
    }
}

/// `session.selectModel` response value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSelectModelValue {
    /// Complete accepted selection.
    pub selected: ModelSelection,
}

impl SessionSelectModelValue {
    /// Parses a `session.selectModel` response value.
    ///
    /// # Errors
    ///
    /// Returns an error when the selected model tuple is malformed.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            selected: ModelSelection::parse(require_field(object, "selected", "$.selected")?)?,
        })
    }
}

/// Host-computed rendering intent attached to one history event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolEventView {
    /// Whether this view presents a call or result.
    #[serde(rename = "for")]
    pub target: ToolEventViewTarget,
    /// Loose, host-owned view object whose `card` tag is mandatory.
    pub view: Map<String, Value>,
}

/// Closed tool-event view target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolEventViewTarget {
    /// Tool call.
    Call,
    /// Tool result.
    Result,
}

impl ToolEventView {
    pub(super) fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let target = match require_string(object, "for", "$.for", false)? {
            "call" => ToolEventViewTarget::Call,
            "result" => ToolEventViewTarget::Result,
            _ => {
                return Err(ContractError::new(
                    "$.for",
                    "unknown tool-event view target",
                ));
            }
        };
        let view = require_object(require_field(object, "view", "$.view")?, "$.view")?;
        require_string(view, "card", "$.view.card", false)?;
        Ok(Self {
            target,
            view: view.clone(),
        })
    }
}

/// One `session.history` entry.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// Raw merge-extensible Session event.
    pub event: SessionEvent,
    /// Optional Host-computed render intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<ToolEventView>,
}

impl HistoryEntry {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            event: SessionEvent::parse(require_field(object, "event", "$.event")?)?,
            view: object.get("view").map(ToolEventView::parse).transpose()?,
        })
    }
}

/// `session.history` response value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionHistoryValue {
    /// Whole-event history entries.
    pub events: Vec<HistoryEntry>,
    /// Whether an older page exists.
    pub has_more: bool,
    /// Projection baseline present only on the tail page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projections: Option<SessionProjectionsBlock>,
}

impl SessionHistoryValue {
    /// Parses a `session.history` response value.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed events, views, paging state, or projections.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let projections = object
            .get("projections")
            .map(|value| {
                let parsed: SessionProjectionsBlock = decode(value)?;
                parsed.validate()?;
                Ok(parsed)
            })
            .transpose()?;
        Ok(Self {
            events: parse_array(
                require_array(object, "events", "$.events")?,
                HistoryEntry::parse,
                "$.events",
            )?,
            has_more: require_bool(object, "hasMore", "$.hasMore")?,
            projections,
        })
    }
}

/// Persisted Session-list projection value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionListMetadata {
    /// Whether the checkpoint prefix contains no turn start.
    pub blank: bool,
    /// Latest human-authored prompt time; null means absent.
    pub last_prompt_at: Option<f64>,
}

impl SessionListMetadata {
    /// Parses the Host-side Session-list projection schema.
    ///
    /// # Errors
    ///
    /// Returns an error unless both `blank` and nullable `lastPromptAt` are present and typed.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_bool(object, "blank", "$.blank")?;
        let last = require_field(object, "lastPromptAt", "$.lastPromptAt")?;
        if !last.is_null() && !last.is_number() {
            return Err(ContractError::new(
                "$.lastPromptAt",
                "expected number or null",
            ));
        }
        decode(value)
    }
}

/// Image-intake limits projection value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageLimitsProjection {
    /// Per-image encoded byte limit.
    pub max_image_bytes: u64,
    /// Per-message image count limit.
    pub max_images_per_message: u64,
    /// Per-message aggregate image-byte limit.
    pub max_message_image_bytes: u64,
    /// Per-image decoded pixel limit.
    pub max_image_pixels: u64,
    /// Host-advertised media type strings.
    pub media_types: Vec<String>,
}

impl ImageLimitsProjection {
    /// Parses the Host-side image-limits projection schema.
    ///
    /// # Errors
    ///
    /// Returns an error unless every numeric limit is a positive integer and media types are strings.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        for name in [
            "maxImageBytes",
            "maxImagesPerMessage",
            "maxMessageImageBytes",
            "maxImagePixels",
        ] {
            require_positive_integer(object, name, &format!("$.{name}"))?;
        }
        let media_types = require_array(object, "mediaTypes", "$.mediaTypes")?;
        if !media_types.iter().all(Value::is_string) {
            return Err(ContractError::new("$.mediaTypes", "expected string array"));
        }
        decode(value)
    }
}

/// Browser-submitted prompt content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PromptContentPart {
    /// Plain text.
    #[serde(rename = "text")]
    Text {
        /// Exact text.
        text: String,
    },
    /// Temporary raster bytes.
    #[serde(rename = "image", rename_all = "camelCase")]
    Image {
        /// Closed version-one media type.
        media_type: ImageMediaType,
        /// Encoded bytes as a string carrier.
        data: String,
        /// Optional display name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

impl PromptContentPart {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        match require_string(object, "type", "$.type", false)? {
            "text" => {
                require_string(object, "text", "$.text", false)?;
            }
            "image" => {
                require_image_media_type(object, "mediaType", "$.mediaType")?;
                require_string(object, "data", "$.data", false)?;
                optional_string(object, "name", "$.name", false)?;
            }
            _ => return Err(ContractError::new("$.type", "unknown prompt content type")),
        }
        decode(value)
    }
}

/// Closed prompt dispatch mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptMode {
    /// Enqueue normally.
    Queue,
    /// Steer the active turn.
    Steer,
}

/// `session.prompt` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPromptRequest {
    /// Target Session.
    pub session_id: SessionId,
    /// Queue or steer behavior.
    pub mode: PromptMode,
    /// Narrow browser wire content.
    pub content: Vec<PromptContentPart>,
    /// Optional browser-local IANA time-zone candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_time_zone: Option<String>,
}

impl SessionPromptRequest {
    /// Parses a `session.prompt` request.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed ids, mode, content, or time-zone provenance.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "sessionId", "$.sessionId")?;
        match require_string(object, "mode", "$.mode", false)? {
            "queue" | "steer" => {}
            _ => return Err(ContractError::new("$.mode", "unknown prompt mode")),
        }
        let content = parse_array(
            require_array(object, "content", "$.content")?,
            PromptContentPart::parse,
            "$.content",
        )?;
        optional_string(object, "clientTimeZone", "$.clientTimeZone", false)?;
        Ok(Self {
            session_id: SessionId::new(require_string(object, "sessionId", "$.sessionId", true)?),
            mode: match object.get("mode").and_then(Value::as_str) {
                Some("queue") => PromptMode::Queue,
                Some("steer") => PromptMode::Steer,
                _ => unreachable!("mode was validated"),
            },
            content,
            client_time_zone: optional_owned_string(object, "clientTimeZone"),
        })
    }
}

/// Successful slash-command metadata returned by `session.prompt`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptCommandSuccess {
    /// Must be `success`.
    pub kind: PromptCommandKind,
    /// Optional command-produced text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Closed prompt-command result discriminant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptCommandKind {
    /// Command completed successfully.
    Success,
}

/// `session.prompt` response value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPromptValue {
    /// Literal successful admission marker.
    pub accepted: bool,
    /// Present only for a dispatched slash command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<PromptCommandSuccess>,
}

impl SessionPromptValue {
    /// Parses a `session.prompt` response value.
    ///
    /// # Errors
    ///
    /// Returns an error unless admission is literal true and the optional command is successful.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_literal_true(object, "accepted", "$.accepted")?;
        if let Some(command) = object.get("command") {
            let command = require_object(command, "$.command")?;
            if require_string(command, "kind", "$.command.kind", false)? != "success" {
                return Err(ContractError::new("$.command.kind", "expected success"));
            }
            optional_string(command, "text", "$.command.text", false)?;
        }
        decode(value)
    }
}

/// `session.attachment` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAttachmentRequest {
    /// Session whose log must reference the object.
    pub session_id: SessionId,
    /// Durable attachment identity.
    pub attachment_id: AttachmentId,
}

impl SessionAttachmentRequest {
    /// Parses a `session.attachment` request.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty Session or attachment id.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_nonempty_string(object, "sessionId", "$.sessionId")?;
        require_nonempty_string(object, "attachmentId", "$.attachmentId")?;
        decode(value)
    }
}

/// `session.attachment` response value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAttachmentValue {
    /// Durable verified image reference.
    pub attachment: ImageAttachmentRef,
    /// Encoded bytes as a string carrier.
    pub data: String,
}

impl SessionAttachmentValue {
    /// Parses a `session.attachment` response value.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed image metadata or non-string data.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        Ok(Self {
            attachment: validate_image_attachment_ref(require_field(
                object,
                "attachment",
                "$.attachment",
            )?)?,
            data: require_string(object, "data", "$.data", false)?.to_owned(),
        })
    }
}

/// A validated loose core content block, including unknown plugin fields.
pub type WireContentBlock = Map<String, Value>;

/// One mutation of a pending queue item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum QueueAction {
    /// Replace content.
    Edit {
        /// Merge-extensible core blocks.
        content: Vec<WireContentBlock>,
    },
    /// Remove the item.
    Remove,
    /// Strictly steer the item.
    Steer,
}

impl QueueAction {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        match require_string(object, "kind", "$.kind", false)? {
            "edit" => {
                let content = require_array(object, "content", "$.content")?;
                let mut parsed = Vec::with_capacity(content.len());
                for (index, block) in content.iter().enumerate() {
                    parsed.push(
                        validate_content_block(block).map_err(|error| {
                            prefix_error(&error, &format!("$.content[{index}]"))
                        })?,
                    );
                }
                Ok(Self::Edit { content: parsed })
            }
            "remove" => Ok(Self::Remove),
            "steer" => Ok(Self::Steer),
            _ => Err(ContractError::new("$.kind", "unknown queue action")),
        }
    }
}

/// `session.updateQueue` request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdateQueueRequest {
    /// Target Session.
    pub session_id: SessionId,
    /// Stable queued message identity.
    pub item_id: MessageId,
    /// Requested mutation.
    pub action: QueueAction,
}

impl SessionUpdateQueueRequest {
    /// Parses a `session.updateQueue` request.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed ids, action tags, or edited content.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let session_id = require_nonempty_string(object, "sessionId", "$.sessionId")?;
        let item_id = require_nonempty_string(object, "itemId", "$.itemId")?;
        Ok(Self {
            session_id: SessionId::new(session_id),
            item_id: MessageId::new(item_id),
            action: QueueAction::parse(require_field(object, "action", "$.action")?)?,
        })
    }
}

/// Response value shared by prompt-free accepted mutations and cancellation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedValue {
    /// Must be literal true.
    pub accepted: bool,
}

impl AcceptedValue {
    /// Parses an object containing literal `accepted: true`.
    ///
    /// # Errors
    ///
    /// Returns an error when the marker is missing, non-boolean, or false.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        require_literal_true(object, "accepted", "$.accepted")?;
        Ok(Self { accepted: true })
    }
}

/// `session.updateQueue` response value.
pub type SessionUpdateQueueValue = AcceptedValue;
/// `session.cancel` response value.
pub type SessionCancelValue = AcceptedValue;

impl SessionSearchValue {
    /// Parses and normalizes a `session.search` response value.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed items, more than 20 rows, or a missing boolean marker.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let items = require_array(object, "items", "$.items")?;
        if items.len() > SESSION_SEARCH_RESULT_LIMIT {
            return Err(ContractError::new("$.items", "array is too long"));
        }
        let mut parsed = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            parsed.push(
                SessionSearchItem::parse(item)
                    .map_err(|error| prefix_error(&error, &format!("$.items[{index}]")))?,
            );
        }
        Ok(Self {
            items: parsed,
            has_more: require_bool(object, "hasMore", "$.hasMore")?,
        })
    }
}

pub(super) fn decode<T: DeserializeOwned>(value: &Value) -> Result<T, ContractError> {
    serde_json::from_value(value.clone())
        .map_err(|error| ContractError::new("$", error.to_string()))
}

pub(super) fn require_object<'a>(
    value: &'a Value,
    path: &str,
) -> Result<&'a Map<String, Value>, ContractError> {
    value
        .as_object()
        .ok_or_else(|| ContractError::new(path, "expected object"))
}

pub(super) fn require_field<'a>(
    object: &'a Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<&'a Value, ContractError> {
    object
        .get(name)
        .ok_or_else(|| ContractError::new(path, "required property is missing"))
}

pub(super) fn require_string<'a>(
    object: &'a Map<String, Value>,
    name: &str,
    path: &str,
    nonempty: bool,
) -> Result<&'a str, ContractError> {
    let string = require_field(object, name, path)?
        .as_str()
        .ok_or_else(|| ContractError::new(path, "expected string"))?;
    if nonempty && string.is_empty() {
        return Err(ContractError::new(path, "expected non-empty string"));
    }
    Ok(string)
}

pub(super) fn require_nonempty_string<'a>(
    object: &'a Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<&'a str, ContractError> {
    require_string(object, name, path, true)
}

pub(super) fn require_bool(
    object: &Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<bool, ContractError> {
    require_field(object, name, path)?
        .as_bool()
        .ok_or_else(|| ContractError::new(path, "expected boolean"))
}

pub(super) fn require_number(
    object: &Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<f64, ContractError> {
    require_field(object, name, path)?
        .as_f64()
        .ok_or_else(|| ContractError::new(path, "expected number"))
}

pub(super) fn require_nonnegative_integer(
    object: &Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<u64, ContractError> {
    require_field(object, name, path)?
        .as_u64()
        .ok_or_else(|| ContractError::new(path, "expected non-negative integer"))
}

pub(super) fn require_array<'a>(
    object: &'a Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<&'a Vec<Value>, ContractError> {
    require_field(object, name, path)?
        .as_array()
        .ok_or_else(|| ContractError::new(path, "expected array"))
}

pub(super) fn require_positive_integer(
    object: &Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<u64, ContractError> {
    let value = require_nonnegative_integer(object, name, path)?;
    if value == 0 {
        return Err(ContractError::new(path, "expected positive integer"));
    }
    Ok(value)
}

pub(super) fn optional_nonnegative_integer(
    object: &Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<(), ContractError> {
    if object.contains_key(name) {
        require_nonnegative_integer(object, name, path)?;
    }
    Ok(())
}

pub(super) fn optional_positive_integer(
    object: &Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<(), ContractError> {
    if object.contains_key(name) {
        require_positive_integer(object, name, path)?;
    }
    Ok(())
}

pub(super) fn require_literal_true(
    object: &Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<(), ContractError> {
    if require_field(object, name, path)? != &Value::Bool(true) {
        return Err(ContractError::new(path, "expected literal true"));
    }
    Ok(())
}

pub(super) fn optional_string(
    object: &Map<String, Value>,
    name: &str,
    path: &str,
    nonempty: bool,
) -> Result<(), ContractError> {
    if object.contains_key(name) {
        require_string(object, name, path, nonempty)?;
    }
    Ok(())
}

pub(super) fn optional_nonempty_string(
    object: &Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<(), ContractError> {
    optional_string(object, name, path, true)
}

pub(super) fn optional_literal(
    object: &Map<String, Value>,
    name: &str,
    literal: &str,
    path: &str,
) -> Result<(), ContractError> {
    if let Some(value) = object.get(name)
        && value.as_str() != Some(literal)
    {
        return Err(ContractError::new(path, format!("expected {literal}")));
    }
    Ok(())
}

pub(super) fn optional_object(
    object: &Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<(), ContractError> {
    if let Some(value) = object.get(name) {
        require_object(value, path)?;
    }
    Ok(())
}

pub(super) fn optional_number_array(
    object: &Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<(), ContractError> {
    if let Some(value) = object.get(name) {
        let values = value
            .as_array()
            .ok_or_else(|| ContractError::new(path, "expected array"))?;
        if !values.iter().all(Value::is_number) {
            return Err(ContractError::new(path, "expected number array"));
        }
    }
    Ok(())
}

pub(super) fn optional_owned_string(object: &Map<String, Value>, name: &str) -> Option<String> {
    object
        .get(name)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

pub(super) fn parse_array<T>(
    values: &[Value],
    parse: impl Fn(&Value) -> Result<T, ContractError>,
    path: &str,
) -> Result<Vec<T>, ContractError> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            parse(value).map_err(|error| prefix_error(&error, &format!("{path}[{index}]")))
        })
        .collect()
}

fn require_image_media_type(
    object: &Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<ImageMediaType, ContractError> {
    match require_string(object, name, path, false)? {
        "image/png" => Ok(ImageMediaType::Png),
        "image/jpeg" => Ok(ImageMediaType::Jpeg),
        "image/webp" => Ok(ImageMediaType::Webp),
        "image/gif" => Ok(ImageMediaType::Gif),
        _ => Err(ContractError::new(path, "unknown image media type")),
    }
}

fn validate_image_attachment_ref(value: &Value) -> Result<ImageAttachmentRef, ContractError> {
    let object = require_object(value, "$")?;
    let attachment_id = require_nonempty_string(object, "attachmentId", "$.attachmentId")?;
    let media_type = require_image_media_type(object, "mediaType", "$.mediaType")?;
    let bytes = require_positive_integer(object, "bytes", "$.bytes")?;
    let width = require_positive_integer(object, "width", "$.width")?;
    let height = require_positive_integer(object, "height", "$.height")?;
    optional_string(object, "name", "$.name", false)?;
    Ok(ImageAttachmentRef {
        attachment_id: AttachmentId::new(attachment_id),
        media_type,
        bytes,
        width,
        height,
        name: optional_owned_string(object, "name"),
    })
}

pub(super) fn validate_content_block(value: &Value) -> Result<WireContentBlock, ContractError> {
    let object = require_object(value, "$")?;
    require_string(object, "type", "$.type", false)?;
    Ok(object.clone())
}

pub(super) fn prefix_error(error: &ContractError, prefix: &str) -> ContractError {
    let suffix = error.path().strip_prefix('$').unwrap_or(error.path());
    ContractError::new(format!("{prefix}{suffix}"), error.message())
}

pub(crate) fn is_ecmascript_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}
