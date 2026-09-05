//! Logical mux and Host stream frame contracts.

use seekdeep_client_connection::{RpcError, RpcId};
use seekdeep_core::session::SessionId;
use seekdeep_llm::{CallId, MessageId};
use seekdeep_user_approval::{ApprovalOutcome, ApprovalRequestId};
use seekdeep_user_questions::AskUserQuestionItem;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{
    jobs::JobView,
    questions::parse_question_item,
    rpc::{ContractError, parse_rpc_error},
    sessions::{
        SessionEvent, ToolEventView, optional_literal, optional_nonempty_string, optional_string,
        parse_array, require_array, require_bool, require_field, require_nonempty_string,
        require_nonnegative_integer, require_object, require_string, validate_content_block,
    },
    workspace::{WorkspaceId, WorkspaceView},
};

/// Closed message roles admitted by transient queue snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireMessageRole {
    /// System message.
    System,
    /// User message.
    User,
    /// Assistant message.
    Assistant,
}

/// Unified transient message envelope in a queued-inbox snapshot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireMessage {
    /// Stable non-empty message identity.
    pub id: MessageId,
    /// Closed role.
    pub role: WireMessageRole,
    /// Merge-extensible content blocks.
    pub content: Vec<Map<String, Value>>,
    /// Loose source object whose `kind` tag is mandatory.
    pub source: Map<String, Value>,
}

impl WireMessage {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let id = MessageId::new(require_nonempty_string(object, "id", "$.id")?);
        let role = match require_string(object, "role", "$.role", false)? {
            "system" => WireMessageRole::System,
            "user" => WireMessageRole::User,
            "assistant" => WireMessageRole::Assistant,
            _ => return Err(ContractError::new("$.role", "unknown message role")),
        };
        let content = parse_array(
            require_array(object, "content", "$.content")?,
            validate_content_block,
            "$.content",
        )?;
        let source = require_object(require_field(object, "source", "$.source")?, "$.source")?;
        require_string(source, "kind", "$.source.kind", false)?;
        Ok(Self {
            id,
            role,
            content,
            source: source.clone(),
        })
    }
}

/// Agent-resolved pending inbox placement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QueuePlacement {
    /// Normal queued work.
    Queued,
    /// Pending steering work.
    Steering,
    /// Invisible context work until claimed.
    Context,
}

/// One pending inbox occurrence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueuedInboxItem {
    /// Stable occurrence identity.
    pub id: MessageId,
    /// Resolved placement.
    pub placement: QueuePlacement,
    /// Complete pending message.
    pub message: WireMessage,
}

impl QueuedInboxItem {
    fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let placement = match require_string(object, "placement", "$.placement", false)? {
            "queued" => QueuePlacement::Queued,
            "steering" => QueuePlacement::Steering,
            "context" => QueuePlacement::Context,
            _ => return Err(ContractError::new("$.placement", "unknown queue placement")),
        };
        Ok(Self {
            id: MessageId::new(require_nonempty_string(object, "id", "$.id")?),
            placement,
            message: WireMessage::parse(require_field(object, "message", "$.message")?)?,
        })
    }
}

/// Closed question-resolution outcomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QuestionResolutionOutcome {
    /// User answered.
    Answered,
    /// Interaction was cancelled.
    Cancelled,
}

