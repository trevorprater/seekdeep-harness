//! Host BFF policy for resolving Remote Agent and Session identities.

use seekdeep_core::session::SessionId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Caller-facing failures preserved by the Gateway's RPC adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "kebab-case")]
pub enum ApiRemoteLookupError {
    /// The identity is fenced by an active agent.
    AgentBusy {
        /// Human-readable reason.
        message: String,
        /// Busy detail.
        details: AgentBusyDetails,
    },
    /// The identity has no project-backed session.
    SessionNotFound {
        /// Human-readable reason.
        message: String,
        /// Missing-session detail.
        details: SessionNotFoundDetails,
    },
    /// An internal resume failure.
    Internal {
        /// Human-readable reason.
        message: String,
        /// Empty detail object.
        details: serde_json::Value,
    },
}

/// Detail for the agent-busy failure class.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentBusyDetails {
    /// Stable reason string.
    pub reason: String,
}

/// Detail for the session-not-found failure class.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionNotFoundDetails {
    /// Missing session identity.
    pub session_id: SessionId,
}

/// Cold identity absent from the durable session store.
#[derive(Clone, Debug, Error)]
#[error("{0}")]
pub struct ApiRemoteSessionNotFound(pub String);

/// Session identity whose lifecycle belongs to subagent routing.
#[derive(Clone, Debug, Error)]
#[error("session \"{session_id}\" is a subagent session; use subagent delivery")]
pub struct ApiRemoteSubagentSessionOwnership {
    /// Identity reserved to subagent routing.
    pub session_id: SessionId,
}

/// Builds the stable caller-facing ownership rejection.
#[must_use]
pub fn api_remote_subagent_ownership_error(session_id: &SessionId) -> ApiRemoteLookupError {
    ApiRemoteLookupError::AgentBusy {
        message: format!("session \"{session_id}\" is owned by subagent routing"),
        details: AgentBusyDetails {
            reason: "use subagent delivery for this child session".to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_error_uses_agent_busy_shape() {
        let session_id = SessionId::new("s1");
        let error = api_remote_subagent_ownership_error(&session_id);
        match error {
            ApiRemoteLookupError::AgentBusy { message, details } => {
                assert_eq!(message, "session \"s1\" is owned by subagent routing");
                assert_eq!(
                    details.reason,
                    "use subagent delivery for this child session"
                );
            }
            other => panic!("expected agent-busy, got {other:?}"),
        }
    }

    #[test]
    fn lookup_error_serializes_by_code() {
        let error = ApiRemoteLookupError::SessionNotFound {
            message: "not found".to_owned(),
            details: SessionNotFoundDetails {
                session_id: SessionId::new("s1"),
            },
        };
        let value = serde_json::to_value(&error).expect("serialize");
        assert_eq!(value["code"], "session-not-found");
        assert_eq!(value["details"]["sessionId"], "s1");
    }

    #[test]
    fn subagent_ownership_error_carries_session_id() {
        let error = ApiRemoteSubagentSessionOwnership {
            session_id: SessionId::new("s1"),
        };
        assert_eq!(
            error.to_string(),
            "session \"s1\" is a subagent session; use subagent delivery"
        );
    }
}
