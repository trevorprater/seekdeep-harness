//! Host BFF policy for resolving Remote Agent and Session identities.

use std::{collections::HashMap, sync::Arc};

use futures::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use parking_lot::Mutex;
use seekdeep_cordis::{Context, Plugin};
use seekdeep_core::session::{SessionEvent, SessionHeader, SessionId, SessionOrigin};
use seekdeep_core::session_store::SESSIONS;
use seekdeep_session_persistence::{SESSION_PERSISTENCE, SessionInspection};
use seekdeep_typert_protocol::{
    TypertBoundaryValue, TypertContextRegistry as _, TypertHostObject, TypertLookupFailure,
    TypertLookupRegistry as _, TypertLookupResolver,
};
use seekdeep_typert_registry::TYPERT;
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

type SharedResume =
    Shared<BoxFuture<'static, Result<Arc<seekdeep_agent::Agent>, Arc<anyhow::Error>>>>;
type SharedResumes = Arc<Mutex<HashMap<SessionId, SharedResume>>>;

async fn resume_agent(
    ctx: &Context,
    session_id: &SessionId,
    agent_options: Option<&ApiRemoteAgentOptionsFn>,
    setup: Option<&ApiRemoteSetup>,
) -> anyhow::Result<Arc<seekdeep_agent::Agent>> {
    let (meta, events) = inspect_api_remote_session(ctx, session_id).await?;
    if has_api_remote_subagent_owner(ctx, &meta, None) {
        return Err(ApiRemoteSubagentSessionOwnership {
            session_id: session_id.clone(),
        }
        .into());
    }
    let setup_value = match setup {
        None => None,
        Some(setup) => {
            setup(SessionInspection {
                meta: meta.clone(),
                events,
            })
            .await?
        }
    };
    let published_session = ctx.get(SESSIONS).and_then(|store| store.get(session_id));
    let published_agent = ctx
        .get(seekdeep_agent::AGENTS)
        .and_then(|registry| registry.get(session_id));
    if published_session.as_ref().is_some_and(|session| {
        has_api_remote_subagent_owner(ctx, session.header(), published_agent.as_ref())
    }) {
        return Err(ApiRemoteSubagentSessionOwnership {
            session_id: session_id.clone(),
        }
        .into());
    }
    let mut resume = seekdeep_agent::ResumeAgentOptions::new(session_id.clone());
    if let Some(agent_options) = agent_options {
        resume.agent_options = agent_options();
    }
    resume.setup = setup_value;
    let registry = ctx
        .get(seekdeep_agent::AGENTS)
        .ok_or_else(|| anyhow::anyhow!("agents registry is not mounted"))?;
    Ok(registry.resume(resume).await?.agent)
}

fn fenced_live_agent(ctx: &Context, session_id: &SessionId) -> Option<ApiRemoteAgentResult> {
    let registry = ctx.get(seekdeep_agent::AGENTS)?;
    let live = registry.get(session_id)?;
    Some(
        if has_api_remote_subagent_owner(ctx, live.session().header(), Some(&live)) {
            ApiRemoteAgentResult::Error(api_remote_subagent_ownership_error(session_id))
        } else {
            ApiRemoteAgentResult::Agent(live)
        },
    )
}

fn classify_resume_error(
    ctx: &Context,
    session_id: &SessionId,
    error: &anyhow::Error,
) -> ApiRemoteAgentResult {
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
        if let Some(fenced) = fenced_live_agent(ctx, session_id) {
            return fenced;
        }
        if let Some(attached) = ctx.get(SESSIONS).and_then(|store| store.get(session_id))
            && has_api_remote_subagent_owner(ctx, attached.header(), None)
        {
            return ApiRemoteAgentResult::Error(api_remote_subagent_ownership_error(session_id));
        }
        ApiRemoteAgentResult::Error(ApiRemoteLookupError::Internal {
            message: format!("resume failed for session {session_id:?}: {error}"),
            details: serde_json::Value::Object(serde_json::Map::new()),
        })
    }
}

async fn resolve_agent(
    ctx: &Context,
    session_id: &SessionId,
    agent_options: Option<&ApiRemoteAgentOptionsFn>,
    setup: Option<&ApiRemoteSetup>,
    resumes: &SharedResumes,
) -> ApiRemoteAgentResult {
    if let Some(fenced) = fenced_live_agent(ctx, session_id) {
        return fenced;
    }
    if let Some(attached) = ctx.get(SESSIONS).and_then(|store| store.get(session_id))
        && has_api_remote_subagent_owner(ctx, attached.header(), None)
    {
        return ApiRemoteAgentResult::Error(api_remote_subagent_ownership_error(session_id));
    }
    let resume = {
        let mut in_flight = resumes.lock();
        if let Some(resume) = in_flight.get(session_id) {
            resume.clone()
        } else {
            let ctx = ctx.clone();
            let session_id = session_id.clone();
            let insert_id = session_id.clone();
            let cleanup_id = session_id.clone();
            let agent_options = agent_options.cloned();
            let setup = setup.cloned();
            let resumes = resumes.clone();
            let resume = async move {
                let result =
                    resume_agent(&ctx, &session_id, agent_options.as_ref(), setup.as_ref())
                        .await
                        .map_err(Arc::new);
                resumes.lock().remove(&cleanup_id);
                result
            }
            .boxed()
            .shared();
            in_flight.insert(insert_id, resume.clone());
            resume
        }
    };
    match resume.await {
        Ok(agent) => ApiRemoteAgentResult::Agent(agent),
        Err(error) => classify_resume_error(ctx, session_id, &error),
    }
}