/// Mux-stream payload union.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MuxFrame {
    /// Raw Session event and optional Host-computed render intent.
    #[serde(rename = "session/event", rename_all = "camelCase")]
    SessionEvent {
        /// Owning Session.
        session_id: SessionId,
        /// Raw merge-extensible event.
        event: SessionEvent,
        /// Optional render intent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        view: Option<ToolEventView>,
    },
    /// Initial last-sequence baseline for an attached Session.
    #[serde(rename = "session/subscribed", rename_all = "camelCase")]
    SessionSubscribed {
        /// Owning Session.
        session_id: SessionId,
        /// Last sequence; `-1` denotes an empty log.
        last_seq: i64,
    },
    /// Answerable approval request.
    #[serde(rename = "approval/requested", rename_all = "camelCase")]
    ApprovalRequested {
        /// Owning Session.
        session_id: SessionId,
        /// Durable approval audit id.
        approval_id: ApprovalRequestId,
        /// Tool name, which may be empty at this schema layer.
        tool_name: String,
        /// Optional model call id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<CallId>,
        /// Optional human reason.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Final approval outcome.
    #[serde(rename = "approval/resolved", rename_all = "camelCase")]
    ApprovalResolved {
        /// Owning Session.
        session_id: SessionId,
        /// Durable approval audit id.
        approval_id: ApprovalRequestId,
        /// Closed Host outcome.
        outcome: ApprovalOutcome,
    },
    /// Non-empty answerable question batch.
    #[serde(rename = "question/requested", rename_all = "camelCase")]
    QuestionRequested {
        /// Owning Session.
        session_id: SessionId,
        /// At least one strict question item.
        questions: Vec<AskUserQuestionItem>,
    },
    /// Final question resolution.
    #[serde(rename = "question/resolved", rename_all = "camelCase")]
    QuestionResolved {
        /// Owning Session.
        session_id: SessionId,
        /// Stable question request correlation id.
        question_rpc_id: RpcId,
        /// Closed resolution outcome.
        outcome: QuestionResolutionOutcome,
    },
    /// Complete pending inbox snapshot.
    #[serde(rename = "session/queue", rename_all = "camelCase")]
    SessionQueue {
        /// Owning Session.
        session_id: SessionId,
        /// Complete queue state.
        items: Vec<QueuedInboxItem>,
    },
    /// Complete visible Job snapshot.
    #[serde(rename = "session/jobs", rename_all = "camelCase")]
    SessionJobs {
        /// Owning Session.
        session_id: SessionId,
        /// Complete Job set.
        jobs: Vec<JobView>,
    },
    /// One projection unit's complete current value.
    #[serde(rename = "session/projection", rename_all = "camelCase")]
    SessionProjection {
        /// Owning Session.
        session_id: SessionId,
        /// Non-empty projection key.
        key: String,
        /// Unit-validated value retained wide.
        value: Value,
        /// Non-negative projection watermark.
        seq: u64,
    },
    /// Terminal stream error.
    #[serde(rename = "stream/error")]
    StreamError {
        /// Closed RPC error.
        error: RpcError,
    },
}

impl MuxFrame {
    /// Parses and normalizes one mux-stream frame.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown frame type or malformed variant payload.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        let kind = require_string(object, "type", "$.type", false)?;
        match kind {
            "session/event" => Ok(Self::SessionEvent {
                session_id: parse_session_id(object)?,
                event: SessionEvent::parse(require_field(object, "event", "$.event")?)?,
                view: object.get("view").map(ToolEventView::parse).transpose()?,
            }),
            "session/subscribed" => Ok(Self::SessionSubscribed {
                session_id: parse_session_id(object)?,
                last_seq: require_integer(object, "lastSeq", "$.lastSeq")?,
            }),
            "approval/requested" => Ok(Self::ApprovalRequested {
                session_id: parse_session_id(object)?,
                approval_id: ApprovalRequestId::new(require_nonempty_string(
                    object,
                    "approvalId",
                    "$.approvalId",
                )?),
                tool_name: require_string(object, "toolName", "$.toolName", false)?.to_owned(),
                call_id: optional_string_value(object, "callId", "$.callId")?.map(CallId::new),
                reason: optional_string_value(object, "reason", "$.reason")?,
            }),
            "approval/resolved" => Ok(Self::ApprovalResolved {
                session_id: parse_session_id(object)?,
                approval_id: ApprovalRequestId::new(require_nonempty_string(
                    object,
                    "approvalId",
                    "$.approvalId",
                )?),
                outcome: parse_approval_outcome(object)?,
            }),
            "question/requested" => {
                let raw = require_array(object, "questions", "$.questions")?;
                if raw.is_empty() {
                    return Err(ContractError::new(
                        "$.questions",
                        "expected non-empty array",
                    ));
                }
                Ok(Self::QuestionRequested {
                    session_id: parse_session_id(object)?,
                    questions: parse_array(raw, parse_question_item, "$.questions")?,
                })
            }
            "question/resolved" => Ok(Self::QuestionResolved {
                session_id: parse_session_id(object)?,
                question_rpc_id: RpcId::new(require_string(
                    object,
                    "questionRpcId",
                    "$.questionRpcId",
                    false,
                )?),
                outcome: match require_string(object, "outcome", "$.outcome", false)? {
                    "answered" => QuestionResolutionOutcome::Answered,
                    "cancelled" => QuestionResolutionOutcome::Cancelled,
                    _ => {
                        return Err(ContractError::new("$.outcome", "unknown question outcome"));
                    }
                },
            }),
            "session/queue" => Ok(Self::SessionQueue {
                session_id: parse_session_id(object)?,
                items: parse_array(
                    require_array(object, "items", "$.items")?,
                    QueuedInboxItem::parse,
                    "$.items",
                )?,
            }),
            "session/jobs" => Ok(Self::SessionJobs {
                session_id: parse_session_id(object)?,
                jobs: parse_array(
                    require_array(object, "jobs", "$.jobs")?,
                    JobView::parse,
                    "$.jobs",
                )?,
            }),
            "session/projection" => Ok(Self::SessionProjection {
                session_id: parse_session_id(object)?,
                key: require_nonempty_string(object, "key", "$.key")?.to_owned(),
                value: require_field(object, "value", "$.value")?.clone(),
                seq: require_nonnegative_integer(object, "seq", "$.seq")?,
            }),
            "stream/error" => Ok(Self::StreamError {
                error: parse_rpc_error(require_field(object, "error", "$.error")?)?,
            }),
            _ => Err(ContractError::new("$.type", "unknown mux frame type")),
        }
    }
}

