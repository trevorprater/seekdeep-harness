//! Production Session-domain runtime, including live and cold listing semantics.

use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use futures::{FutureExt as _, future::BoxFuture};
use seekdeep_agent::{AGENTS, AgentRegistry, AgentStatus};
use seekdeep_client_connection::{HttpResponse, RpcError, RpcResult};
use seekdeep_cordis::Context;
use seekdeep_core::{
    session::{Session, SessionEvent, SessionHeader, SessionOrigin},
    session_store::{SESSIONS, SessionStore},
};
use seekdeep_llm::AbortSignal;
use seekdeep_session_persistence::{
    SESSION_PERSISTENCE, SessionPersistence, ensure_persistence_not_aborted,
};
use seekdeep_session_projection::{
    ProjectionDefinition, ProjectionSnapshot, ProjectionTransition, SESSION_PROJECTIONS,
    SessionProjectionRegistry,
};
use seekdeep_session_projection_cache::{SESSION_PROJECTION_CACHE, SessionProjectionCache};
use seekdeep_session_query::{
    SESSION_QUERY, SessionQueryError, SessionQueryErrorCode, SessionQueryService,
    cursor::SessionSearchCursor,
    types::{
        SessionEventMetadataFilter, SessionEventSurface,
        SessionSearchExecContext as QueryExecContext, SessionSearchRequest as QueryRequest,
    },
};
use serde_json::{Map, Value};

use crate::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, RpcMethod, RpcReceipt, RpcRequest,
    RpcResponse,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
        sessions::{
            SESSION_SEARCH_RESULT_LIMIT, SESSION_SEARCH_SNIPPET_MAX_CODE_POINTS,
            SessionListMetadata, SessionListValue, SessionProjectionsBlock, SessionSearchItem,
            SessionSearchRequest, SessionSearchValue, SessionSummary, SessionSummaryOrigin,
        },
    },
};

/// Source default for one cold blankness artifact probe.
pub const DEFAULT_COLD_BLANK_PROBE_MAX_BYTES: u64 = 1024;
const COLD_SUMMARY_BATCH_SIZE: usize = 16;
const SESSION_SEARCH_PROVIDER_CALL_LIMIT: usize = 100;

/// Session-domain runtime options.
#[derive(Clone, Default)]
pub struct SessionApiProxyOptions {
    /// Maximum physical artifact size eligible for a cold blankness read.
    pub cold_blank_probe_max_bytes: Option<u64>,
    /// Optional artifact-size boundary for alternate hosts and deterministic tests.
    pub artifact_metadata: Option<Arc<dyn ColdArtifactMetadata>>,
}

impl std::fmt::Debug for SessionApiProxyOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionApiProxyOptions")
            .field(
                "cold_blank_probe_max_bytes",
                &self.cold_blank_probe_max_bytes,
            )
            .field("has_artifact_metadata", &self.artifact_metadata.is_some())
            .finish()
    }
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

/// Physical artifact metadata boundary used by bounded cold blankness probes.
pub trait ColdArtifactMetadata: Send + Sync + 'static {
    /// Reads the artifact's observed physical byte length.
    fn size(&self, path: PathBuf) -> BoxFuture<'static, anyhow::Result<u64>>;
}

#[derive(Debug)]
struct FilesystemColdArtifactMetadata;

impl ColdArtifactMetadata for FilesystemColdArtifactMetadata {
    fn size(&self, path: PathBuf) -> BoxFuture<'static, anyhow::Result<u64>> {
        async move { Ok(tokio::fs::metadata(path).await?.len()) }.boxed()
    }
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
    query: Option<Arc<SessionQueryService>>,
    projections: Arc<dyn SessionProjectionReads>,
    artifact_metadata: Arc<dyn ColdArtifactMetadata>,
    options: SessionApiProxyOptions,
    domains: Arc<dyn ApiProxyRuntime>,
}

