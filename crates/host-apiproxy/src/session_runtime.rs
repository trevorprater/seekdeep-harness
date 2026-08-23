//! Production Session-domain runtime, including live and cold listing semantics.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
    task::{Context as TaskContext, Poll},
};

use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_agent::{
    AGENTS, Agent, AgentOptions, AgentRegistry, AgentSetup, AgentStatus, CreateAgentMeta,
    CreateAgentOptions,
};
use seekdeep_agent_presets::{
    AGENT_PRESETS, PresetMountError, UnknownPresetError, resolve_session_preset,
};
use seekdeep_api_remotes::{
    ApiRemoteLookupError, ApiRemoteSubagentSessionOwnership, api_remote_subagent_ownership_error,
    has_api_remote_subagent_owner,
};
use seekdeep_attachment::ATTACHMENTS;
use seekdeep_client_connection::{HttpResponse, RpcError, RpcResult};
use seekdeep_cordis::{Context, EventOptions, EventReply, fiber::EffectHandle};
use seekdeep_core::{
    session::{Session, SessionEvent, SessionHeader, SessionId, SessionOrigin, SurfaceOp},
    session_store::{SESSIONS, SessionStore},
};
use seekdeep_llm::{AbortSignal, ContentBlock, Message, ModelId, ProviderId};
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
use seekdeep_tools::{TOOLS, ToolResult, ToolRuntime};
use seekdeep_workspace::WORKSPACE_REGISTRY;
use serde_json::{Map, Value};

use crate::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, DefaultModelSelection, RpcMethod,
    RpcReceipt, RpcRequest, RpcResponse,
    api::{
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
        sessions::{
            HistoryEntry, SESSION_SEARCH_RESULT_LIMIT, SESSION_SEARCH_SNIPPET_MAX_CODE_POINTS,
            SessionCreateRequest, SessionCreateValue, SessionHistoryRequest, SessionHistoryValue,
            SessionListMetadata, SessionListValue, SessionProjectionsBlock, SessionSearchItem,
            SessionSearchRequest, SessionSearchValue, SessionSummary, SessionSummaryOrigin,
            ToolEventView, ToolEventViewTarget,
        },
    },
};

/// Source default for one cold blankness artifact probe.
pub const DEFAULT_COLD_BLANK_PROBE_MAX_BYTES: u64 = 1024;
const COLD_SUMMARY_BATCH_SIZE: usize = 16;
const SESSION_SEARCH_PROVIDER_CALL_LIMIT: usize = 100;
static NEXT_SESSION_FRAME_ID: AtomicU64 = AtomicU64::new(1);

/// Session-domain runtime options.
#[derive(Clone, Default)]
pub struct SessionApiProxyOptions {
    /// Maximum physical artifact size eligible for a cold blankness read.
    pub cold_blank_probe_max_bytes: Option<u64>,
    /// Optional artifact-size boundary for alternate hosts and deterministic tests.
    pub artifact_metadata: Option<Arc<dyn ColdArtifactMetadata>>,
    /// Default project directory for a create request that names no source.
    pub default_cwd: Option<String>,
    /// Live model default used by newly created and resumed Agents.
    pub default_model_selection: Option<DefaultModelSelection>,
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
            .field("default_cwd", &self.default_cwd)
            .field(
                "has_default_model_selection",
                &self.default_model_selection.is_some(),
            )
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

    /// Folds a detached projection baseline over one exact event cut.
    ///
    /// # Errors
    ///
    /// Returns a projection initialization, fold, or view failure.
    fn snapshot_for_events(
        &self,
        events: &[SessionEvent],
    ) -> anyhow::Result<Option<ProjectionSnapshot>>;
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

    fn snapshot_for_events(
        &self,
        events: &[SessionEvent],
    ) -> anyhow::Result<Option<ProjectionSnapshot>> {
        self.live
            .as_ref()
            .map(|registry| {
                registry
                    .restore(&indexmap::IndexMap::new(), events, 0)
                    .map(|restored| restored.snapshot)
            })
            .transpose()
    }
}

/// Services consumed by the Session API Proxy domain.
pub struct SessionApiProxyServices {
    /// Lifecycle and event context for this gateway generation.
    pub context: Context,
    /// Authoritative live Session registry.
    pub sessions: Arc<SessionStore>,
    /// Exact live Agent registry.
    pub agents: Arc<AgentRegistry>,
    /// Optional cold Session persistence.
    pub persistence: Option<Arc<dyn SessionPersistence>>,
    /// Optional indexed Session query provider.
    pub query: Option<Arc<SessionQueryService>>,
    /// Optional live and cached projection reads.
    pub projections: Arc<dyn SessionProjectionReads>,
    /// Optional live projection change source.
    pub projection_registry: Option<Arc<SessionProjectionRegistry>>,
    /// Optional replay-safe tool presenters.
    pub tools: Option<Arc<ToolRuntime>>,
}

