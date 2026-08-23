//! Production Agent-preset roster, authoring, and blank-session switching RPCs.

use std::{
    collections::HashMap,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context as TaskContext, Poll},
};

use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use parking_lot::Mutex;
use seekdeep_agent::{AgentOptions, AgentSetup};
use seekdeep_agent_presets::{
    AGENT_PRESETS, InvalidPresetIdError, PresetExistsError, PresetMountError,
    PresetNotWritableError, PresetTrust, UnknownPresetError, resolve_session_preset,
};
use seekdeep_api_remotes::{
    ApiRemoteAgentOptions, ApiRemoteAgentResult, ApiRemoteLookupError,
    create_api_remote_agent_resolver,
};
use seekdeep_client_connection::{HttpResponse, RpcError, RpcResult};
use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply, fiber::EffectHandle};
use seekdeep_llm::{AbortSignal, ModelId, ProviderId};
use seekdeep_skill::{SKILLS, SkillLookupOptions, SkillViewOptions, is_user_invocable};
use serde_json::{Map, Value, json};

use crate::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, DefaultModelSelection, PathOpener,
    PathOpenerInternals, RpcId, RpcMethod, RpcReceipt, RpcRequest, RpcResponse,
    api::{
        agent_presets::{
            AgentPresetCopyRequest, AgentPresetEntry, AgentPresetIdValue, AgentPresetListValue,
            AgentPresetReadValue, AgentPresetSelectRequest, AgentPresetTrust,
        },
        downloads::SessionLogQuery,
        events::{HostFrame, MuxFrame},
        goals::{
            GoalClearValue, GoalCreateRequest, GoalEditRequest, GoalRef as WireGoalRef,
            GoalRefRequest, GoalRefValue,
        },
        skills::{SkillEntry, SkillListRequest, SkillListValue},
    },
    native_path_opener::{can_open_native_path, open_native_path},
};

static NEXT_HOST_EVENT_ID: AtomicU64 = AtomicU64::new(1);

/// Native and model defaults consumed by Agent-preset RPCs.
#[derive(Clone)]
pub struct PresetApiProxyOptions {
    /// Default model options for a cold Agent resume.
    pub default_model_selection: DefaultModelSelection,
    /// Optional native path opener.
    pub open_path: Option<PathOpener>,
    /// Optional native-open capability override.
    pub can_open_path: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    /// Platform boundary used by the default opener.
    pub native_path_opener: PathOpenerInternals,
}

impl std::fmt::Debug for PresetApiProxyOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PresetApiProxyOptions")
            .field("has_open_path", &self.open_path.is_some())
            .field("has_can_open_path", &self.can_open_path.is_some())
            .field("native_path_opener", &self.native_path_opener)
            .finish_non_exhaustive()
    }
}

/// Agent-preset decorator over the remaining API Proxy domains.
pub struct PresetApiProxyRuntime {
    context: Context,
    options: PresetApiProxyOptions,
    resolve_agent: Arc<
        dyn Fn(seekdeep_core::session::SessionId) -> BoxFuture<'static, ApiRemoteAgentResult>
            + Send
            + Sync,
    >,
    switches: Arc<Mutex<HashMap<seekdeep_core::session::SessionId, Arc<tokio::sync::Mutex<()>>>>>,
    domains: Arc<dyn ApiProxyRuntime>,
}

impl std::fmt::Debug for PresetApiProxyRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PresetApiProxyRuntime")
            .field("options", &self.options)
            .field("switch_locks", &self.switches.lock().len())
            .finish_non_exhaustive()
    }
}