impl std::fmt::Debug for SessionApiProxyRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionApiProxyRuntime")
            .field("live_sessions", &self.sessions.list().len())
            .field("has_persistence", &self.persistence.is_some())
            .field("has_query", &self.query.is_some())
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
        let query = context.get(SESSION_QUERY);
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
            query,
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
        query: Option<Arc<SessionQueryService>>,
        projections: Arc<dyn SessionProjectionReads>,
        options: SessionApiProxyOptions,
        domains: Arc<dyn ApiProxyRuntime>,
    ) -> Arc<Self> {
        let artifact_metadata = options
            .artifact_metadata
            .clone()
            .unwrap_or_else(|| Arc::new(FilesystemColdArtifactMetadata));
        Arc::new(Self {
            sessions,
            agents,
            persistence,
            query,
            projections,
            artifact_metadata,
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
            RpcMethod::SessionSearch => self.search(request, signal).await,
            _ => self.domains.unary(method, request, signal).await,
        }
    }

    async fn list(&self, request: RpcRequest<Value>) -> anyhow::Result<RpcResponse<Value>> {
        let items = self.list_visible_summaries(None).await?;
        let value = serde_json::to_value(SessionListValue { items })?;
        Ok(RpcResponse::new(
            request.rpc_id,
            RpcResult::Success { value: Some(value) },
        ))
    }

    async fn list_visible_summaries(
        &self,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Vec<SessionSummary>> {
        ensure_persistence_not_aborted(signal.as_ref())?;
        let mut items = self
            .sessions
            .list()
            .iter()
            .map(|session| self.summarize_live(session))
            .collect::<Vec<_>>();
        ensure_persistence_not_aborted(signal.as_ref())?;
        let attached = items
            .iter()
            .map(|summary| summary.session_id.clone())
            .collect::<std::collections::HashSet<_>>();
        if let Some(persistence) = &self.persistence {
            let cold = persistence
                .list(signal.clone())
                .await?
                .into_iter()
                .filter(|meta| !attached.contains(&meta.id) && meta.cwd.is_some())
                .collect::<Vec<_>>();
            for batch in cold.chunks(COLD_SUMMARY_BATCH_SIZE) {
                let summaries = futures::future::join_all(
                    batch
                        .iter()
                        .cloned()
                        .map(|meta| self.summarize_cold(meta, signal.clone())),
                )
                .await;
                for summary in summaries {
                    items.push(summary?);
                }
                ensure_persistence_not_aborted(signal.as_ref())?;
            }
        }
        items.sort_by(|left, right| right.updated_at.total_cmp(&left.updated_at));
        Ok(items)
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

    async fn summarize_cold(
        &self,
        meta: SessionHeader,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<SessionSummary> {
        let projections = self.projections.cached_snapshot(&meta).ok().flatten();
        let cached_metadata = projections
            .as_ref()
            .and_then(|snapshot| snapshot.values.get("sessionListMetadata"))
            .and_then(|value| SessionListMetadata::parse(value).ok());
        let probed = if cached_metadata.as_ref().is_some_and(|value| !value.blank) {
            None
        } else {
            self.probe_cold_metadata(&meta, signal.clone()).await?
        };
        ensure_persistence_not_aborted(signal.as_ref())?;
        if let Some(live) = self.sessions.get(&meta.id) {
            return Ok(self.summarize_live(&live));
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
        Ok(summary(
            &meta,
            &[],
            false,
            blank,
            recency,
            projections.map(projection_block),
        ))
    }

    async fn probe_cold_metadata(
        &self,
        meta: &SessionHeader,
        signal: Option<AbortSignal>,
    ) -> anyhow::Result<Option<SessionListMetadata>> {
        let maximum = self
            .options
            .cold_blank_probe_max_bytes
            .unwrap_or(DEFAULT_COLD_BLANK_PROBE_MAX_BYTES);
        if maximum == 0 {
            return Ok(None);
        }
        ensure_persistence_not_aborted(signal.as_ref())?;
        let Some(persistence) = self.persistence.as_ref() else {
            return Ok(None);
        };
        let Some(location) = persistence.locate(meta) else {
            return Ok(None);
        };
        let Ok(size) = self.artifact_metadata.size(location.path).await else {
            ensure_persistence_not_aborted(signal.as_ref())?;
            return Ok(None);
        };
        if size > maximum {
            return Ok(None);
        }
        match persistence.read_from(&meta.id, 0, signal.clone()).await {
            Ok(inspection) => {
                ensure_persistence_not_aborted(signal.as_ref())?;
                Ok(Some(fold_list_metadata(&inspection.events)))
            }
            Err(error) => {
                ensure_persistence_not_aborted(signal.as_ref())?;
                tracing::warn!(session = %meta.id, %error, "session.list cold blank probe failed; serving the row as visible");
                Ok(None)
            }
        }
    }

    async fn search(
        &self,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<RpcResponse<Value>> {
        if signal.is_aborted() {
            return Ok(search_cancelled(request));
        }
        let payload: SessionSearchRequest = serde_json::from_value(request.payload.clone())?;
        if self.query.is_none() {
            return Ok(search_failure(
                request,
                "internal",
                "session search is unavailable: this deployment does not mount seekdeep-session-query",
            ));
        }
        match self.search_inner(payload, signal.clone()).await {
            Ok(value) => Ok(RpcResponse::new(
                request.rpc_id,
                RpcResult::Success {
                    value: Some(serde_json::to_value(value)?),
                },
            )),
            Err(error) if search_was_cancelled(&error, &signal) => Ok(search_cancelled(request)),
            Err(error) => Ok(search_failure(
                request,
                "internal",
                format!("session search failed: {error}"),
            )),
        }
    }

    async fn search_inner(
        &self,
        request: SessionSearchRequest,
        signal: AbortSignal,
    ) -> anyhow::Result<SessionSearchValue> {
        let visible = self.list_visible_summaries(Some(signal.clone())).await?;
        ensure_persistence_not_aborted(Some(&signal))?;
        if visible.is_empty() {
            return Ok(SessionSearchValue {
                items: Vec::new(),
                has_more: false,
            });
        }
        let visible_ids = visible
            .into_iter()
            .map(|summary| summary.session_id)
            .collect::<HashSet<_>>();
        let query = self.query.as_ref().expect("search requires query service");
        let mut authorized = Vec::new();
        let mut accepted_ids = HashSet::new();
        let mut seen_cursors = HashSet::new();
        let mut cursor: Option<SessionSearchCursor> = None;
        let mut provider_calls = 0;
        let mut provider_page_limit = SESSION_SEARCH_RESULT_LIMIT;
        while authorized.len() <= SESSION_SEARCH_RESULT_LIMIT {
            ensure_persistence_not_aborted(Some(&signal))?;
            anyhow::ensure!(
                provider_calls < SESSION_SEARCH_PROVIDER_CALL_LIMIT,
                "session search provider exceeded the {SESSION_SEARCH_PROVIDER_CALL_LIMIT}-call work budget"
            );
            provider_calls += 1;
            let requested_cursor = cursor.clone();
            let requested_page_limit = provider_page_limit;
            let result = query
                .search_sessions(
                    QueryRequest {
                        query: request.query.clone(),
                        session_filters: None,
                        event_filters: Some(vec![
                            SessionEventMetadataFilter::Type {
                                values: vec![
                                    "user/message".to_owned(),
                                    "assistant/message".to_owned(),
                                ],
                            },
                            SessionEventMetadataFilter::Surface {
                                values: vec![SessionEventSurface::Current],
                            },
                        ]),
                        limit: Some(u64::try_from(requested_page_limit).unwrap_or(u64::MAX)),
                        cursor: requested_cursor.clone(),
                    },
                    Some(QueryExecContext {
                        signal: Some(signal.clone()),
                    }),
                )
                .await;
            let page = match result {
                Ok(page) => page,
                Err(error)
                    if requested_cursor.is_none()
                        && query_error_code(&error)
                            == Some(SessionQueryErrorCode::SessionQueryInvalidLimit)
                        && requested_page_limit > 1 =>
                {
                    provider_page_limit = (requested_page_limit / 2).max(1);
                    continue;
                }
                Err(error)
                    if requested_cursor.is_some()
                        && query_error_code(&error)
                            == Some(SessionQueryErrorCode::SessionQueryStaleCursor) =>
                {
                    authorized.clear();
                    accepted_ids.clear();
                    seen_cursors.clear();
                    cursor = None;
                    continue;
                }
                Err(error) => return Err(error),
            };
            ensure_persistence_not_aborted(Some(&signal))?;
            anyhow::ensure!(
                page.items.len() <= requested_page_limit,
                "session search provider returned {} items; maximum is {requested_page_limit}",
                page.items.len()
            );
            append_authorized_hits(page.items, &visible_ids, &mut accepted_ids, &mut authorized);
            if let Some(next) = &page.next_cursor
                && !seen_cursors.insert(next.clone())
            {
                anyhow::bail!("session search provider repeated a continuation cursor");
            }
            if authorized.len() > SESSION_SEARCH_RESULT_LIMIT || page.next_cursor.is_none() {
                break;
            }
            cursor = page.next_cursor;
        }
        Ok(finish_search(authorized))
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
            query: self.query.clone(),
            projections: self.projections.clone(),
            artifact_metadata: self.artifact_metadata.clone(),
            options: self.options.clone(),
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

fn query_error_code(error: &anyhow::Error) -> Option<SessionQueryErrorCode> {
    error
        .downcast_ref::<SessionQueryError>()
        .map(|error| error.code)
}

fn search_was_cancelled(error: &anyhow::Error, signal: &AbortSignal) -> bool {
    signal.is_aborted()
        || query_error_code(error) == Some(SessionQueryErrorCode::SessionQueryAborted)
        || error
            .downcast_ref::<seekdeep_session_persistence::SessionPersistenceAborted>()
            .is_some()
}

fn search_cancelled(request: RpcRequest<Value>) -> RpcResponse<Value> {
    search_failure(request, "cancelled", "session search was aborted")
}

fn search_failure(
    request: RpcRequest<Value>,
    code: impl Into<String>,
    message: impl Into<String>,
) -> RpcResponse<Value> {
    RpcResponse::new(
        request.rpc_id,
        RpcResult::Failure {
            error: RpcError {
                code: code.into(),
                message: message.into(),
                details: Map::new(),
            },
        },
    )
}

fn truncate_code_points(value: &str, maximum: usize) -> String {
    value.chars().take(maximum).collect()
}

fn append_authorized_hits(
    hits: Vec<seekdeep_session_query::SessionSearchHit>,
    visible_ids: &HashSet<seekdeep_core::session::SessionId>,
    accepted_ids: &mut HashSet<seekdeep_core::session::SessionId>,
    authorized: &mut Vec<SessionSearchItem>,
) {
    for hit in hits {
        if authorized.len() > SESSION_SEARCH_RESULT_LIMIT {
            continue;
        }
        let session_id = &hit.record.header.id;
        if !visible_ids.contains(session_id)
            || hit.best_match.record.session_id != *session_id
            || hit.best_match.record.surface != SessionEventSurface::Current
            || !matches!(
                hit.best_match.record.event_type.as_str(),
                "user/message" | "assistant/message"
            )
            || accepted_ids.contains(session_id)
        {
            continue;
        }
        accepted_ids.insert(session_id.clone());
        authorized.push(SessionSearchItem {
            session_id: session_id.clone(),
            snippet: truncate_code_points(
                &hit.best_match.snippet,
                SESSION_SEARCH_SNIPPET_MAX_CODE_POINTS,
            ),
        });
    }
}

fn finish_search(mut authorized: Vec<SessionSearchItem>) -> SessionSearchValue {
    let has_more = authorized.len() > SESSION_SEARCH_RESULT_LIMIT;
    authorized.truncate(SESSION_SEARCH_RESULT_LIMIT);
    SessionSearchValue {
        items: authorized,
        has_more,
    }
}
