//! Production Session-domain runtime, including live and cold listing semantics.

use std::{collections::BTreeMap, sync::Arc};

use futures::{FutureExt as _, future::BoxFuture};
use seekdeep_agent::{AGENTS, AgentRegistry, AgentStatus};
use seekdeep_client_connection::{HttpResponse, RpcResult};
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{Session, SessionEvent, SessionHeader, SessionOrigin},
    session_store::{SESSIONS, SessionStore},
};
use seekdeep_llm::AbortSignal;
use seekdeep_session_persistence::{SESSION_PERSISTENCE, SessionPersistence};
use seekdeep_session_projection::{
    ProjectionDefinition, ProjectionSnapshot, ProjectionTransition, SESSION_PROJECTIONS,
    SessionProjectionRegistry,
};
use seekdeep_session_projection_cache::{SESSION_PROJECTION_CACHE, SessionProjectionCache};
use serde_json::Value;

use crate::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, RpcMethod, RpcReceipt, RpcRequest,
    RpcResponse,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
        sessions::{
            SessionListMetadata, SessionListValue, SessionProjectionsBlock, SessionSummary,
            SessionSummaryOrigin,
        },
    },
};

/// Source default for one cold blankness artifact probe.
pub const DEFAULT_COLD_BLANK_PROBE_MAX_BYTES: u64 = 1024;
const COLD_SUMMARY_BATCH_SIZE: usize = 16;

/// Session-domain runtime options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SessionApiProxyOptions {
    /// Maximum physical artifact size eligible for a cold blankness read.
    pub cold_blank_probe_max_bytes: Option<u64>,
}

/// Optional projection reads used by Session summaries.
pub trait SessionProjectionReads: Send + Sync + 'static {
    /// Reads the complete live projection cut, when projection services exist.
    ///
    /// # Errors
    ///
    /// Returns a projection initialization, fold, or view failure.
    fn live_snapshot(&self, session: &Arc<Session>) -> anyhow::Result<Option<ProjectionSnapshot>>;

    /// Reads the zero-I/O cold projection cut, when a cache row exists.
    ///
    /// # Errors
    ///
    /// Returns a cache decoding or projection-view failure.
    fn cached_snapshot(&self, meta: &SessionHeader) -> anyhow::Result<Option<ProjectionSnapshot>>;
}

#[derive(Debug)]
struct CordisProjectionReads {
    live: Option<Arc<SessionProjectionRegistry>>,
    cache: Option<Arc<SessionProjectionCache>>,
}

impl SessionProjectionReads for CordisProjectionReads {
    fn live_snapshot(&self, session: &Arc<Session>) -> anyhow::Result<Option<ProjectionSnapshot>> {
        self.live
            .as_ref()
            .map(|registry| registry.snapshot(session))
            .transpose()
    }

    fn cached_snapshot(&self, meta: &SessionHeader) -> anyhow::Result<Option<ProjectionSnapshot>> {
        Ok(self
            .cache
            .as_ref()
            .and_then(|cache| cache.cached_snapshot(meta)))
    }
}

/// Session-domain decorator over the remaining API Proxy domains.
pub struct SessionApiProxyRuntime {
    sessions: Arc<SessionStore>,
    agents: Arc<AgentRegistry>,
    persistence: Option<Arc<dyn SessionPersistence>>,
    projections: Arc<dyn SessionProjectionReads>,
    options: SessionApiProxyOptions,
    domains: Arc<dyn ApiProxyRuntime>,
}

impl std::fmt::Debug for SessionApiProxyRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionApiProxyRuntime")
            .field("live_sessions", &self.sessions.list().len())
            .field("has_persistence", &self.persistence.is_some())
            .field("options", &self.options)
            .finish_non_exhaustive()
    }
}

impl SessionApiProxyRuntime {
    /// Resolves the required Session and Agent registries plus optional cold-read services.
    ///
    /// # Errors
    ///
    /// Returns when the required Session or Agent registry is absent.
    pub fn from_context(
        context: &Context,
        options: SessionApiProxyOptions,
        domains: Arc<dyn ApiProxyRuntime>,
    ) -> anyhow::Result<Arc<Self>> {
        let sessions = context
            .get(SESSIONS)
            .ok_or_else(|| anyhow::anyhow!("sessions service is required"))?;
        let agents = context
            .get(AGENTS)
            .ok_or_else(|| anyhow::anyhow!("agents service is required"))?;
        let persistence = context
            .get(SESSION_PERSISTENCE)
            .map(|service| service.persistence());
        let projection_registry = context.get(SESSION_PROJECTIONS);
        if let Some(registry) = &projection_registry {
            registry.register(context, session_list_projection_definition())?;
        }
        let projections: Arc<dyn SessionProjectionReads> = Arc::new(CordisProjectionReads {
            live: projection_registry,
            cache: context.get(SESSION_PROJECTION_CACHE),
        });
        Ok(Self::new(
            sessions,
            agents,
            persistence,
            projections,
            options,
            domains,
        ))
    }