impl PresetApiProxyRuntime {
    /// Builds the roster RPC layer and its shared cold-Agent resolver.
    #[must_use]
    pub fn from_context(
        context: &Context,
        options: PresetApiProxyOptions,
        domains: Arc<dyn ApiProxyRuntime>,
    ) -> Arc<Self> {
        let setup_context = context.clone();
        let setup: Arc<
            dyn Fn(
                    seekdeep_session_persistence::SessionInspection,
                ) -> BoxFuture<'static, anyhow::Result<Option<AgentSetup>>>
                + Send
                + Sync,
        > = Arc::new(move |inspection| {
            let roster = setup_context.get(AGENT_PRESETS);
            Box::pin(async move {
                let Some(roster) = roster else {
                    return Ok(None);
                };
                let id = resolve_session_preset(&inspection.meta, &inspection.events);
                let preset = roster.resolve_mountable(id.as_deref()).await?;
                let setup_roster = roster.clone();
                let setup: AgentSetup = Arc::new(move |agent_context| {
                    let roster = setup_roster.clone();
                    let preset = preset.clone();
                    Box::pin(async move {
                        roster.mount_resolved(&agent_context, preset).await?;
                        Ok(None)
                    })
                });
                Ok(Some(setup))
            })
        });
        let defaults = options.default_model_selection.clone();
        let agent_options = Arc::new(move || {
            let selection = defaults();
            AgentOptions {
                provider: Some(ProviderId::new(selection.provider)),
                model: Some(ModelId::new(selection.model)),
                ..AgentOptions::default()
            }
        });
        let resolve_agent = create_api_remote_agent_resolver(
            context,
            ApiRemoteAgentOptions {
                agent_options: Some(agent_options),
                setup: Some(setup),
            },
        );
        Arc::new(Self {
            context: context.clone(),
            options,
            resolve_agent,
            switches: Arc::new(Mutex::new(HashMap::new())),
            domains,
        })
    }