/// Creates the Host's shared Agent resolver.
#[must_use]
pub fn create_api_remote_agent_resolver(
    ctx: &Context,
    options: ApiRemoteAgentOptions,
) -> Arc<dyn Fn(SessionId) -> BoxFuture<'static, ApiRemoteAgentResult> + Send + Sync> {
    let owner_ctx = ctx.clone();
    let resolver_ctx = owner_ctx.clone();
    let agent_options = options.agent_options;
    let setup = options.setup;
    let resumes = Arc::new(Mutex::new(HashMap::new()));
    let resolver: Arc<dyn Fn(SessionId) -> BoxFuture<'static, ApiRemoteAgentResult> + Send + Sync> =
        Arc::new(move |session_id: SessionId| {
            let ctx = resolver_ctx.clone();
            let agent_options = agent_options.clone();
            let setup = setup.clone();
            let resumes = resumes.clone();
            Box::pin(async move {
                resolve_agent(
                    &ctx,
                    &session_id,
                    agent_options.as_ref(),
                    setup.as_ref(),
                    &resumes,
                )
                .await
            })
        });
    install_typert_resolvers(&owner_ctx, &resolver);
    resolver
}

fn install_typert_resolvers(
    ctx: &Context,
    resolver: &Arc<dyn Fn(SessionId) -> BoxFuture<'static, ApiRemoteAgentResult> + Send + Sync>,
) {
    let agent_resolver = resolver.clone();
    let plugin = Plugin::new(
        "api-remote-agent-resolvers",
        ["typert"],
        move |context, _| {
            let agent_resolver = agent_resolver.clone();
            Box::pin(async move {
                let typert = context
                    .get(TYPERT)
                    .ok_or_else(|| anyhow::anyhow!("api-remotes requires typert"))?;
                let resolve_agent = {
                    let agent_resolver = agent_resolver.clone();
                    Arc::new(move |value: TypertBoundaryValue| {
                        let agent_resolver = agent_resolver.clone();
                        Box::pin(async move {
                            let session_id = boundary_session_id(value)?;
                            match agent_resolver(session_id).await {
                                ApiRemoteAgentResult::Agent(agent) => {
                                    Ok(Some(agent as TypertHostObject))
                                }
                                ApiRemoteAgentResult::Error(error) => {
                                    Err(TypertLookupFailure::new(serde_json::to_value(error)?)
                                        .into())
                                }
                            }
                        }) as seekdeep_typert_protocol::TypertLookupFuture
                    }) as TypertLookupResolver
                };
                typert
                    .lookups()
                    .configure(&context, "agent", resolve_agent.clone())?;
                let session_resolver = Arc::new(move |value: TypertBoundaryValue| {
                    let resolve_agent = resolve_agent.clone();
                    Box::pin(async move {
                        let agent = resolve_agent(value).await?;
                        Ok(agent.and_then(|object| {
                            Arc::downcast::<seekdeep_agent::Agent>(object)
                                .ok()
                                .map(|agent| agent.session().clone() as TypertHostObject)
                        }))
                    }) as seekdeep_typert_protocol::TypertLookupFuture
                });
                typert
                    .lookups()
                    .configure(&context, "session", session_resolver)?;
                let context_resolver = {
                    let agent_resolver = agent_resolver.clone();
                    Arc::new(move |value: TypertBoundaryValue| {
                        let agent_resolver = agent_resolver.clone();
                        Box::pin(async move {
                            let session_id = boundary_session_id(value)?;
                            match agent_resolver(session_id).await {
                                ApiRemoteAgentResult::Agent(agent) => {
                                    Ok(Some(agent.context().clone()))
                                }
                                ApiRemoteAgentResult::Error(error) => {
                                    Err(TypertLookupFailure::new(serde_json::to_value(error)?)
                                        .into())
                                }
                            }
                        })
                            as seekdeep_typert_protocol::TypertHostContextFuture
                    })
                };
                typert
                    .contexts()
                    .configure_host(&context, "agent", context_resolver)?;
                Ok(())
            })
        },
    );
    if let Err(error) = ctx.plugin(plugin, serde_json::Value::Null) {
        tracing::warn!(%error, "API Remote Typert resolver mount failed");
    }
}

fn boundary_session_id(value: TypertBoundaryValue) -> anyhow::Result<SessionId> {
    serde_json::from_value(
        value
            .into_optional_json()
            .ok_or_else(|| anyhow::anyhow!("session identity is undefined"))?,
    )
    .map_err(Into::into)
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