/// Host-stream payload union.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HostFrame {
    /// A Session was published.
    #[serde(rename = "host/session-added", rename_all = "camelCase")]
    SessionAdded {
        /// New Session id.
        session_id: SessionId,
        /// Conversation-not-started bit.
        blank: bool,
        /// Optional lineage parent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_session_id: Option<SessionId>,
        /// Optional coarse origin.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<SessionAddedOrigin>,
        /// Optional working directory.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// Optional persisted agent preset.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_preset: Option<String>,
    },
    /// A Session was removed.
    #[serde(rename = "host/session-removed", rename_all = "camelCase")]
    SessionRemoved {
        /// Removed Session.
        session_id: SessionId,
    },
    /// Attached Agent running state changed.
    #[serde(rename = "host/session-status", rename_all = "camelCase")]
    SessionStatus {
        /// Session.
        session_id: SessionId,
        /// Running state.
        running: bool,
    },
    /// Live Agent failure with no turn position.
    #[serde(rename = "host/agent-error", rename_all = "camelCase")]
    AgentError {
        /// Session.
        session_id: SessionId,
        /// Diagnostic.
        message: String,
    },
    /// Complete changed Workspace snapshot.
    #[serde(rename = "host/workspace-changed")]
    WorkspaceChanged {
        /// Changed Workspace.
        workspace: WorkspaceView,
    },
    /// Committed Workspace registration deletion.
    #[serde(rename = "host/workspace-removed", rename_all = "camelCase")]
    WorkspaceRemoved {
        /// Removed Workspace id.
        workspace_id: WorkspaceId,
    },
    /// Complete durable Workspace ordering.
    #[serde(rename = "host/workspace-order-changed", rename_all = "camelCase")]
    WorkspaceOrderChanged {
        /// Ordered Workspace ids.
        workspace_ids: Vec<WorkspaceId>,
    },
    /// Complete archived Session set.
    #[serde(rename = "host/archived-sessions-changed", rename_all = "camelCase")]
    ArchivedSessionsChanged {
        /// Archived Session ids.
        archived_session_ids: Vec<SessionId>,
    },
    /// Allowlisted Cordis event forwarded without projection.
    #[serde(rename = "host/remote-event")]
    RemoteEvent {
        /// Non-empty Host event name.
        event: String,
        /// JSON-safe arguments retained wide.
        args: Vec<Value>,
    },
    /// Terminal stream error.
    #[serde(rename = "stream/error")]
    StreamError {
        /// Closed RPC error.
        error: RpcError,
    },
}

/// Closed coarse Session-added origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionAddedOrigin {
    /// Delegated child Session.
    Subagent,
}

