//! Host BFF policy for resolving Remote Agent and Session identities.

use std::sync::Arc;

use futures::future::BoxFuture;
use seekdeep_cordis::Context;
use seekdeep_core::session::{SessionEvent, SessionHeader, SessionId, SessionOrigin};
use seekdeep_core::session_store::SESSIONS;
use seekdeep_session_persistence::{SESSION_PERSISTENCE, SessionInspection};
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

/// Result of resolving one session identity to its live Agent.
#[derive(Clone, Debug)]
pub enum ApiRemoteAgentResult {
    /// The resolved live agent.
    Agent(Arc<seekdeep_agent::Agent>),
    /// The caller-facing failure.
    Error(ApiRemoteLookupError),
}

#[allow(clippy::type_complexity)]
type ApiRemoteSetup = Arc<
    dyn Fn(
            SessionInspection,
        ) -> BoxFuture<'static, anyhow::Result<Option<seekdeep_agent::AgentSetup>>>
        + Send
        + Sync,
>;
type ApiRemoteAgentOptionsFn = Arc<dyn Fn() -> seekdeep_agent::AgentOptions + Send + Sync>;

/// Resume configuration supplied by the owning Host composition.
pub struct ApiRemoteAgentOptions {
    /// Read the per-Agent defaults when a cold identity must resume.
    pub agent_options: Option<ApiRemoteAgentOptionsFn>,
    /// Build the Host-specific Agent-scope composition before publication.
    pub setup: Option<ApiRemoteSetup>,
}

/// Tests whether generic Host routing must leave an identity to subagent routing.
#[must_use]
pub fn has_api_remote_subagent_owner(
    ctx: &Context,
    header: &SessionHeader,
    agent: Option<&Arc<seekdeep_agent::Agent>>,
) -> bool {
    if header.origin == Some(SessionOrigin::Subagent) {
        return true;
    }
    let Some(parent_id) = &header.parent_session else {
        return false;
    };
    let Some(agent) = agent else {
        return false;
    };
    let Some(registry) = ctx.get(seekdeep_agent::AGENTS) else {
        return false;
    };
    let Some(parent) = registry.get(parent_id) else {
        return false;
    };
    registry.is_owned_by(agent.id(), &parent)
}

/// Inspects one cold served session without repairing, resuming, or publishing it.
///
/// # Errors
///
/// Returns a missing-persistence or not-found failure.
pub async fn inspect_api_remote_session(
    ctx: &Context,
    session_id: &SessionId,
) -> anyhow::Result<(SessionHeader, Vec<SessionEvent>)> {
    let persistence = ctx.get(SESSION_PERSISTENCE).ok_or_else(|| {
        anyhow::anyhow!(
            "session persistence is not configured (load a seekdeep-session-persistence backend)"
        )
    })?;
    let backend = persistence.persistence();
    let listed = backend.list(None).await?;
    if !listed
        .iter()
        .any(|meta| meta.id == *session_id && meta.cwd.is_some())
    {
        return Err(ApiRemoteSessionNotFound(format!("session {session_id:?} not found")).into());
    }
    let inspected = backend.inspect(session_id, None).await?;
    if inspected.meta.cwd.is_none() {
        return Err(ApiRemoteSessionNotFound(format!("session {session_id:?} not found")).into());
    }
    Ok((inspected.meta, inspected.events))
}

async fn resolve_agent(
    ctx: &Context,
    session_id: &SessionId,
    agent_options: Option<&ApiRemoteAgentOptionsFn>,
    setup: Option<&ApiRemoteSetup>,
) -> ApiRemoteAgentResult {
    // Live agent reuse.
    if let Some(registry) = ctx.get(seekdeep_agent::AGENTS)
        && let Some(live) = registry.get(session_id)
    {
        if has_api_remote_subagent_owner(ctx, live.session().header(), Some(&live)) {
            return ApiRemoteAgentResult::Error(api_remote_subagent_ownership_error(session_id));
        }
        return ApiRemoteAgentResult::Agent(live);
    }
    // Attached subagent-owner fence.
    if let Some(store) = ctx.get(SESSIONS)
        && let Some(attached) = store.get(session_id)
        && has_api_remote_subagent_owner(ctx, attached.header(), None)
    {
        return ApiRemoteAgentResult::Error(api_remote_subagent_ownership_error(session_id));
    }
    // Cold resume.
    let resumed: anyhow::Result<ApiRemoteAgentResult> = async {
        let (meta, _events) = inspect_api_remote_session(ctx, session_id).await?;
        if has_api_remote_subagent_owner(ctx, &meta, None) {
            return Err(ApiRemoteSubagentSessionOwnership {
                session_id: session_id.clone(),
            }
            .into());
        }
        let inspection = ctx
            .get(SESSION_PERSISTENCE)
            .ok_or_else(|| anyhow::anyhow!("session persistence is not configured"))?
            .persistence()
            .inspect(session_id, None)
            .await?;
        let setup_value = match setup {
            None => None,
            Some(setup) => setup(inspection).await?,
        };
        let mut resume = seekdeep_agent::ResumeAgentOptions::new(session_id.clone());
        if let Some(agent_options) = agent_options {
            resume.agent_options = agent_options();
        }
        resume.setup = setup_value;
        let registry = ctx
            .get(seekdeep_agent::AGENTS)
            .ok_or_else(|| anyhow::anyhow!("agents registry is not mounted"))?;
        let handle = registry.resume(resume).await?;
        Ok(ApiRemoteAgentResult::Agent(handle.agent))
    }
    .await;
    match resumed {
        Ok(result) => result,
        Err(error) => {
            if error.downcast_ref::<ApiRemoteSessionNotFound>().is_some() {
                ApiRemoteAgentResult::Error(ApiRemoteLookupError::SessionNotFound {
                    message: error.to_string(),
                    details: SessionNotFoundDetails {
                        session_id: session_id.clone(),
                    },
                })
            } else if error
                .downcast_ref::<ApiRemoteSubagentSessionOwnership>()
                .is_some()
            {
                ApiRemoteAgentResult::Error(api_remote_subagent_ownership_error(session_id))
            } else {
                ApiRemoteAgentResult::Error(ApiRemoteLookupError::Internal {
                    message: format!("resume failed for session {session_id:?}: {error}"),
                    details: serde_json::Value::Object(serde_json::Map::new()),
                })
            }
        }
    }
}

/// Creates the Host's shared Agent resolver.
#[must_use]
pub fn create_api_remote_agent_resolver(
    ctx: &Context,
    options: ApiRemoteAgentOptions,
) -> Arc<dyn Fn(SessionId) -> BoxFuture<'static, ApiRemoteAgentResult> + Send + Sync> {
    let ctx = ctx.clone();
    let agent_options = options.agent_options;
    let setup = options.setup;
    Arc::new(move |session_id: SessionId| {
        let ctx = ctx.clone();
        let agent_options = agent_options.clone();
        let setup = setup.clone();
        Box::pin(async move {
            resolve_agent(&ctx, &session_id, agent_options.as_ref(), setup.as_ref()).await
        })
    })
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
