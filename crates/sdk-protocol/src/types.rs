//! Named request, result, and notification payloads for the SDK runtime protocol.

use seekdeep_core::session::{SessionEvent, SessionId};
use seekdeep_llm::{ContentBlock, MessageId, ModelId, ProviderId};
use seekdeep_lossless_json::JsonValue;
use seekdeep_subagent::SubagentStopReason;
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{DeserializeOwned, Error as _},
};

/// Parameters for the process-wide SDK handshake.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    /// Working directory recorded on every SDK-created session header.
    pub cwd: String,
    /// Provider route every SDK-created agent runs on.
    pub provider: ProviderId,
    /// Model name every SDK-created agent runs on.
    pub model: ModelId,
    /// Optional positive output-token cap inherited by SDK-created agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
}

/// Wire-stable server identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    /// Stable runtime name (`seekdeep-harness-sdk-runtime`).
    pub name: String,
    /// Runtime version.
    pub version: String,
}

/// Result of SDK initialization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// Wire-stable server identity and version.
    pub server_info: ServerInfo,
}

/// One user turn on one SDK session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPromptParams {
    /// SDK-side session identity.
    pub session_id: SessionId,
    /// Prompt blocks sent verbatim as the user message.
    pub content_blocks: Vec<ContentBlock>,
}

/// Durable enqueue receipt for one prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPromptResult {
    /// Identity of the queued user message.
    pub message_id: MessageId,
}

/// Deployment-mapped SDK outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SdkRunStatus {
    /// Accepted result.
    Ok,
    /// Any non-accepted result.
    Error,
}

/// One session-log event streamed as it is recorded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEventNotification {
    /// Session owning the event.
    pub session_id: SessionId,
    /// Complete durable event envelope.
    pub event: SessionEvent,
}

/// Whole-agent lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    /// Between turns.
    Idle,
    /// Working now.
    Running,
}

/// One whole-agent lifecycle transition.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatusNotification {
    /// Session whose live agent changed status.
    pub session_id: SessionId,
    /// State after the transition.
    pub status: SessionStatus,
}

/// One in-runtime child publication.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentStartedNotification {
    /// Delegating session.
    pub parent_session_id: SessionId,
    /// New child session.
    pub child_session_id: SessionId,
}

/// One in-process subagent terminal notification.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentFinishedNotification {
    /// Provider that ran the child.
    pub provider: String,
    /// Provider-reported child identity.
    pub agent_id: SessionId,
    /// Delegating session.
    pub parent_session_id: SessionId,
    /// Child session.
    pub child_session_id: SessionId,
    /// Deployment-mapped outcome.
    pub status: SdkRunStatus,
    /// Provider-reported stop reason.
    pub stop_reason: SubagentStopReason,
    /// Selected assistant output, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<Vec<ContentBlock>>,
}

/// Closed server-to-client notification map.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "method", content = "params")]
pub enum HarnessSdkNotification {
    /// `session.event`.
    #[serde(rename = "session.event")]
    SessionEvent(SessionEventNotification),
    /// `session.status`.
    #[serde(rename = "session.status")]
    SessionStatus(SessionStatusNotification),
    /// `subagent.started`.
    #[serde(rename = "subagent.started")]
    SubagentStarted(SubagentStartedNotification),
    /// `subagent.finished`.
    #[serde(rename = "subagent.finished")]
    SubagentFinished(SubagentFinishedNotification),
}

/// Closed client-to-server request map.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "method", content = "params")]
pub enum HarnessSdkRequest {
    /// Process-wide handshake.
    #[serde(rename = "initialize")]
    Initialize(InitializeParams),
    /// Enqueue one user prompt.
    #[serde(rename = "session/prompt")]
    SessionPrompt(SessionPromptParams),
    /// Shut the runtime down; source wire params are absent.
    #[serde(rename = "shutdown")]
    Shutdown,
}

// Adjacent-tag buffering decodes strings before raw payload fields can retain their code units.
impl<'de> Deserialize<'de> for HarnessSdkNotification {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let envelope = <JsonValue as Deserialize>::deserialize(deserializer)?;
        let method = envelope_method::<D::Error>(&envelope)?;
        match method.as_str() {
            "session.event" => envelope_params(&envelope).map(Self::SessionEvent),
            "session.status" => envelope_params(&envelope).map(Self::SessionStatus),
            "subagent.started" => envelope_params(&envelope).map(Self::SubagentStarted),
            "subagent.finished" => envelope_params(&envelope).map(Self::SubagentFinished),
            _ => Err(D::Error::unknown_variant(
                &method,
                &[
                    "session.event",
                    "session.status",
                    "subagent.started",
                    "subagent.finished",
                ],
            )),
        }
    }
}

impl<'de> Deserialize<'de> for HarnessSdkRequest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let envelope = <JsonValue as Deserialize>::deserialize(deserializer)?;
        let method = envelope_method::<D::Error>(&envelope)?;
        match method.as_str() {
            "initialize" => envelope_params(&envelope).map(Self::Initialize),
            "session/prompt" => envelope_params(&envelope).map(Self::SessionPrompt),
            "shutdown" => {
                if let Some(params) = envelope.get("params") {
                    params.deserialize::<()>().map_err(D::Error::custom)?;
                }
                Ok(Self::Shutdown)
            }
            _ => Err(D::Error::unknown_variant(
                &method,
                &["initialize", "session/prompt", "shutdown"],
            )),
        }
    }
}

fn envelope_method<E: serde::de::Error>(envelope: &JsonValue) -> Result<String, E> {
    envelope
        .get("method")
        .ok_or_else(|| E::missing_field("method"))?
        .deserialize()
        .map_err(E::custom)
}

fn envelope_params<T: DeserializeOwned, E: serde::de::Error>(envelope: &JsonValue) -> Result<T, E> {
    envelope
        .get("params")
        .ok_or_else(|| E::missing_field("params"))?
        .deserialize()
        .map_err(E::custom)
}