impl HostFrame {
    /// Parses and normalizes one Host-stream frame.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown frame type or malformed variant payload.
    pub fn parse(value: &Value) -> Result<Self, ContractError> {
        let object = require_object(value, "$")?;
        match require_string(object, "type", "$.type", false)? {
            "host/session-added" => {
                optional_nonempty_string(object, "parentSessionId", "$.parentSessionId")?;
                optional_literal(object, "origin", "subagent", "$.origin")?;
                optional_string(object, "cwd", "$.cwd", false)?;
                optional_string(object, "agentPreset", "$.agentPreset", false)?;
                Ok(Self::SessionAdded {
                    session_id: parse_session_id(object)?,
                    blank: require_bool(object, "blank", "$.blank")?,
                    parent_session_id: optional_string_value(
                        object,
                        "parentSessionId",
                        "$.parentSessionId",
                    )?
                    .map(SessionId::new),
                    origin: object
                        .contains_key("origin")
                        .then_some(SessionAddedOrigin::Subagent),
                    cwd: optional_string_value(object, "cwd", "$.cwd")?,
                    agent_preset: optional_string_value(object, "agentPreset", "$.agentPreset")?,
                })
            }
            "host/session-removed" => Ok(Self::SessionRemoved {
                session_id: parse_session_id(object)?,
            }),
            "host/session-status" => Ok(Self::SessionStatus {
                session_id: parse_session_id(object)?,
                running: require_bool(object, "running", "$.running")?,
            }),
            "host/agent-error" => Ok(Self::AgentError {
                session_id: parse_session_id(object)?,
                message: require_string(object, "message", "$.message", false)?.to_owned(),
            }),
            "host/workspace-changed" => Ok(Self::WorkspaceChanged {
                workspace: WorkspaceView::parse(require_field(
                    object,
                    "workspace",
                    "$.workspace",
                )?)?,
            }),
            "host/workspace-removed" => Ok(Self::WorkspaceRemoved {
                workspace_id: WorkspaceId::new(require_nonempty_string(
                    object,
                    "workspaceId",
                    "$.workspaceId",
                )?),
            }),
            "host/workspace-order-changed" => Ok(Self::WorkspaceOrderChanged {
                workspace_ids: parse_workspace_ids(require_array(
                    object,
                    "workspaceIds",
                    "$.workspaceIds",
                )?)?,
            }),
            "host/archived-sessions-changed" => Ok(Self::ArchivedSessionsChanged {
                archived_session_ids: parse_session_ids(require_array(
                    object,
                    "archivedSessionIds",
                    "$.archivedSessionIds",
                )?)?,
            }),
            "host/remote-event" => Ok(Self::RemoteEvent {
                event: require_nonempty_string(object, "event", "$.event")?.to_owned(),
                args: require_array(object, "args", "$.args")?.clone(),
            }),
            "stream/error" => Ok(Self::StreamError {
                error: parse_rpc_error(require_field(object, "error", "$.error")?)?,
            }),
            _ => Err(ContractError::new("$.type", "unknown Host frame type")),
        }
    }
}

fn parse_session_id(object: &Map<String, Value>) -> Result<SessionId, ContractError> {
    Ok(SessionId::new(require_nonempty_string(
        object,
        "sessionId",
        "$.sessionId",
    )?))
}

fn require_integer(
    object: &Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<i64, ContractError> {
    require_field(object, name, path)?
        .as_i64()
        .ok_or_else(|| ContractError::new(path, "expected integer"))
}

fn optional_string_value(
    object: &Map<String, Value>,
    name: &str,
    path: &str,
) -> Result<Option<String>, ContractError> {
    optional_string(object, name, path, false)?;
    Ok(object
        .get(name)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
}

fn parse_approval_outcome(object: &Map<String, Value>) -> Result<ApprovalOutcome, ContractError> {
    match require_string(object, "outcome", "$.outcome", false)? {
        "allowed-once" => Ok(ApprovalOutcome::AllowedOnce),
        "rejected" => Ok(ApprovalOutcome::Rejected),
        "cancelled" => Ok(ApprovalOutcome::Cancelled),
        "unavailable" => Ok(ApprovalOutcome::Unavailable),
        _ => Err(ContractError::new("$.outcome", "unknown approval outcome")),
    }
}

fn parse_session_ids(values: &[Value]) -> Result<Vec<SessionId>, ContractError> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .filter(|id| !id.is_empty())
                .map(SessionId::new)
                .ok_or_else(|| {
                    ContractError::new(format!("$[{index}]"), "expected non-empty Session id")
                })
        })
        .collect()
}

fn parse_workspace_ids(values: &[Value]) -> Result<Vec<WorkspaceId>, ContractError> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .filter(|id| !id.is_empty())
                .map(WorkspaceId::new)
                .ok_or_else(|| {
                    ContractError::new(format!("$[{index}]"), "expected non-empty Workspace id")
                })
        })
        .collect()
}