    async fn preset_unary(
        &self,
        method: RpcMethod,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<RpcResponse<Value>> {
        match method {
            RpcMethod::AgentPresetList => self.list(request).await,
            RpcMethod::AgentPresetSelect => self.select(request).await,
            RpcMethod::AgentPresetRead => self.read(request).await,
            RpcMethod::AgentPresetCopy => self.copy(request).await,
            RpcMethod::AgentPresetOpenDocument => self.open_document(request, signal).await,
            RpcMethod::AgentPresetRemove => self.remove(request).await,
            RpcMethod::SkillList => self.list_skills(request).await,
            RpcMethod::GoalCreate
            | RpcMethod::GoalEdit
            | RpcMethod::GoalPause
            | RpcMethod::GoalResume
            | RpcMethod::GoalComplete
            | RpcMethod::GoalClear => self.mutate_goal(method, request).await,
            _ => self.domains.unary(method, request, signal).await,
        }
    }

    async fn list(&self, request: RpcRequest<Value>) -> anyhow::Result<RpcResponse<Value>> {
        let Some(roster) = self.context.get(AGENT_PRESETS) else {
            return typed_success(
                request,
                &AgentPresetListValue {
                    presets: Vec::new(),
                    authorable: false,
                    has_document: false,
                },
            );
        };
        let default = roster.default_id();
        let presets = roster
            .list()
            .await?
            .into_iter()
            .map(|preset| AgentPresetEntry {
                id: preset.id.clone(),
                trust: wire_trust(preset.trust),
                is_default: preset.id == default,
                name: preset.name,
                description: preset.description,
                broken: preset.broken,
            })
            .collect();
        typed_success(
            request,
            &AgentPresetListValue {
                presets,
                authorable: roster.authorable(),
                has_document: self.can_open_paths(),
            },
        )
    }

    async fn select(&self, request: RpcRequest<Value>) -> anyhow::Result<RpcResponse<Value>> {
        let payload: AgentPresetSelectRequest = serde_json::from_value(request.payload.clone())?;
        let Some(roster) = self.context.get(AGENT_PRESETS) else {
            return Ok(no_roster(request, &payload.agent_preset));
        };
        let lock = {
            let mut locks = self.switches.lock();
            locks
                .entry(payload.session_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;
        let agent = match (self.resolve_agent)(payload.session_id.clone()).await {
            ApiRemoteAgentResult::Agent(agent) => agent,
            ApiRemoteAgentResult::Error(error) => return remote_failure(request, error),
        };
        if agent
            .session()
            .events()
            .iter()
            .any(|event| event.event_type == "turn/start")
        {
            return Ok(failure(
                request,
                "agent-preset-locked",
                format!(
                    "session \"{}\" has already started; its agent preset is fixed",
                    payload.session_id
                ),
                Map::from_iter([
                    (
                        "sessionId".to_owned(),
                        Value::String(payload.session_id.to_string()),
                    ),
                    (
                        "agentPreset".to_owned(),
                        Value::String(payload.agent_preset.clone()),
                    ),
                ]),
            ));
        }
        match roster
            .recompose(agent.context(), &payload.agent_preset)
            .await
        {
            Ok(preset) => {
                agent.session().append(
                    "agent-preset/selected",
                    json!({ "agentPreset": preset.id }),
                    seekdeep_core::session::AppendOptions::default(),
                )?;
                typed_success(
                    request,
                    &AgentPresetIdValue {
                        agent_preset: preset.id,
                    },
                )
            }
            Err(error) => Ok(select_preset_error(request, &payload.agent_preset, &error)),
        }
    }

    async fn read(&self, request: RpcRequest<Value>) -> anyhow::Result<RpcResponse<Value>> {
        let payload: AgentPresetIdValue = serde_json::from_value(request.payload.clone())?;
        let Some(roster) = self.context.get(AGENT_PRESETS) else {
            return Ok(no_roster(request, &payload.agent_preset));
        };
        match roster.resolve(Some(&payload.agent_preset)).await {
            Ok(preset) => match roster.read(&preset.id).await {
                Ok(content) => typed_success(
                    request,
                    &AgentPresetReadValue {
                        agent_preset: preset.id,
                        trust: wire_trust(preset.trust),
                        content,
                        name: preset.name,
                        description: preset.description,
                    },
                ),
                Err(error) => Ok(preset_error(request, &payload.agent_preset, &error)),
            },
            Err(error) => Ok(preset_error(request, &payload.agent_preset, &error)),
        }
    }

    async fn copy(&self, request: RpcRequest<Value>) -> anyhow::Result<RpcResponse<Value>> {
        let payload: AgentPresetCopyRequest = serde_json::from_value(request.payload.clone())?;
        let Some(roster) = self.context.get(AGENT_PRESETS) else {
            return Ok(no_roster(request, &payload.agent_preset));
        };
        match roster
            .copy(
                &payload.from,
                &payload.agent_preset,
                payload.name.as_deref(),
            )
            .await
        {
            Ok(()) => typed_success(
                request,
                &AgentPresetIdValue {
                    agent_preset: payload.agent_preset,
                },
            ),
            Err(error) => Ok(preset_error(request, &payload.agent_preset, &error)),
        }
    }

    async fn open_document(
        &self,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: AgentPresetIdValue = serde_json::from_value(request.payload.clone())?;
        let Some(roster) = self.context.get(AGENT_PRESETS) else {
            return Ok(no_roster(request, &payload.agent_preset));
        };
        let preset = match roster.resolve(Some(&payload.agent_preset)).await {
            Ok(preset) => preset,
            Err(error) => return Ok(preset_error(request, &payload.agent_preset, &error)),
        };
        if preset.trust != PresetTrust::User {
            let error: anyhow::Error =
                PresetNotWritableError::new(&preset.id, "it ships with the deployment").into();
            return Ok(preset_error(request, &payload.agent_preset, &error));
        }
        let directory = preset
            .path
            .parent()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !self.can_open_paths() {
            return typed_success(request, &json!({ "opened": false, "path": directory }));
        }
        let result = if let Some(open) = &self.options.open_path {
            open(directory, signal.clone()).await
        } else {
            open_native_path(&directory, &signal, &self.options.native_path_opener).await
        };
        match result {
            Ok(()) => typed_success(request, &json!({ "opened": true })),
            Err(_) if signal.is_aborted() => Ok(failure(
                request,
                "cancelled",
                "path open was aborted",
                Map::new(),
            )),
            Err(error) => Ok(failure(
                request,
                "internal",
                format!("path open failed: {error}"),
                Map::new(),
            )),
        }
    }

    async fn remove(&self, request: RpcRequest<Value>) -> anyhow::Result<RpcResponse<Value>> {
        let payload: AgentPresetIdValue = serde_json::from_value(request.payload.clone())?;
        let Some(roster) = self.context.get(AGENT_PRESETS) else {
            return Ok(no_roster(request, &payload.agent_preset));
        };
        match roster.remove(&payload.agent_preset).await {
            Ok(()) => typed_success(request, &json!({})),
            Err(error) => Ok(preset_error(request, &payload.agent_preset, &error)),
        }
    }

    async fn list_skills(&self, request: RpcRequest<Value>) -> anyhow::Result<RpcResponse<Value>> {
        let payload: SkillListRequest = serde_json::from_value(request.payload.clone())?;
        let Some(sessions) = self.context.get(seekdeep_core::session_store::SESSIONS) else {
            return Ok(failure(
                request,
                "internal",
                "session store is absent",
                Map::new(),
            ));
        };
        let Some(session) = sessions.get(&payload.session_id) else {
            return Ok(failure(
                request,
                "session-not-found",
                format!(
                    "session \"{}\" not found (not attached)",
                    payload.session_id
                ),
                Map::from_iter([(
                    "sessionId".to_owned(),
                    Value::String(payload.session_id.to_string()),
                )]),
            ));
        };
        let Some(cwd) = session.header().cwd.clone() else {
            return Ok(failure(
                request,
                "internal",
                format!("session \"{}\" has no project cwd", payload.session_id),
                Map::new(),
            ));
        };
        let live = self
            .context
            .get(seekdeep_agent::AGENTS)
            .and_then(|agents| agents.get(&payload.session_id));
        let roster = self.context.get(AGENT_PRESETS);
        let scoped_registry = live.as_ref().and_then(|agent| {
            roster
                .as_ref()
                .and_then(|roster| roster.service_for(agent, SKILLS))
        });
        let Some(skills) = scoped_registry.or_else(|| self.context.get(SKILLS)) else {
            return Ok(failure(
                request,
                "internal",
                "skill registry is absent: neither this session's agent preset nor the host composition mounts seekdeep-skill",
                Map::new(),
            ));
        };
        let scope = if let Some(agent) = live {
            Some(agent.scope_key())
        } else if let Some(roster) = roster {
            let preset = resolve_session_preset(session.header(), &session.events());
            roster.standing_key_for(preset.as_deref()).await.ok()
        } else {
            None
        };
        let listed = match skills
            .list(&SkillViewOptions {
                lookup: SkillLookupOptions {
                    cwd: Some(cwd),
                    signal: None,
                },
                scope,
            })
            .await
        {
            Ok(listed) => listed,
            Err(error) => {
                return Ok(failure(
                    request,
                    "internal",
                    format!("skill listing failed: {error}"),
                    Map::new(),
                ));
            }
        };
        typed_success(
            request,
            &SkillListValue {
                skills: listed
                    .into_iter()
                    .filter(is_user_invocable)
                    .map(|skill| SkillEntry {
                        name: skill.name,
                        description: skill.description,
                        when_to_use: skill.when_to_use,
                        model_invocable: skill.invocation.model_invocable,
                    })
                    .collect(),
            },
        )
    }

    async fn mutate_goal(
        &self,
        method: RpcMethod,
        request: RpcRequest<Value>,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let session_id = goal_session_id(method, &request.payload)?;
        let agent = match (self.resolve_agent)(session_id).await {
            ApiRemoteAgentResult::Agent(agent) => agent,
            ApiRemoteAgentResult::Error(error) => return remote_failure(request, error),
        };
        let roster = self.context.get(AGENT_PRESETS);
        let goals = roster
            .as_ref()
            .and_then(|roster| roster.service_for(&agent, seekdeep_goal::GOAL))
            .or_else(|| self.context.get(seekdeep_goal::GOAL));
        let Some(goals) = goals else {
            return Ok(failure(
                request,
                "internal",
                "goal service is absent: neither this session's agent preset nor the host composition mounts seekdeep-goal",
                Map::new(),
            ));
        };
        let result = match method {
            RpcMethod::GoalCreate => {
                let payload: GoalCreateRequest = serde_json::from_value(request.payload.clone())?;
                goals
                    .create(
                        &agent,
                        &seekdeep_goal::CreateGoalRequest {
                            objective: payload.objective,
                            max_goal_rounds: payload.max_goal_rounds,
                        },
                    )
                    .map(|goal| {
                        Some(wire_goal_ref(&seekdeep_goal::GoalRef {
                            id: goal.id,
                            revision: goal.revision,
                        }))
                    })
            }
            RpcMethod::GoalEdit => {
                let payload: GoalEditRequest = serde_json::from_value(request.payload.clone())?;
                goals
                    .edit(
                        &agent,
                        &core_goal_ref(&payload.r#ref),
                        &seekdeep_goal::EditGoalRequest {
                            objective: payload.objective,
                            max_goal_rounds: payload.max_goal_rounds,
                        },
                    )
                    .map(|goal| {
                        Some(wire_goal_ref(&seekdeep_goal::GoalRef {
                            id: goal.id,
                            revision: goal.revision,
                        }))
                    })
            }
            RpcMethod::GoalPause | RpcMethod::GoalResume | RpcMethod::GoalComplete => {
                let payload: GoalRefRequest = serde_json::from_value(request.payload.clone())?;
                let goal_ref = core_goal_ref(&payload.r#ref);
                let changed = match method {
                    RpcMethod::GoalPause => goals.pause(&agent, &goal_ref),
                    RpcMethod::GoalResume => goals.resume(&agent, &goal_ref),
                    RpcMethod::GoalComplete => goals.complete(&agent, &goal_ref),
                    _ => unreachable!("closed goal transition match"),
                };
                changed.map(|goal| {
                    Some(wire_goal_ref(&seekdeep_goal::GoalRef {
                        id: goal.id,
                        revision: goal.revision,
                    }))
                })
            }
            RpcMethod::GoalClear => {
                let payload: GoalRefRequest = serde_json::from_value(request.payload.clone())?;
                goals
                    .clear(&agent, &core_goal_ref(&payload.r#ref))
                    .map(|_| None)
            }
            _ => unreachable!("goal method dispatch"),
        };
        match result {
            Ok(Some(goal_ref)) => typed_success(request, &GoalRefValue { r#ref: goal_ref }),
            Ok(None) => typed_success(request, &GoalClearValue { cleared: true }),
            Err(error) => Ok(goal_failure(request, &error)),
        }
    }

    fn can_open_paths(&self) -> bool {
        if let Some(probe) = &self.options.can_open_path {
            return probe();
        }
        self.options.open_path.is_some() || can_open_native_path(&self.options.native_path_opener)
    }

    fn preset_host(&self, signal: AbortSignal) -> ApiDownlinkStream<HostFrame> {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let effect = self.context.events().on_sync(
            &self.context,
            "agent-preset/selected",
            move |_, args| {
                let session_id = required_event_arg::<seekdeep_core::session::SessionId>(
                    "agent-preset/selected",
                    &args,
                    0,
                )?;
                let preset = required_event_arg::<String>("agent-preset/selected", &args, 1)?;
                let _ = sender.send(HostFrame::RemoteEvent {
                    event: "agent-preset/selected".to_owned(),
                    args: vec![
                        Value::String(session_id.to_string()),
                        Value::String((*preset).clone()),
                    ],
                });
                Ok(EventReply::Undefined)
            },
            EventOptions::default(),
        );
        let effect = match effect {
            Ok(effect) => effect,
            Err(error) => {
                return PresetHostStream {
                    inner: futures::stream::once(async move { Err(error.into()) }).boxed(),
                    _guard: PresetHostListenerGuard::new(None),
                }
                .boxed();
            }
        };
        let stream = async_stream::stream! {
            loop {
                tokio::select! {
                    () = signal.cancelled() => break,
                    frame = receiver.recv() => match frame {
                        Some(frame) => yield Ok(RpcRequest::new(next_host_event_id(), frame)),
                        None => break,
                    },
                }
            }
        };
        PresetHostStream {
            inner: stream.boxed(),
            _guard: PresetHostListenerGuard::new(Some(effect)),
        }
        .boxed()
    }
}

impl ApiProxyRuntime for PresetApiProxyRuntime {
    fn unary(
        &self,
        method: RpcMethod,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<RpcResponse<Value>>> {
        let runtime = Arc::new(Self {
            context: self.context.clone(),
            options: self.options.clone(),
            resolve_agent: self.resolve_agent.clone(),
            switches: self.switches.clone(),
            domains: self.domains.clone(),
        });
        async move { runtime.preset_unary(method, request, signal).await }.boxed()
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
        futures::stream::select(
            self.domains.host(request, signal.clone()),
            self.preset_host(signal),
        )
        .boxed()
    }

    fn session_log(
        &self,
        query: SessionLogQuery,
        signal: AbortSignal,
    ) -> BoxFuture<'static, anyhow::Result<HttpResponse>> {
        self.domains.session_log(query, signal)
    }
}

fn required_event_arg<T: std::any::Any + Send + Sync>(
    event: &str,
    args: &EventArgs,
    index: usize,
) -> anyhow::Result<Arc<T>> {
    args.get(index)
        .ok_or_else(|| anyhow::anyhow!("{event} argument {index} has the wrong type or is absent"))
}

fn next_host_event_id() -> RpcId {
    RpcId::new(format!(
        "host-agent-preset-{}",
        NEXT_HOST_EVENT_ID.fetch_add(1, Ordering::Relaxed)
    ))
}

struct PresetHostListenerGuard {
    effect: Option<EffectHandle>,
}

struct PresetHostStream {
    inner: ApiDownlinkStream<HostFrame>,
    _guard: PresetHostListenerGuard,
}

impl futures::Stream for PresetHostStream {
    type Item = anyhow::Result<RpcRequest<HostFrame>>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(context)
    }
}

impl PresetHostListenerGuard {
    const fn new(effect: Option<EffectHandle>) -> Self {
        Self { effect }
    }
}

impl Drop for PresetHostListenerGuard {
    fn drop(&mut self) {
        let Some(effect) = self.effect.take() else {
            return;
        };
        let dispose = async move {
            if let Err(error) = effect.dispose().await {
                tracing::warn!(%error, "Agent-preset Host listener disposal failed");
            }
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(dispose);
        } else {
            std::thread::spawn(move || futures::executor::block_on(dispose));
        }
    }
}

fn wire_trust(trust: PresetTrust) -> AgentPresetTrust {
    match trust {
        PresetTrust::System => AgentPresetTrust::System,
        PresetTrust::User => AgentPresetTrust::User,
    }
}

fn goal_session_id(
    method: RpcMethod,
    payload: &Value,
) -> anyhow::Result<seekdeep_core::session::SessionId> {
    match method {
        RpcMethod::GoalCreate => {
            Ok(serde_json::from_value::<GoalCreateRequest>(payload.clone())?.session_id)
        }
        RpcMethod::GoalEdit => {
            Ok(serde_json::from_value::<GoalEditRequest>(payload.clone())?.session_id)
        }
        RpcMethod::GoalPause
        | RpcMethod::GoalResume
        | RpcMethod::GoalComplete
        | RpcMethod::GoalClear => {
            Ok(serde_json::from_value::<GoalRefRequest>(payload.clone())?.session_id)
        }
        _ => anyhow::bail!("{method} is not a goal mutation"),
    }
}

fn core_goal_ref(goal_ref: &WireGoalRef) -> seekdeep_goal::GoalRef {
    seekdeep_goal::GoalRef {
        id: seekdeep_goal::GoalId::new(goal_ref.id.as_str()),
        revision: goal_ref.revision,
    }
}

fn wire_goal_ref(goal_ref: &seekdeep_goal::GoalRef) -> WireGoalRef {
    WireGoalRef {
        id: crate::api::goals::GoalId::new(goal_ref.id.as_str()),
        revision: goal_ref.revision,
    }
}

fn goal_failure(request: RpcRequest<Value>, error: &anyhow::Error) -> RpcResponse<Value> {
    let mut details = Map::new();
    if let Some(error) = error.downcast_ref::<seekdeep_goal::runtime::GoalError>()
        && let Ok(Value::String(code)) = serde_json::to_value(error.code)
    {
        details.insert("goalCode".to_owned(), Value::String(code));
    }
    failure(request, "internal", error.to_string(), details)
}

fn no_roster(request: RpcRequest<Value>, preset: &str) -> RpcResponse<Value> {
    failure(
        request,
        "agent-preset-not-found",
        "this deployment composes no agent presets",
        Map::from_iter([
            ("agentPreset".to_owned(), Value::String(preset.to_owned())),
            ("available".to_owned(), Value::Array(Vec::new())),
        ]),
    )
}

fn preset_error(
    request: RpcRequest<Value>,
    preset: &str,
    error: &anyhow::Error,
) -> RpcResponse<Value> {
    if let Some(error) = error.downcast_ref::<UnknownPresetError>() {
        return failure(
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
    if error.downcast_ref::<PresetNotWritableError>().is_some() {
        return failure(
            request,
            "agent-preset-read-only",
            error.to_string(),
            Map::from_iter([
                ("agentPreset".to_owned(), Value::String(preset.to_owned())),
                ("reason".to_owned(), Value::String(error.to_string())),
            ]),
        );
    }
    if error.downcast_ref::<InvalidPresetIdError>().is_some()
        || error.downcast_ref::<PresetExistsError>().is_some()
    {
        return failure(
            request,
            "agent-preset-invalid",
            error.to_string(),
            Map::from_iter([
                ("agentPreset".to_owned(), Value::String(preset.to_owned())),
                ("reason".to_owned(), Value::String(error.to_string())),
            ]),
        );
    }
    failure(
        request,
        "internal",
        format!("agent preset \"{preset}\": {error}"),
        Map::new(),
    )
}

fn select_preset_error(
    request: RpcRequest<Value>,
    preset: &str,
    error: &anyhow::Error,
) -> RpcResponse<Value> {
    if let Some(error) = error.downcast_ref::<PresetMountError>() {
        return failure(
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
    if error.downcast_ref::<UnknownPresetError>().is_some() {
        return preset_error(request, preset, error);
    }
    failure(
        request,
        "internal",
        format!("failed to select agent preset \"{preset}\": {error}"),
        Map::new(),
    )
}

fn remote_failure(
    request: RpcRequest<Value>,
    error: ApiRemoteLookupError,
) -> anyhow::Result<RpcResponse<Value>> {
    let value = serde_json::to_value(error)?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("remote lookup error is not an object"))?;
    Ok(failure(
        request,
        object
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("internal"),
        object
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("lookup failed"),
        object
            .get("details")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
    ))
}

fn typed_success<T: serde::Serialize>(
    request: RpcRequest<Value>,
    value: &T,
) -> anyhow::Result<RpcResponse<Value>> {
    Ok(RpcResponse::new(
        request.rpc_id,
        RpcResult::Success {
            value: Some(serde_json::to_value(value)?),
        },
    ))
}

fn failure(
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