    /// Composes explicit services for alternate hosts and differential tests.
    #[must_use]
    pub fn new(
        sessions: Arc<SessionStore>,
        agents: Arc<AgentRegistry>,
        persistence: Option<Arc<dyn SessionPersistence>>,
        projections: Arc<dyn SessionProjectionReads>,
        options: SessionApiProxyOptions,
        domains: Arc<dyn ApiProxyRuntime>,
    ) -> Arc<Self> {
        Arc::new(Self {
            sessions,
            agents,
            persistence,
            projections,
            options,
            domains,
        })
    }

    async fn session_unary(
        &self,
        method: RpcMethod,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<RpcResponse<Value>> {
        match method {
            RpcMethod::SessionList => self.list(request).await,
            _ => self.domains.unary(method, request, signal).await,
        }
    }

    async fn list(&self, request: RpcRequest<Value>) -> anyhow::Result<RpcResponse<Value>> {
        let mut items = self
            .sessions
            .list()
            .iter()
            .map(|session| self.summarize_live(session))
            .collect::<Vec<_>>();
        let attached = items
            .iter()
            .map(|summary| summary.session_id.clone())
            .collect::<std::collections::HashSet<_>>();
        if let Some(persistence) = &self.persistence {
            let cold = persistence
                .list(None)
                .await?
                .into_iter()
                .filter(|meta| !attached.contains(&meta.id) && meta.cwd.is_some())
                .collect::<Vec<_>>();
            for batch in cold.chunks(COLD_SUMMARY_BATCH_SIZE) {
                let summaries = futures::future::join_all(
                    batch.iter().cloned().map(|meta| self.summarize_cold(meta)),
                )
                .await;
                items.extend(summaries);
            }
        }
        items.sort_by(|left, right| right.updated_at.total_cmp(&left.updated_at));
        let value = serde_json::to_value(SessionListValue { items })?;
        Ok(RpcResponse::new(
            request.rpc_id,
            RpcResult::Success { value: Some(value) },
        ))
    }

    fn summarize_live(&self, session: &Arc<Session>) -> SessionSummary {
        let events = session.events();
        let metadata = fold_list_metadata(&events);
        let agent = self.agents.get(session.id());
        let projections = self
            .projections
            .live_snapshot(session)
            .ok()
            .flatten()
            .map(projection_block);
        summary(
            session.header(),
            &events,
            agent.is_some_and(|agent| agent.status() == AgentStatus::Running),
            metadata.blank,
            metadata.last_prompt_at,
            projections,
        )
    }

    async fn summarize_cold(&self, meta: SessionHeader) -> SessionSummary {
        let projections = self.projections.cached_snapshot(&meta).ok().flatten();
        let cached_metadata = projections
            .as_ref()
            .and_then(|snapshot| snapshot.values.get("sessionListMetadata"))
            .and_then(|value| SessionListMetadata::parse(value).ok());
        let probed = if cached_metadata.as_ref().is_some_and(|value| !value.blank) {
            None
        } else {
            self.probe_cold_metadata(&meta).await
        };
        if let Some(live) = self.sessions.get(&meta.id) {
            return self.summarize_live(&live);
        }
        let recency = probed
            .as_ref()
            .or(cached_metadata.as_ref())
            .and_then(|metadata| metadata.last_prompt_at);
        let blank = if cached_metadata.as_ref().is_some_and(|value| !value.blank) {
            false
        } else {
            probed.as_ref().is_some_and(|metadata| metadata.blank)
        };
        summary(
            &meta,
            &[],
            false,
            blank,
            recency,
            projections.map(projection_block),
        )
    }

    async fn probe_cold_metadata(&self, meta: &SessionHeader) -> Option<SessionListMetadata> {
        let maximum = self
            .options
            .cold_blank_probe_max_bytes
            .unwrap_or(DEFAULT_COLD_BLANK_PROBE_MAX_BYTES);
        if maximum == 0 {
            return None;
        }
        let persistence = self.persistence.as_ref()?;
        let location = persistence.locate(meta)?;
        let size = tokio::fs::metadata(&location.path).await.ok()?.len();
        if size > maximum {
            return None;
        }
        match persistence.read_from(&meta.id, 0, None).await {
            Ok(inspection) => Some(fold_list_metadata(&inspection.events)),
            Err(error) => {
                tracing::warn!(session = %meta.id, %error, "session.list cold blank probe failed; serving the row as visible");
                None
            }
        }
    }
}

impl ApiProxyRuntime for SessionApiProxyRuntime {
    fn unary(
        &self,
        method: RpcMethod,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcResponse<Value>>> {
        let runtime = Arc::new(Self {
            sessions: self.sessions.clone(),
            agents: self.agents.clone(),
            persistence: self.persistence.clone(),
            projections: self.projections.clone(),
            options: self.options,
            domains: self.domains.clone(),
        });
        async move { runtime.session_unary(method, request, signal).await }.boxed()
    }

    fn respond(
        &self,
        message: ClientResponse,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcReceipt>> {
        self.domains.respond(message, signal)
    }

    fn mux(&self, request: RpcRequest<Value>, signal: AbortSignal) -> ApiDownlinkStream<MuxFrame> {
        self.domains.mux(request, signal)
    }

    fn host(
        &self,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> ApiDownlinkStream<HostFrame> {
        self.domains.host(request, signal)
    }

    fn session_log(
        &self,
        query: SessionLogQuery,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<HttpResponse>> {
        self.domains.session_log(query, signal)
    }
}

fn fold_list_metadata(events: &[SessionEvent]) -> SessionListMetadata {
    let mut blank = true;
    let mut last_prompt_at = None;
    for event in events {
        if event.event_type == "turn/start" {
            blank = false;
        }
        if event.event_type == "user/message"
            && event
                .data
                .get("source")
                .and_then(|source| source.get("kind"))
                .and_then(Value::as_str)
                == Some("user")
        {
            last_prompt_at = Some(i64_wire_number(event.time));
        }
    }
    SessionListMetadata {
        blank,
        last_prompt_at,
    }
}

fn session_list_projection_definition() -> ProjectionDefinition {
    ProjectionDefinition::new(
        "sessionListMetadata",
        1,
        || {
            Ok(serde_json::to_value(SessionListMetadata {
                blank: true,
                last_prompt_at: None,
            })?)
        },
        |state, event| {
            let current = SessionListMetadata::parse(state)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            let mut next = current.clone();
            if event.event_type == "turn/start" {
                next.blank = false;
            }
            if event.event_type == "user/message"
                && event
                    .data
                    .get("source")
                    .and_then(|source| source.get("kind"))
                    .and_then(Value::as_str)
                    == Some("user")
            {
                next.last_prompt_at = Some(i64_wire_number(event.time));
            }
            if next == current {
                Ok(ProjectionTransition::Unchanged)
            } else {
                ProjectionTransition::changed(next)
            }
        },
        |state| {
            SessionListMetadata::parse(state)
                .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            Ok(state.clone())
        },
    )
}

fn summary(
    header: &SessionHeader,
    events: &[SessionEvent],
    running: bool,
    blank: bool,
    last_prompt_at: Option<f64>,
    projections: Option<SessionProjectionsBlock>,
) -> SessionSummary {
    let created_at = u64_wire_number(header.created_at);
    SessionSummary {
        session_id: header.id.clone(),
        updated_at: last_prompt_at.map_or(created_at, |last| created_at.max(last)),
        running,
        blank,
        parent_session_id: header.parent_session.clone(),
        origin: header.origin.map(|origin| match origin {
            SessionOrigin::Subagent => SessionSummaryOrigin::Subagent,
        }),
        cwd: header.cwd.clone(),
        agent_preset: resolved_agent_preset(header, events),
        projections,
    }
}

fn resolved_agent_preset(header: &SessionHeader, events: &[SessionEvent]) -> Option<String> {
    events
        .iter()
        .rev()
        .find_map(|event| {
            (event.event_type == "agent-preset/selected")
                .then(|| event.data.get("agentPreset").and_then(Value::as_str))
                .flatten()
                .map(ToOwned::to_owned)
        })
        .or_else(|| header.agent_preset.clone())
}

fn projection_block(snapshot: ProjectionSnapshot) -> SessionProjectionsBlock {
    SessionProjectionsBlock {
        as_of_seq: snapshot.as_of_seq,
        values: snapshot.values.into_iter().collect::<BTreeMap<_, _>>(),
    }
}

fn u64_wire_number(value: u64) -> f64 {
    serde_json::Number::from(value)
        .as_f64()
        .expect("every u64 has a finite JavaScript-number projection")
}

fn i64_wire_number(value: i64) -> f64 {
    serde_json::Number::from(value)
        .as_f64()
        .expect("every i64 has a finite JavaScript-number projection")
}