impl std::fmt::Debug for SessionApiProxyServices {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionApiProxyServices")
            .field("live_sessions", &self.sessions.list().len())
            .field("has_persistence", &self.persistence.is_some())
            .field("has_query", &self.query.is_some())
            .field("has_tools", &self.tools.is_some())
            .field(
                "has_projection_registry",
                &self.projection_registry.is_some(),
            )
            .finish_non_exhaustive()
    }
}

/// Session-domain decorator over the remaining API Proxy domains.
pub struct SessionApiProxyRuntime {
    context: Context,
    sessions: Arc<SessionStore>,
    agents: Arc<AgentRegistry>,
    persistence: Option<Arc<dyn SessionPersistence>>,
    query: Option<Arc<SessionQueryService>>,
    projections: Arc<dyn SessionProjectionReads>,
    projection_registry: Option<Arc<SessionProjectionRegistry>>,
    tools: Option<Arc<ToolRuntime>>,
    artifact_metadata: Arc<dyn ColdArtifactMetadata>,
    options: SessionApiProxyOptions,
    creation_locks: Arc<Mutex<HashMap<SessionId, Arc<tokio::sync::Mutex<()>>>>>,
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
            if let Some(attachments) = context.get(ATTACHMENTS) {
                let limits = attachments.image_limits().clone();
                registry.register(
                    context,
                    ProjectionDefinition::new(
                        "imageLimits",
                        1,
                        || Ok(Value::Null),
                        |_, _| Ok(ProjectionTransition::Unchanged),
                        move |_| Ok(serde_json::to_value(&limits)?),
                    ),
                )?;
            }
        }
        let projections: Arc<dyn SessionProjectionReads> = Arc::new(CordisProjectionReads {
            live: projection_registry.clone(),
            cache: context.get(SESSION_PROJECTION_CACHE),
        });
        Ok(Self::new(
            SessionApiProxyServices {
                context: context.clone(),
                sessions,
                agents,
                persistence,
                query,
                projections,
                projection_registry: projection_registry.clone(),
                tools: context.get(TOOLS),
            },
            options,
            domains,
        ))
    }

    /// Composes explicit services for alternate hosts and differential tests.
    #[must_use]
    pub fn new(
        services: SessionApiProxyServices,
        options: SessionApiProxyOptions,
        domains: Arc<dyn ApiProxyRuntime>,
    ) -> Arc<Self> {
        let artifact_metadata = options
            .artifact_metadata
            .clone()
            .unwrap_or_else(|| Arc::new(FilesystemColdArtifactMetadata));
        Arc::new(Self {
            context: services.context,
            sessions: services.sessions,
            agents: services.agents,
            persistence: services.persistence,
            query: services.query,
            projections: services.projections,
            projection_registry: services.projection_registry,
            tools: services.tools,
            artifact_metadata,
            options,
            creation_locks: Arc::new(Mutex::new(HashMap::new())),
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
            RpcMethod::SessionCreate => self.create(request, signal).await,
            RpcMethod::SessionList => self.list(request).await,
            RpcMethod::SessionSearch => self.search(request, signal).await,
            RpcMethod::SessionHistory => self.history(request).await,
            _ => self.domains.unary(method, request, signal).await,
        }
    }

    async fn create(
        &self,
        request: RpcRequest<Value>,
        _signal: AbortSignal,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: SessionCreateRequest = serde_json::from_value(request.payload.clone())?;
        let session_id = payload
            .session_id
            .clone()
            .unwrap_or_else(|| SessionId::new(format!("session-{}", uuid::Uuid::new_v4())));
        let workspace = if let Some(workspace_id) = &payload.workspace_id {
            let Some(workspace) = self.context.get(WORKSPACE_REGISTRY).and_then(|registry| {
                registry.get(&seekdeep_workspace::WorkspaceId::new(workspace_id.as_str()))
            }) else {
                return Ok(session_failure(
                    request,
                    "workspace-not-found",
                    format!("workspace \"{workspace_id}\" not found"),
                    Map::from_iter([(
                        "workspaceId".to_owned(),
                        Value::String(workspace_id.to_string()),
                    )]),
                ));
            };
            Some(workspace)
        } else {
            None
        };
        let cwd = workspace.as_ref().map_or_else(
            || {
                payload
                    .cwd
                    .clone()
                    .or_else(|| self.options.default_cwd.clone())
                    .unwrap_or_default()
            },
            |workspace| workspace.path(),
        );
        let agent = match self
            .ensure_session(
                session_id.clone(),
                cwd,
                payload.session_id.is_some(),
                payload.agent_preset.as_deref(),
            )
            .await
        {
            Ok(agent) => agent,
            Err(error) => return Ok(create_failure(request, &session_id, &error)),
        };
        if let Some(workspace) = workspace
            && let Err(error) = workspace.attach_session(session_id.clone()).await
        {
            return Ok(session_failure(
                request,
                "workspace-attach-failed",
                format!(
                    "session \"{session_id}\" was created but could not attach to workspace \"{}\": {error}",
                    workspace.id()
                ),
                Map::from_iter([
                    (
                        "sessionId".to_owned(),
                        Value::String(session_id.to_string()),
                    ),
                    (
                        "workspaceId".to_owned(),
                        Value::String(workspace.id().to_string()),
                    ),
                ]),
            ));
        }
        let agent_preset =
            resolve_session_preset(agent.session().header(), &agent.session().events());
        let value = serde_json::to_value(SessionCreateValue {
            session_id,
            agent_preset,
        })?;
        Ok(RpcResponse::new(
            request.rpc_id,
            RpcResult::Success { value: Some(value) },
        ))
    }

    async fn ensure_session(
        &self,
        session_id: SessionId,
        cwd: String,
        check_persisted_identity: bool,
        requested_preset: Option<&str>,
    ) -> anyhow::Result<Arc<Agent>> {
        let lock = {
            let mut locks = self.creation_locks.lock();
            locks
                .entry(session_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let guard = lock.lock().await;
        let result = self
            .ensure_session_locked(
                &session_id,
                &cwd,
                check_persisted_identity,
                requested_preset,
            )
            .await;
        drop(guard);
        let mut locks = self.creation_locks.lock();
        if Arc::strong_count(&lock) == 2
            && locks
                .get(&session_id)
                .is_some_and(|current| Arc::ptr_eq(current, &lock))
        {
            locks.remove(&session_id);
        }
        result
    }

    async fn ensure_session_locked(
        &self,
        session_id: &SessionId,
        cwd: &str,
        check_persisted_identity: bool,
        requested_preset: Option<&str>,
    ) -> anyhow::Result<Arc<Agent>> {
        let attached = self.sessions.get(session_id);
        let live = self.agents.get(session_id);
        if attached.as_ref().is_some_and(|session| {
            has_api_remote_subagent_owner(&self.context, session.header(), live.as_ref())
        }) {
            return Err(ApiRemoteSubagentSessionOwnership {
                session_id: session_id.clone(),
            }
            .into());
        }
        if let Some(agent) = live {
            return self.validate_adoption(agent, cwd, requested_preset);
        }

        if check_persisted_identity
            && let Some(persistence) = &self.persistence
            && persistence
                .list(None)
                .await?
                .iter()
                .any(|meta| meta.id == *session_id)
        {
            let inspected = persistence.inspect(session_id, None).await?;
            if has_api_remote_subagent_owner(&self.context, &inspected.meta, None) {
                return Err(ApiRemoteSubagentSessionOwnership {
                    session_id: session_id.clone(),
                }
                .into());
            }
            if inspected.meta.cwd.as_deref() != Some(cwd) {
                return Err(
                    SessionCwdConflict::new(session_id.clone(), cwd, inspected.meta.cwd).into(),
                );
            }
            let stored_preset = resolve_session_preset(&inspected.meta, &inspected.events);
            assert_preset_unchanged(session_id, requested_preset, stored_preset.as_deref())?;
            let (_, setup) = self.compose_agent(stored_preset.as_deref()).await?;
            let mut options = seekdeep_agent::ResumeAgentOptions::new(session_id.clone());
            options.agent_options = self.default_agent_options()?;
            options.setup = setup;
            let resumed = self.agents.resume(options).await.map(|handle| handle.agent);
            return match resumed {
                Ok(agent) => self.validate_adoption(agent, cwd, requested_preset),
                Err(error) => self.recover_creation_race(session_id, cwd, requested_preset, error),
            };
        }

        tokio::fs::create_dir_all(cwd).await.map_err(|error| {
            anyhow::anyhow!("failed to ensure project directory \"{cwd}\": {error}")
        })?;
        let (agent_preset, setup) = self.compose_agent(requested_preset).await?;
        let options = CreateAgentOptions {
            session_id: session_id.clone(),
            meta: CreateAgentMeta {
                cwd: Some(cwd.to_owned()),
                agent_preset,
                ..CreateAgentMeta::default()
            },
            seed: None,
            agent_options: self.default_agent_options()?,
            signal: None,
            setup,
            owner_agent: None,
        };
        match self.agents.create(options).await.map(|handle| handle.agent) {
            Ok(agent) => self.validate_adoption(agent, cwd, requested_preset),
            Err(error) => self.recover_creation_race(session_id, cwd, requested_preset, error),
        }
    }

    async fn compose_agent(
        &self,
        requested_preset: Option<&str>,
    ) -> anyhow::Result<(Option<String>, Option<AgentSetup>)> {
        let Some(roster) = self.context.get(AGENT_PRESETS) else {
            return Ok((None, None));
        };
        let preset = roster.resolve(requested_preset).await?;
        let preset_id = preset.id;
        let setup_roster = roster.clone();
        let setup_id = preset_id.clone();
        let setup: AgentSetup = Arc::new(move |agent_context| {
            let roster = setup_roster.clone();
            let preset_id = setup_id.clone();
            Box::pin(async move {
                roster.mount(&agent_context, Some(&preset_id)).await?;
                Ok(None)
            })
        });
        Ok((Some(preset_id), Some(setup)))
    }

    fn default_agent_options(&self) -> anyhow::Result<AgentOptions> {
        let defaults = self
            .options
            .default_model_selection
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("session.create requires default model selection"))?;
        let selection = defaults();
        Ok(AgentOptions {
            provider: Some(ProviderId::new(selection.provider)),
            model: Some(ModelId::new(selection.model)),
            ..AgentOptions::default()
        })
    }

    fn recover_creation_race(
        &self,
        session_id: &SessionId,
        cwd: &str,
        requested_preset: Option<&str>,
        error: anyhow::Error,
    ) -> anyhow::Result<Arc<Agent>> {
        if let Some(agent) = self.agents.get(session_id) {
            return self.validate_adoption(agent, cwd, requested_preset);
        }
        if let Some(session) = self.sessions.get(session_id)
            && has_api_remote_subagent_owner(&self.context, session.header(), None)
        {
            return Err(ApiRemoteSubagentSessionOwnership {
                session_id: session_id.clone(),
            }
            .into());
        }
        Err(error)
    }

    fn validate_adoption(
        &self,
        agent: Arc<Agent>,
        cwd: &str,
        requested_preset: Option<&str>,
    ) -> anyhow::Result<Arc<Agent>> {
        if has_api_remote_subagent_owner(&self.context, agent.session().header(), Some(&agent)) {
            return Err(ApiRemoteSubagentSessionOwnership {
                session_id: agent.id().clone(),
            }
            .into());
        }
        let existing_preset =
            resolve_session_preset(agent.session().header(), &agent.session().events());
        assert_preset_unchanged(agent.id(), requested_preset, existing_preset.as_deref())?;
        if agent.session().header().cwd.as_deref() != Some(cwd) {
            return Err(SessionCwdConflict::new(
                agent.id().clone(),
                cwd,
                agent.session().header().cwd.clone(),
            )
            .into());
        }
        Ok(agent)
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

    async fn history(&self, request: RpcRequest<Value>) -> anyhow::Result<RpcResponse<Value>> {
        let payload: SessionHistoryRequest = serde_json::from_value(request.payload.clone())?;
        let source = match self.history_source(&payload.session_id).await {
            Ok(source) => source,
            Err(error) if error.downcast_ref::<HistoryNotFound>().is_some() => {
                return Ok(session_failure(
                    request,
                    "session-not-found",
                    error.to_string(),
                    Map::from_iter([(
                        "sessionId".to_owned(),
                        Value::String(payload.session_id.to_string()),
                    )]),
                ));
            }
            Err(error) => {
                return Ok(session_failure(
                    request,
                    "internal",
                    format!(
                        "history unavailable for session \"{}\": {error}",
                        payload.session_id
                    ),
                    Map::new(),
                ));
            }
        };
        let scope = if let Some(agent) = self.agents.get(&payload.session_id) {
            Some(agent.scope_key())
        } else if let Some(roster) = self.context.get(AGENT_PRESETS) {
            let preset = resolve_session_preset(&source.header, &source.events);
            roster.standing_key_for(preset.as_deref()).await.ok()
        } else {
            None
        };
        let projections = if payload.before_seq.is_none() {
            self.projections
                .snapshot_for_events(&source.events)
                .ok()
                .flatten()
                .map(projection_block)
        } else {
            None
        };
        let (events, has_more) = paginate_history(
            &source.events,
            payload.before_seq,
            payload.max_messages.unwrap_or(50),
        );
        let entries = events
            .iter()
            .map(|event| -> anyhow::Result<HistoryEntry> {
                Ok(HistoryEntry {
                    event: serde_json::from_value(serde_json::to_value(event)?)?,
                    view: self.history_view(event, &events, scope),
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let value = serde_json::to_value(SessionHistoryValue {
            events: entries,
            has_more,
            projections,
        })?;
        Ok(RpcResponse::new(
            request.rpc_id,
            RpcResult::Success { value: Some(value) },
        ))
    }

    async fn history_source(&self, session_id: &SessionId) -> anyhow::Result<HistorySource> {
        if let Some(session) = self.sessions.get(session_id) {
            return Ok(HistorySource {
                header: session.header().clone(),
                events: session.events(),
            });
        }
        let persistence = self.persistence.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "session persistence is not configured (load a seekdeep-session-persistence backend)"
            )
        })?;
        let listed = persistence.list(None).await?;
        if !listed
            .iter()
            .any(|meta| meta.id == *session_id && meta.cwd.is_some())
        {
            return Err(HistoryNotFound(session_id.clone()).into());
        }
        let inspected = persistence.inspect(session_id, None).await?;
        if inspected.meta.cwd.is_none() {
            return Err(HistoryNotFound(session_id.clone()).into());
        }
        Ok(HistorySource {
            header: inspected.meta,
            events: inspected.events,
        })
    }

    fn history_view(
        &self,
        event: &SessionEvent,
        page: &[SessionEvent],
        scope: Option<seekdeep_scope::ScopeKey>,
    ) -> Option<ToolEventView> {
        let tools = self.tools.as_ref()?;
        match event.event_type.as_str() {
            "tool/call" => {
                let name = event.data.get("name")?.as_str()?;
                let arguments: Value =
                    serde_json::from_str(event.data.get("arguments")?.as_str()?).ok()?;
                let definition = tools.get(name, scope)?;
                let presenter = definition.present_call.as_ref()?;
                let view = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    presenter(&arguments)
                }))
                .ok()??;
                Some(ToolEventView {
                    target: ToolEventViewTarget::Call,
                    view: serde_json::to_value(view).ok()?.as_object()?.clone(),
                })
            }
            "tool/result" => {
                let message: Message =
                    serde_json::from_value(event.data.get("message")?.clone()).ok()?;
                let call_id = message.source().fields.get("callId")?.as_str()?;
                let (name, arguments) = backscan_call(page, call_id)?;
                let ContentBlock::ToolResult {
                    content, is_error, ..
                } = message.content().first()?
                else {
                    return None;
                };
                let definition = tools.get(&name, scope)?;
                let presenter = definition.present_result.as_ref()?;
                let result = ToolResult {
                    content: content.clone(),
                    is_error: is_error.unwrap_or(false),
                    meta: event.data.get("meta").cloned(),
                };
                let view = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    presenter(&arguments, &result)
                }))
                .ok()??;
                Some(ToolEventView {
                    target: ToolEventViewTarget::Result,
                    view: serde_json::to_value(view).ok()?.as_object()?.clone(),
                })
            }
            _ => None,
        }
    }

    fn register_mux_listeners(
        self: &Arc<Self>,
        sender: &tokio::sync::mpsc::UnboundedSender<RpcRequest<MuxFrame>>,
        subscribed: &Arc<parking_lot::Mutex<HashSet<SessionId>>>,
    ) -> (Vec<EffectHandle>, anyhow::Result<()>) {
        let mut effects = Vec::new();
        let result = (|| -> anyhow::Result<()> {
            let runtime = self.clone();
            let event_sender = sender.clone();
            let event_subscribed = subscribed.clone();
            effects.push(self.context.events().on_sync(
                &self.context,
                "session/event",
                move |_, args| {
                    let session = args
                        .get::<Session>(0)
                        .ok_or_else(|| anyhow::anyhow!("session/event lacks a session"))?;
                    let event = args
                        .get::<SessionEvent>(1)
                        .ok_or_else(|| anyhow::anyhow!("session/event lacks an event"))?;
                    send_subscribed_if_new(&event_sender, &event_subscribed, &session);
                    let scope = runtime
                        .agents
                        .get(session.id())
                        .map(|agent| agent.scope_key());
                    let view = runtime.history_view(&event, &session.events(), scope);
                    let Ok(wire_event) = serde_json::from_value(serde_json::to_value(&*event)?)
                    else {
                        tracing::warn!(session = %session.id(), "API Proxy could not encode a committed Session event");
                        return Ok(EventReply::Undefined);
                    };
                    let _ = event_sender.send(RpcRequest::new(
                        next_session_frame_id(),
                        MuxFrame::SessionEvent {
                            session_id: session.id().clone(),
                            event: wire_event,
                            view,
                        },
                    ));
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )?);
            let created_sender = sender.clone();
            let created_subscribed = subscribed.clone();
            effects.push(self.context.events().on_sync(
                &self.context,
                "session/created",
                move |_, args| {
                    let session = args
                        .get::<Session>(0)
                        .ok_or_else(|| anyhow::anyhow!("session/created lacks a session"))?;
                    send_subscribed_if_new(&created_sender, &created_subscribed, &session);
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )?);
            let disposed_subscribed = subscribed.clone();
            effects.push(self.context.events().on_sync(
                &self.context,
                "session/disposed",
                move |_, args| {
                    if let Some(session) = args.get::<Session>(0) {
                        disposed_subscribed.lock().remove(session.id());
                    }
                    Ok(EventReply::Undefined)
                },
                EventOptions::default(),
            )?);
            if let Some(projections) = &self.projection_registry {
                let projection_sender = sender.clone();
                let projection_subscribed = subscribed.clone();
                effects.push(projections.on_changed(
                    &self.context,
                    Arc::new(move |session, key, value, seq| {
                        send_subscribed_if_new(
                            &projection_sender,
                            &projection_subscribed,
                            &session,
                        );
                        let _ = projection_sender.send(RpcRequest::new(
                            next_session_frame_id(),
                            MuxFrame::SessionProjection {
                                session_id: session.id().clone(),
                                key: key.to_owned(),
                                value: value.clone(),
                                seq,
                            },
                        ));
                        Ok(())
                    }),
                )?);
            }
            Ok(())
        })();
        (effects, result)
    }

    fn mux_stream(
        self: &Arc<Self>,
        signal: &AbortSignal,
        domains: ApiDownlinkStream<MuxFrame>,
    ) -> ApiDownlinkStream<MuxFrame> {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let subscribed = Arc::new(parking_lot::Mutex::new(HashSet::new()));
        let (effects, register) = self.register_mux_listeners(&sender, &subscribed);
        if let Err(error) = register {
            return SessionMuxStream {
                inner: futures::stream::once(async move { Err(error) }).boxed(),
                _guard: SessionMuxListenerGuard::new(effects),
            }
            .boxed();
        }
        let mut baseline = Vec::new();
        for session in self.sessions.list() {
            if subscribed.lock().insert(session.id().clone()) {
                baseline.push(subscribed_envelope(&session));
            }
        }
        let live_signal = signal.clone();
        let live = async_stream::stream! {
            loop {
                tokio::select! {
                    () = live_signal.cancelled() => break,
                    envelope = receiver.recv() => match envelope {
                        Some(envelope) => yield Ok(envelope),
                        None => break,
                    },
                }
            }
        };
        let mut tail = futures::stream::select(live.boxed(), domains);
        let inner = async_stream::stream! {
            for envelope in baseline {
                yield Ok(envelope);
            }
            while let Some(envelope) = tail.next().await {
                yield envelope;
            }
        };
        SessionMuxStream {
            inner: inner.boxed(),
            _guard: SessionMuxListenerGuard::new(effects),
        }
        .boxed()
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
            context: self.context.clone(),
            sessions: self.sessions.clone(),
            agents: self.agents.clone(),
            persistence: self.persistence.clone(),
            query: self.query.clone(),
            projections: self.projections.clone(),
            projection_registry: self.projection_registry.clone(),
            tools: self.tools.clone(),
            artifact_metadata: self.artifact_metadata.clone(),
            options: self.options.clone(),
            creation_locks: self.creation_locks.clone(),
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
        let runtime = Arc::new(Self {
            context: self.context.clone(),
            sessions: self.sessions.clone(),
            agents: self.agents.clone(),
            persistence: self.persistence.clone(),
            query: self.query.clone(),
            projections: self.projections.clone(),
            projection_registry: self.projection_registry.clone(),
            tools: self.tools.clone(),
            artifact_metadata: self.artifact_metadata.clone(),
            options: self.options.clone(),
            creation_locks: self.creation_locks.clone(),
            domains: self.domains.clone(),
        });
        let domains = self.domains.mux(request, signal.clone());
        runtime.mux_stream(&signal, domains)
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
        agent_preset: seekdeep_agent_presets::resolve_session_preset(header, events),
        projections,
    }
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

struct HistorySource {
    header: SessionHeader,
    events: Vec<SessionEvent>,
}

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
struct AgentPresetConflict {
    session_id: SessionId,
    requested_preset: String,
    existing_preset: Option<String>,
    message: String,
}

impl AgentPresetConflict {
    fn new(session_id: SessionId, requested_preset: &str, existing_preset: Option<String>) -> Self {
        let message = if let Some(existing) = &existing_preset {
            format!(
                "session \"{session_id}\" already runs agent preset {existing:?}; requested {requested_preset:?}. A session's preset is fixed at creation."
            )
        } else {
            format!(
                "session \"{session_id}\" records no agent preset, so it cannot be adopted under one; a deployment composing no roster records none on any session — requested {requested_preset:?}. A session's preset is fixed at creation."
            )
        };
        Self {
            session_id,
            requested_preset: requested_preset.to_owned(),
            existing_preset,
            message,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "session \"{session_id}\" already exists with cwd {existing_cwd:?}; requested {requested_cwd:?}"
)]
struct SessionCwdConflict {
    session_id: SessionId,
    requested_cwd: String,
    existing_cwd: Option<String>,
}

impl SessionCwdConflict {
    fn new(session_id: SessionId, requested_cwd: &str, existing_cwd: Option<String>) -> Self {
        Self {
            session_id,
            requested_cwd: requested_cwd.to_owned(),
            existing_cwd,
        }
    }
}

fn assert_preset_unchanged(
    session_id: &SessionId,
    requested: Option<&str>,
    existing: Option<&str>,
) -> anyhow::Result<()> {
    if let Some(requested) = requested
        && Some(requested) != existing
    {
        return Err(AgentPresetConflict::new(
            session_id.clone(),
            requested,
            existing.map(ToOwned::to_owned),
        )
        .into());
    }
    Ok(())
}

fn create_failure(
    request: RpcRequest<Value>,
    session_id: &SessionId,
    error: &anyhow::Error,
) -> RpcResponse<Value> {
    if let Some(error) = error.downcast_ref::<AgentPresetConflict>() {
        let mut details = Map::from_iter([
            (
                "sessionId".to_owned(),
                Value::String(error.session_id.to_string()),
            ),
            (
                "requestedPreset".to_owned(),
                Value::String(error.requested_preset.clone()),
            ),
        ]);
        if let Some(existing) = &error.existing_preset {
            details.insert("existingPreset".to_owned(), Value::String(existing.clone()));
        }
        return session_failure(request, "agent-preset-conflict", error.to_string(), details);
    }
    if let Some(error) = error.downcast_ref::<UnknownPresetError>() {
        return session_failure(
            request,
            "agent-preset-not-found",
            error.to_string(),
            Map::from_iter([
                (
                    "agentPreset".to_owned(),
                    Value::String(error.preset_id.clone()),
                ),
                (
                    "available".to_owned(),
                    Value::Array(error.available.iter().cloned().map(Value::String).collect()),
                ),
            ]),
        );
    }
    if let Some(error) = error.downcast_ref::<PresetMountError>() {
        return session_failure(
            request,
            "agent-preset-invalid",
            error.to_string(),
            Map::from_iter([
                (
                    "agentPreset".to_owned(),
                    Value::String(error.preset_id.clone()),
                ),
                ("reason".to_owned(), Value::String(error.reason.clone())),
            ]),
        );
    }
    if let Some(error) = error.downcast_ref::<SessionCwdConflict>() {
        let mut details = Map::from_iter([
            (
                "sessionId".to_owned(),
                Value::String(error.session_id.to_string()),
            ),
            (
                "requestedCwd".to_owned(),
                Value::String(error.requested_cwd.clone()),
            ),
        ]);
        if let Some(existing) = &error.existing_cwd {
            details.insert("existingCwd".to_owned(), Value::String(existing.clone()));
        }
        return session_failure(request, "session-conflict", error.to_string(), details);
    }
    if error
        .downcast_ref::<ApiRemoteSubagentSessionOwnership>()
        .is_some()
    {
        return api_lookup_failure(request, api_remote_subagent_ownership_error(session_id));
    }
    session_failure(
        request,
        "internal",
        format!("failed to create session \"{session_id}\": {error}"),
        Map::new(),
    )
}

fn api_lookup_failure(
    request: RpcRequest<Value>,
    error: ApiRemoteLookupError,
) -> RpcResponse<Value> {
    match error {
        ApiRemoteLookupError::AgentBusy { message, details } => session_failure(
            request,
            "agent-busy",
            message,
            serde_json::to_value(details)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default(),
        ),
        ApiRemoteLookupError::SessionNotFound { message, details } => session_failure(
            request,
            "session-not-found",
            message,
            serde_json::to_value(details)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default(),
        ),
        ApiRemoteLookupError::Internal { message, details } => session_failure(
            request,
            "internal",
            message,
            details.as_object().cloned().unwrap_or_default(),
        ),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("session \"{0}\" not found")]
struct HistoryNotFound(SessionId);

fn paginate_history(
    events: &[SessionEvent],
    before_seq: Option<u64>,
    max_messages: u64,
) -> (Vec<SessionEvent>, bool) {
    let window = events
        .iter()
        .filter(|event| before_seq.is_none_or(|before| event.seq < before))
        .cloned()
        .collect::<Vec<_>>();
    let maximum = usize::try_from(max_messages).unwrap_or(usize::MAX);
    let mut count = 0;
    let mut cut = 0;
    for event in window.iter().rev() {
        if !matches!(
            event.event_type.as_str(),
            "user/message" | "assistant/message"
        ) || !matches!(&event.surface_op, Some(SurfaceOp::Marker(marker)) if marker == "append")
        {
            continue;
        }
        count += 1;
        let group_start = event
            .source_event_seqs
            .as_ref()
            .and_then(|sources| sources.iter().copied().min())
            .map_or(event.seq, |source| source.min(event.seq));
        if count >= maximum {
            cut = group_start;
            break;
        }
    }
    (
        window
            .into_iter()
            .filter(|event| event.seq >= cut)
            .collect(),
        cut > 0,
    )
}

fn backscan_call(page: &[SessionEvent], call_id: &str) -> Option<(String, Value)> {
    page.iter().rev().find_map(|event| {
        if event.event_type != "tool/call"
            || event.data.get("callId").and_then(Value::as_str) != Some(call_id)
        {
            return None;
        }
        let name = event.data.get("name")?.as_str()?.to_owned();
        let arguments = serde_json::from_str(event.data.get("arguments")?.as_str()?).ok()?;
        Some((name, arguments))
    })
}

fn session_failure(
    request: RpcRequest<Value>,
    code: impl Into<String>,
    message: impl Into<String>,
    details: Map<String, Value>,
) -> RpcResponse<Value> {
    RpcResponse::new(
        request.rpc_id,
        RpcResult::Failure {
            error: RpcError {
                code: code.into(),
                message: message.into(),
                details,
            },
        },
    )
}

fn subscribed_envelope(session: &Session) -> RpcRequest<MuxFrame> {
    RpcRequest::new(
        next_session_frame_id(),
        MuxFrame::SessionSubscribed {
            session_id: session.id().clone(),
            last_seq: i64::try_from(session.seq())
                .unwrap_or(i64::MAX)
                .saturating_sub(1),
        },
    )
}

fn send_subscribed_if_new(
    sender: &tokio::sync::mpsc::UnboundedSender<RpcRequest<MuxFrame>>,
    subscribed: &parking_lot::Mutex<HashSet<SessionId>>,
    session: &Session,
) {
    if subscribed.lock().insert(session.id().clone()) {
        let _ = sender.send(subscribed_envelope(session));
    }
}

fn next_session_frame_id() -> crate::RpcId {
    crate::RpcId::new(format!(
        "session-frame-{}",
        NEXT_SESSION_FRAME_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

struct SessionMuxListenerGuard {
    effects: Option<Vec<EffectHandle>>,
}

impl SessionMuxListenerGuard {
    const fn new(effects: Vec<EffectHandle>) -> Self {
        Self {
            effects: Some(effects),
        }
    }
}

impl Drop for SessionMuxListenerGuard {
    fn drop(&mut self) {
        let Some(effects) = self.effects.take() else {
            return;
        };
        let dispose = async move {
            for effect in effects.into_iter().rev() {
                if let Err(error) = effect.dispose().await {
                    tracing::warn!(%error, "Session mux listener disposal failed");
                }
            }
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(dispose);
        } else {
            std::thread::spawn(move || futures::executor::block_on(dispose));
        }
    }
}

struct SessionMuxStream {
    inner: ApiDownlinkStream<MuxFrame>,
    _guard: SessionMuxListenerGuard,
}

impl futures::Stream for SessionMuxStream {
    type Item = anyhow::Result<RpcRequest<MuxFrame>>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(context)
    }
}
