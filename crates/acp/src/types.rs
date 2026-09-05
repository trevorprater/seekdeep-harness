//! ACP version, method, content, permission, and terminal vocabulary.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Pinned `@agentclientprotocol/sdk@0.25.1` protocol version.
pub const PROTOCOL_VERSION: u64 = 1;

/// ACP agent-side method names used by the automation bridge.
pub mod agent_methods {
    /// Version/capability negotiation.
    pub const INITIALIZE: &str = "initialize";
    /// Authentication no-op.
    pub const AUTHENTICATE: &str = "authenticate";
    /// Fresh session creation.
    pub const SESSION_NEW: &str = "session/new";
    /// One prompt activity.
    pub const SESSION_PROMPT: &str = "session/prompt";
    /// Session-scoped cancellation notification.
    pub const SESSION_CANCEL: &str = "session/cancel";
}

/// ACP client-side method names used by the automation bridge.
pub mod client_methods {
    /// Committed session update notification.
    pub const SESSION_UPDATE: &str = "session/update";
    /// One-shot permission request.
    pub const SESSION_REQUEST_PERMISSION: &str = "session/request_permission";
}

/// Branded remote session identity within one ACP connection.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AcpSessionId(String);

impl AcpSessionId {
    /// Brands a wire identity.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Closed ACP prompt terminal vocabulary plus unknown-value preservation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AcpStopReason {
    /// Normal turn completion.
    EndTurn,
    /// Output token ceiling.
    MaxTokens,
    /// Product refusal.
    Refusal,
    /// Explicit cancellation.
    Cancelled,
    /// Turn-request budget exhaustion.
    MaxTurnRequests,
    /// Future wire member.
    Unknown(String),
}

impl AcpStopReason {
    /// Parses one wire value without discarding future members.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "end_turn" => Self::EndTurn,
            "max_tokens" => Self::MaxTokens,
            "refusal" => Self::Refusal,
            "cancelled" => Self::Cancelled,
            "max_turn_requests" => Self::MaxTurnRequests,
            value => Self::Unknown(value.to_owned()),
        }
    }

    /// Exact wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::EndTurn => "end_turn",
            Self::MaxTokens => "max_tokens",
            Self::Refusal => "refusal",
            Self::Cancelled => "cancelled",
            Self::MaxTurnRequests => "max_turn_requests",
            Self::Unknown(value) => value,
        }
    }
}

impl Serialize for AcpStopReason {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AcpStopReason {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        String::deserialize(deserializer).map(|value| Self::parse(&value))
    }
}

/// Client-side automatic permission policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionPolicy {
    /// Reject every request.
    #[default]
    Reject,
    /// Select the first allow-once or allow-always option.
    Allow,
}

/// One client-observed session update.
#[derive(Clone, Debug, PartialEq)]
pub struct AcpSessionUpdate {
    /// Remote session.
    pub session_id: AcpSessionId,
    /// Raw merge-extensible update.
    pub update: Value,
}
