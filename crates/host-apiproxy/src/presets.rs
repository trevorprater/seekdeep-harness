//! Production Agent-preset roster, authoring, and blank-session switching RPCs.

use std::{
    collections::HashMap,
    pin::Pin,
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context as TaskContext, Poll},
};

use base64::Engine as _;
use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use parking_lot::{Mutex, RwLock};
use seekdeep_agent::{
    AGENT, AGENTS, Agent, AgentOptions, AgentSetup, CreateAgentMeta, CreateAgentOptions,
    ModelSelection as AgentModelSelection,
};
use seekdeep_agent_presets::{
    AGENT_PRESETS, InvalidPresetIdError, PresetExistsError, PresetMountError,
    PresetNotWritableError, PresetTrust, UnknownPresetError, resolve_session_preset,
};
use seekdeep_api_remotes::{
    ApiRemoteAgentOptions, ApiRemoteAgentResult, ApiRemoteLookupError,
    create_api_remote_agent_resolver, inspect_api_remote_session,
};
use seekdeep_attachment::{ATTACHMENTS, AttachmentError, ImageAttachmentRef, SaveImageAttachment};
use seekdeep_client_connection::{HttpResponse, RpcError, RpcResult};
use seekdeep_cordis::{Context, EventArgs, EventOptions, EventReply, fiber::EffectHandle};
use seekdeep_llm::{
    AbortSignal, ContentBlock, LLM, LlmCallConfig, MessageSource, ModelId, ProviderId,
    ReasoningEffortId, UserMessage, content_has_image,
};
use seekdeep_skill::{SKILLS, SkillLookupOptions, SkillViewOptions, is_user_invocable};
use serde_json::{Map, Value, json};

use crate::{
    ApiDownlinkStream, ApiProxyRuntime, ClientResponse, DefaultModelSelection, PathOpener,
    PathOpenerInternals, RpcId, RpcMethod, RpcReceipt, RpcRequest, RpcResponse,
    SaveDefaultModelSelection,
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
        sessions::{
            ModelSelection as SessionModelSelection, PromptContentPart, PromptMode,
            SessionAttachmentRequest, SessionAttachmentValue, SessionForkRequest, SessionForkValue,
            SessionModelsRequest, SessionModelsValue, SessionPromptRequest, SessionPromptValue,
            SessionRenameRequest, SessionRenameValue, SessionSelectModelRequest,
            SessionSelectModelValue,
        },
        skills::{SkillEntry, SkillListRequest, SkillListValue},
    },
    configuration::build_model_catalog,
    native_path_opener::{can_open_native_path, open_native_path},
};

static NEXT_HOST_EVENT_ID: AtomicU64 = AtomicU64::new(1);

/// Native and model defaults consumed by Agent-preset RPCs.
#[derive(Clone)]
pub struct PresetApiProxyOptions {
    /// Default model options for a cold Agent resume.
    pub default_model_selection: DefaultModelSelection,
    /// Optional persistence callback for an accepted model switch.
    pub save_default_model_selection: Option<SaveDefaultModelSelection>,
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
            .field(
                "has_save_default_model_selection",
                &self.save_default_model_selection.is_some(),
            )
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
    selections: Arc<Mutex<HashMap<seekdeep_core::session::SessionId, Weak<ApiModelSelection>>>>,
    image_admissions:
        Arc<Mutex<HashMap<seekdeep_core::session::SessionId, Arc<tokio::sync::Mutex<()>>>>>,
    domains: Arc<dyn ApiProxyRuntime>,
}

struct ApiModelSelection {
    session: Arc<seekdeep_core::session::Session>,
    defaults: DefaultModelSelection,
    picked: RwLock<Option<AgentModelSelection>>,
    assembled: RwLock<Option<AgentModelSelection>>,
}

enum PreparedPromptPart {
    Text(String),
    Image(SaveImageAttachment),
}

struct ForkSource {
    header: seekdeep_core::session::SessionHeader,
    events: Vec<seekdeep_core::session::SessionEvent>,
}

impl ApiModelSelection {
    fn new(
        session: Arc<seekdeep_core::session::Session>,
        defaults: DefaultModelSelection,
    ) -> Arc<Self> {
        Arc::new(Self {
            session,
            defaults,
            picked: RwLock::new(None),
            assembled: RwLock::new(None),
        })
    }

    fn current(&self) -> AgentModelSelection {
        if let Some(picked) = self.picked.read().clone() {
            return picked;
        }
        if let Some(header) = self.session.request_header() {
            return AgentModelSelection {
                provider: header.config.provider,
                model: header.config.model,
                reasoning_effort: header.config.reasoning_effort,
            };
        }
        let defaults = (self.defaults)();
        AgentModelSelection {
            provider: ProviderId::new(defaults.provider),
            model: ModelId::new(defaults.model),
            reasoning_effort: defaults.reasoning_effort.map(ReasoningEffortId::new),
        }
    }

    fn select(&self, selection: AgentModelSelection) {
        *self.picked.write() = Some(selection);
    }
}

fn install_api_model_selection(
    agent: &Arc<Agent>,
    defaults: DefaultModelSelection,
    selections: &Arc<Mutex<HashMap<seekdeep_core::session::SessionId, Weak<ApiModelSelection>>>>,
) -> anyhow::Result<Arc<ApiModelSelection>> {
    if let Some(existing) = selections
        .lock()
        .get(agent.id())
        .and_then(Weak::upgrade)
        .filter(|state| Arc::ptr_eq(&state.session, agent.session()))
    {
        return Ok(existing);
    }
    let prompt = agent
        .context()
        .get(seekdeep_system_prompt::SYSTEM_PROMPT)
        .ok_or_else(|| anyhow::anyhow!("model selection requires systemPrompt"))?;
    let state = ApiModelSelection::new(agent.session().clone(), defaults);
    let assembly_state = state.clone();
    prompt.on_assemble(
        agent.context(),
        move |_assembly, _context, next| {
            let state = assembly_state.clone();
            async move {
                let selected = state.current();
                let mut assembly = next.run().await?;
                *state.assembled.write() = Some(selected.clone());
                assembly
                    .variables
                    .insert("provider".to_owned(), Some(selected.provider.into_string()));
                assembly
                    .variables
                    .insert("model".to_owned(), Some(selected.model.into_string()));
                Ok(assembly)
            }
        },
        EventOptions::default(),
    )?;
    let request_state = state.clone();
    agent.context().events().on_waterfall(
        agent.context(),
        "agent/request",
        move |_, _, next| {
            let state = request_state.clone();
            Box::pin(async move {
                let reply = next.run().await?;
                let Some(config) = reply.downcast::<LlmCallConfig>() else {
                    anyhow::bail!("agent/request returned an invalid call config");
                };
                let Some(selected) = state.assembled.read().clone() else {
                    return Ok(EventReply::Value(config));
                };
                let mut resolved = (*config).clone();
                resolved.provider = selected.provider;
                resolved.model = selected.model;
                resolved.reasoning_effort = selected.reasoning_effort;
                Ok(EventReply::Value(Arc::new(resolved)))
            })
        },
        EventOptions::default(),
    )?;
    selections
        .lock()
        .insert(agent.id().clone(), Arc::downgrade(&state));
    Ok(state)
}

impl std::fmt::Debug for PresetApiProxyRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PresetApiProxyRuntime")
            .field("options", &self.options)
            .field("switch_locks", &self.switches.lock().len())
            .field("selection_slots", &self.selections.lock().len())
            .field("image_admission_locks", &self.image_admissions.lock().len())
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
        let selections = Arc::new(Mutex::new(HashMap::new()));
        let setup_context = context.clone();
        let setup_defaults = options.default_model_selection.clone();
        let setup_selections = selections.clone();
        let setup: Arc<
            dyn Fn(
                    seekdeep_session_persistence::SessionInspection,
                ) -> BoxFuture<'static, anyhow::Result<Option<AgentSetup>>>
                + Send
                + Sync,
        > = Arc::new(move |inspection| {
            let roster = setup_context.get(AGENT_PRESETS);
            let defaults = setup_defaults.clone();
            let selections = setup_selections.clone();
            Box::pin(async move {
                let mounted = if let Some(roster) = roster {
                    let id = resolve_session_preset(&inspection.meta, &inspection.events);
                    let preset = roster.resolve_mountable(id.as_deref()).await?;
                    Some((roster, preset))
                } else {
                    None
                };
                let setup: AgentSetup = Arc::new(move |agent_context| {
                    let mounted = mounted.clone();
                    let defaults = defaults.clone();
                    let selections = selections.clone();
                    Box::pin(async move {
                        let agent = agent_context.get(AGENT).ok_or_else(|| {
                            anyhow::anyhow!("API Proxy Agent setup has no scoped Agent")
                        })?;
                        install_api_model_selection(&agent, defaults, &selections)?;
                        if let Some((roster, preset)) = mounted {
                            roster.mount_resolved(&agent_context, preset).await?;
                        }
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
            selections,
            image_admissions: Arc::new(Mutex::new(HashMap::new())),
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
            RpcMethod::SessionModels => self.models(request).await,
            RpcMethod::SessionSelectModel => self.select_model(request, signal).await,
            RpcMethod::SessionPrompt => self.prompt(request, signal).await,
            RpcMethod::SessionAttachment => self.attachment(request, signal).await,
            RpcMethod::SessionFork => self.fork(request, signal).await,
            RpcMethod::SessionRename => self.rename_session(request).await,
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

    async fn rename_session(
        &self,
        request: RpcRequest<Value>,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: SessionRenameRequest = serde_json::from_value(request.payload.clone())?;
        let agent = match (self.resolve_agent)(payload.session_id.clone()).await {
            ApiRemoteAgentResult::Agent(agent) => agent,
            ApiRemoteAgentResult::Error(error) => return remote_failure(request, error),
        };
        let Some(titles) = self.context.get(seekdeep_session_title::SESSION_TITLE) else {
            return Ok(failure(
                request,
                "internal",
                "renaming is unavailable: this deployment mounts no session-title service",
                Map::new(),
            ));
        };
        match titles.rename(agent.session(), &payload.title) {
            Ok(accepted) => typed_success(
                request,
                &SessionRenameValue {
                    title: accepted.event.title,
                    seq: accepted.event_seq,
                },
            ),
            Err(error)
                if error
                    .downcast_ref::<seekdeep_session_title::SessionTitleInvalidError>()
                    .is_some() =>
            {
                Ok(failure(
                    request,
                    "title-invalid",
                    error.to_string(),
                    Map::from_iter([(
                        "sessionId".to_owned(),
                        Value::String(payload.session_id.to_string()),
                    )]),
                ))
            }
            Err(error) => Ok(failure(
                request,
                "internal",
                format!(
                    "failed to rename session \"{}\": {error}",
                    payload.session_id
                ),
                Map::new(),
            )),
        }
    }

    async fn models(&self, request: RpcRequest<Value>) -> anyhow::Result<RpcResponse<Value>> {
        let payload: SessionModelsRequest = serde_json::from_value(request.payload.clone())?;
        let agent = match (self.resolve_agent)(payload.session_id.clone()).await {
            ApiRemoteAgentResult::Agent(agent) => agent,
            ApiRemoteAgentResult::Error(error) => return remote_failure(request, error),
        };
        let selection = self.selection_for(&agent)?;
        let llm = self
            .context
            .get(LLM)
            .ok_or_else(|| anyhow::anyhow!("llm service is absent"))?;
        let current = wire_model_selection(selection.current());
        let routable = llm
            .list_providers()
            .iter()
            .any(|provider| provider.id.as_str() == current.provider);
        let (groups, failures) = build_model_catalog(llm).await;
        typed_success(
            request,
            &SessionModelsValue {
                current,
                routable,
                groups,
                failures,
            },
        )
    }

    async fn select_model(
        &self,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: SessionSelectModelRequest = serde_json::from_value(request.payload.clone())?;
        let agent = match (self.resolve_agent)(payload.session_id.clone()).await {
            ApiRemoteAgentResult::Agent(agent) => agent,
            ApiRemoteAgentResult::Error(error) => return remote_failure(request, error),
        };
        let lock = {
            let mut locks = self.image_admissions.lock();
            locks
                .entry(payload.session_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;
        let llm = self
            .context
            .get(LLM)
            .ok_or_else(|| anyhow::anyhow!("llm service is absent"))?;
        let requested = LlmCallConfig {
            provider: ProviderId::new(&payload.provider),
            model: ModelId::new(&payload.model),
            reasoning_effort: payload
                .reasoning_effort
                .as_deref()
                .map(ReasoningEffortId::new),
            temperature: None,
            max_tokens: None,
            stop: None,
        };
        let resolved = match llm.resolve_call_config(&requested, Some(&signal)).await {
            Ok(resolved) => resolved,
            Err(error) => {
                return Ok(model_unavailable(
                    request,
                    &payload.provider,
                    &payload.model,
                    &error,
                ));
            }
        };
        if agent_has_visible_image(&agent) {
            let info = match llm
                .resolve_model_info(&resolved.provider, &resolved.model, Some(&signal))
                .await
            {
                Ok(info) => info,
                Err(error) => {
                    return Ok(model_unavailable(
                        request,
                        &payload.provider,
                        &payload.model,
                        &error,
                    ));
                }
            };
            if info
                .input_modalities
                .as_ref()
                .is_some_and(|modalities| !modalities.iter().any(|modality| modality.0 == "image"))
            {
                return Ok(failure(
                    request,
                    "model-unavailable",
                    format!(
                        "Model \"{}\" does not accept image input, but this session already contains images; select an image-capable model.",
                        resolved.model
                    ),
                    Map::from_iter([
                        (
                            "provider".to_owned(),
                            Value::String(payload.provider.clone()),
                        ),
                        ("model".to_owned(), Value::String(payload.model.clone())),
                    ]),
                ));
            }
        }
        let selected = AgentModelSelection {
            provider: resolved.provider,
            model: resolved.model,
            reasoning_effort: resolved.reasoning_effort,
        };
        self.selection_for(&agent)?.select(selected.clone());
        if let Some(save) = &self.options.save_default_model_selection
            && let Err(error) = save(default_model_selection(selected.clone())).await
        {
            tracing::warn!(%error, "API Proxy model switch applied but default persistence failed");
        }
        typed_success(
            request,
            &SessionSelectModelValue {
                selected: wire_model_selection(selected),
            },
        )
    }

    #[allow(clippy::too_many_lines)] // One ordered admission transaction validates before any image is saved.
    async fn prompt(
        &self,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: SessionPromptRequest = serde_json::from_value(request.payload.clone())?;
        let agent = match (self.resolve_agent)(payload.session_id.clone()).await {
            ApiRemoteAgentResult::Agent(agent) => agent,
            ApiRemoteAgentResult::Error(error) => return remote_failure(request, error),
        };
        let selection = self.selection_for(&agent)?.current();
        let llm = self
            .context
            .get(LLM)
            .ok_or_else(|| anyhow::anyhow!("llm service is absent"))?;
        if !llm
            .list_providers()
            .iter()
            .any(|provider| provider.id == selection.provider)
        {
            return Ok(failure(
                request,
                "model-unavailable",
                format!(
                    "No adapter serves provider \"{}\" for this session; select an available model before prompting.",
                    selection.provider
                ),
                Map::from_iter([
                    (
                        "provider".to_owned(),
                        Value::String(selection.provider.to_string()),
                    ),
                    (
                        "model".to_owned(),
                        Value::String(selection.model.to_string()),
                    ),
                ]),
            ));
        }
        let has_image = payload
            .content
            .iter()
            .any(|part| matches!(part, PromptContentPart::Image { .. }));
        let lock = if has_image {
            Some({
                let mut locks = self.image_admissions.lock();
                locks
                    .entry(payload.session_id.clone())
                    .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                    .clone()
            })
        } else {
            None
        };
        let _guard = match &lock {
            Some(lock) => Some(lock.lock().await),
            None => None,
        };
        if has_image {
            let info = match llm
                .resolve_model_info(&selection.provider, &selection.model, Some(&signal))
                .await
            {
                Ok(info) => info,
                Err(error) => {
                    return Ok(model_unavailable(
                        request,
                        selection.provider.as_str(),
                        selection.model.as_str(),
                        &error,
                    ));
                }
            };
            if info
                .input_modalities
                .as_ref()
                .is_some_and(|modalities| !modalities.iter().any(|modality| modality.0 == "image"))
            {
                return Ok(failure(
                    request,
                    "attachment-error",
                    format!(
                        "Model \"{}\" does not support image input.",
                        selection.model
                    ),
                    Map::from_iter([(
                        "reason".to_owned(),
                        Value::String("MODEL_DOES_NOT_SUPPORT_IMAGES".to_owned()),
                    )]),
                ));
            }
        }
        let content = match self.durable_prompt_content(payload.content).await {
            Ok(content) => content,
            Err(error) => return Ok(prompt_failure(request, &error)),
        };
        let mut source = MessageSource::user();
        source.fields.insert(
            "rpcId".to_owned(),
            Value::String(request.rpc_id.to_string()),
        );
        if let Some(zone) = payload.client_time_zone {
            source
                .fields
                .insert("clientTimeZone".to_owned(), Value::String(zone));
        }
        let message = UserMessage::new(content, source);
        let admitted = match payload.mode {
            PromptMode::Queue => agent.followup(message),
            PromptMode::Steer => agent.steer(message),
        };
        match admitted {
            Ok(()) => typed_success(
                request,
                &SessionPromptValue {
                    accepted: true,
                    command: None,
                },
            ),
            Err(error) => Ok(failure(
                request,
                "agent-busy",
                "prompt rejected",
                Map::from_iter([("reason".to_owned(), Value::String(error.to_string()))]),
            )),
        }
    }

    async fn durable_prompt_content(
        &self,
        content: Vec<PromptContentPart>,
    ) -> anyhow::Result<Vec<ContentBlock>> {
        if content
            .iter()
            .all(|part| matches!(part, PromptContentPart::Text { .. }))
        {
            return Ok(content
                .into_iter()
                .map(|part| match part {
                    PromptContentPart::Text { text } => ContentBlock::Text { text },
                    PromptContentPart::Image { .. } => unreachable!("checked above"),
                })
                .collect());
        }
        let attachments = self
            .context
            .get(ATTACHMENTS)
            .ok_or_else(|| anyhow::anyhow!("attachment service is absent"))?;
        let image_count = content
            .iter()
            .filter(|part| matches!(part, PromptContentPart::Image { .. }))
            .count();
        if u64::try_from(image_count).unwrap_or(u64::MAX)
            > attachments.image_limits().max_images_per_message
        {
            return Err(AttachmentError::new(
                "Prompt exceeds the configured image-count limit.",
                "TOO_MANY_IMAGES",
            )
            .into());
        }
        let mut prepared = Vec::with_capacity(content.len());
        let mut total_bytes = 0_u64;
        for part in content {
            match part {
                PromptContentPart::Text { text } => prepared.push(PreparedPromptPart::Text(text)),
                PromptContentPart::Image {
                    media_type,
                    data,
                    name,
                } => {
                    let decoded = base64::engine::general_purpose::STANDARD
                        .decode(&data)
                        .ok()
                        .filter(|decoded| {
                            !data.is_empty()
                                && base64::engine::general_purpose::STANDARD.encode(decoded) == data
                        })
                        .ok_or_else(|| {
                            AttachmentError::new(
                                "Image upload is not canonical base64.",
                                "INVALID_IMAGE_BASE64",
                            )
                        })?;
                    total_bytes = total_bytes
                        .saturating_add(u64::try_from(decoded.len()).unwrap_or(u64::MAX));
                    prepared.push(PreparedPromptPart::Image(SaveImageAttachment {
                        data: decoded,
                        media_type,
                        name,
                    }));
                }
            }
        }
        if total_bytes > attachments.image_limits().max_message_image_bytes {
            return Err(AttachmentError::new(
                "Prompt exceeds the configured aggregate image-byte limit.",
                "IMAGES_TOO_LARGE",
            )
            .into());
        }
        for part in &prepared {
            if let PreparedPromptPart::Image(image) = part {
                attachments.validate_image(image).await?;
            }
        }
        let mut durable = Vec::with_capacity(prepared.len());
        for part in prepared {
            match part {
                PreparedPromptPart::Text(text) => durable.push(ContentBlock::Text { text }),
                PreparedPromptPart::Image(image) => durable.push(ContentBlock::Image {
                    attachment: attachments.save_image(image).await?,
                }),
            }
        }
        Ok(durable)
    }

    async fn attachment(
        &self,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: SessionAttachmentRequest = serde_json::from_value(request.payload.clone())?;
        let events = if let Some(session) = self
            .context
            .get(seekdeep_core::session_store::SESSIONS)
            .and_then(|sessions| sessions.get(&payload.session_id))
        {
            session.events()
        } else {
            match inspect_api_remote_session(&self.context, &payload.session_id).await {
                Ok((_, events)) => events,
                Err(error)
                    if error
                        .downcast_ref::<seekdeep_api_remotes::ApiRemoteSessionNotFound>()
                        .is_some() =>
                {
                    return Ok(failure(
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
                    return Ok(failure(
                        request,
                        "internal",
                        format!(
                            "attachment authorization unavailable for session \"{}\": {error}",
                            payload.session_id
                        ),
                        Map::new(),
                    ));
                }
            }
        };
        let Some(reference) = referenced_image(&events, payload.attachment_id.as_str()) else {
            return Ok(failure(
                request,
                "attachment-error",
                "Image is not referenced by this session.",
                Map::from_iter([(
                    "reason".to_owned(),
                    Value::String("ATTACHMENT_NOT_REFERENCED".to_owned()),
                )]),
            ));
        };
        let Some(attachments) = self.context.get(ATTACHMENTS) else {
            return Ok(failure(
                request,
                "internal",
                "Unable to read image attachment.",
                Map::new(),
            ));
        };
        match attachments.read_image(&reference, Some(signal)).await {
            Ok(stored) => typed_success(
                request,
                &SessionAttachmentValue {
                    attachment: stored.reference,
                    data: base64::engine::general_purpose::STANDARD.encode(stored.data),
                },
            ),
            Err(error) if error.downcast_ref::<AttachmentError>().is_some() => {
                Ok(attachment_failure(request, &error))
            }
            Err(_) => Ok(failure(
                request,
                "internal",
                "Unable to read image attachment.",
                Map::new(),
            )),
        }
    }

    #[allow(clippy::too_many_lines)] // One ordered fork transaction preserves cut, publication, and attachment boundaries.
    async fn fork(
        &self,
        request: RpcRequest<Value>,
        signal: AbortSignal,
    ) -> anyhow::Result<RpcResponse<Value>> {
        let payload: SessionForkRequest = serde_json::from_value(request.payload.clone())?;
        let source = match self.fork_source(&payload.session_id).await {
            Ok(source) => source,
            Err(error)
                if error
                    .downcast_ref::<seekdeep_api_remotes::ApiRemoteSessionNotFound>()
                    .is_some() =>
            {
                return Ok(failure(
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
                return Ok(failure(
                    request,
                    "internal",
                    format!(
                        "fork source unavailable for session \"{}\": {error}",
                        payload.session_id
                    ),
                    Map::new(),
                ));
            }
        };
        let last_seq = source.events.last().map(|event| event.seq);
        let boundary = payload.at_seq.and_then(|anchor| {
            source
                .events
                .iter()
                .find(|event| event.event_type == "turn/end" && event.seq >= anchor)
        });
        let boundary = boundary.or_else(|| {
            if payload.at_seq.is_none() || payload.at_seq > last_seq {
                source
                    .events
                    .iter()
                    .rev()
                    .find(|event| event.event_type == "turn/end")
            } else {
                None
            }
        });
        let Some(boundary) = boundary else {
            let message =
                if let Some(anchor) = payload.at_seq.filter(|_| payload.at_seq <= last_seq) {
                    format!(
                        "session \"{}\" has not completed the turn containing event {anchor}",
                        payload.session_id
                    )
                } else {
                    format!(
                        "session \"{}\" has no completed turn to fork from",
                        payload.session_id
                    )
                };
            return Ok(failure(
                request,
                "fork-unavailable",
                message,
                Map::from_iter([(
                    "sessionId".to_owned(),
                    Value::String(payload.session_id.to_string()),
                )]),
            ));
        };
        let boundary_index = source
            .events
            .iter()
            .position(|event| event.seq == boundary.seq)
            .expect("boundary came from source events");
        let mut cut = boundary_index + 1;
        while source
            .events
            .get(cut)
            .is_some_and(|event| event.event_type != "turn/start")
        {
            cut += 1;
        }
        let workspace = match self.fork_workspace(&source.header, signal).await {
            Ok(workspace) => workspace,
            Err(error) => {
                return Ok(failure(
                    request,
                    "internal",
                    format!(
                        "failed to resolve fork workspace for session \"{}\": {error}",
                        payload.session_id
                    ),
                    Map::new(),
                ));
            }
        };
        let preset = resolve_session_preset(&source.header, &source.events);
        let (agent_preset, setup) = self.fork_setup(preset.as_deref()).await?;
        let child_id =
            seekdeep_core::session::SessionId::new(format!("session-{}", uuid::Uuid::new_v4()));
        let defaults = (self.options.default_model_selection)();
        let options = CreateAgentOptions {
            session_id: child_id.clone(),
            meta: CreateAgentMeta {
                cwd: source.header.cwd.clone(),
                parent_session: Some(source.header.id.clone()),
                seed_length: Some(u64::try_from(cut).unwrap_or(u64::MAX)),
                agent_preset,
                ..CreateAgentMeta::default()
            },
            seed: Some(source.events[..cut].to_vec()),
            agent_options: AgentOptions {
                provider: Some(ProviderId::new(defaults.provider)),
                model: Some(ModelId::new(defaults.model)),
                ..AgentOptions::default()
            },
            signal: None,
            setup: Some(setup),
            owner_agent: None,
        };
        let agents = self
            .context
            .get(AGENTS)
            .ok_or_else(|| anyhow::anyhow!("agents registry is absent"))?;
        if let Err(error) = agents.create(options).await {
            return Ok(failure(
                request,
                "internal",
                format!("failed to fork session \"{}\": {error}", payload.session_id),
                Map::new(),
            ));
        }
        if let Some(workspace) = workspace
            && let Err(error) = workspace.attach_session(child_id.clone()).await
        {
            return Ok(failure(
                request,
                "workspace-attach-failed",
                format!(
                    "session \"{child_id}\" was forked but could not attach to workspace \"{}\": {error}",
                    workspace.id()
                ),
                Map::from_iter([
                    ("sessionId".to_owned(), Value::String(child_id.to_string())),
                    (
                        "workspaceId".to_owned(),
                        Value::String(workspace.id().to_string()),
                    ),
                ]),
            ));
        }
        typed_success(
            request,
            &SessionForkValue {
                session_id: child_id,
            },
        )
    }

    async fn fork_source(
        &self,
        session_id: &seekdeep_core::session::SessionId,
    ) -> anyhow::Result<ForkSource> {
        if let Some(session) = self
            .context
            .get(seekdeep_core::session_store::SESSIONS)
            .and_then(|sessions| sessions.get(session_id))
        {
            return Ok(ForkSource {
                header: session.header().clone(),
                events: session.events(),
            });
        }
        let (header, events) = inspect_api_remote_session(&self.context, session_id).await?;
        Ok(ForkSource { header, events })
    }

    async fn fork_workspace(
        &self,
        source: &seekdeep_core::session::SessionHeader,
        signal: AbortSignal,
    ) -> anyhow::Result<Option<Arc<seekdeep_workspace::Workspace>>> {
        let Some(registry) = self.context.get(seekdeep_workspace::WORKSPACE_REGISTRY) else {
            return Ok(None);
        };
        let workspaces = registry.list()?;
        if source.origin != Some(seekdeep_core::session::SessionOrigin::Subagent) {
            return Ok(workspaces
                .into_iter()
                .find(|workspace| workspace.session_ids().contains(&source.id)));
        }
        let query = self
            .context
            .get(seekdeep_session_query::SESSION_QUERY)
            .ok_or_else(|| {
                anyhow::anyhow!("subagent fork workspace resolution requires sessionQuery")
            })?;
        let trace = query.trace_session(source.id.clone(), Some(signal)).await?;
        for ancestor in trace.ancestors {
            if let Some(workspace) = workspaces
                .iter()
                .find(|workspace| workspace.session_ids().contains(&ancestor.header.id))
            {
                return Ok(Some(workspace.clone()));
            }
        }
        Ok(None)
    }

    async fn fork_setup(
        &self,
        preset: Option<&str>,
    ) -> anyhow::Result<(Option<String>, AgentSetup)> {
        let mounted = if let Some(roster) = self.context.get(AGENT_PRESETS) {
            let preset = roster.resolve_mountable(preset).await?;
            let id = preset.id.clone();
            Some((roster, preset, id))
        } else {
            None
        };
        let agent_preset = mounted.as_ref().map(|(_, _, id)| id.clone());
        let defaults = self.options.default_model_selection.clone();
        let selections = self.selections.clone();
        let setup: AgentSetup = Arc::new(move |agent_context| {
            let mounted = mounted.clone();
            let defaults = defaults.clone();
            let selections = selections.clone();
            Box::pin(async move {
                let agent = agent_context
                    .get(AGENT)
                    .ok_or_else(|| anyhow::anyhow!("API Proxy fork setup has no scoped Agent"))?;
                install_api_model_selection(&agent, defaults, &selections)?;
                if let Some((roster, preset, _)) = mounted {
                    roster.mount_resolved(&agent_context, preset).await?;
                }
                Ok(None)
            })
        });
        Ok((agent_preset, setup))
    }

    fn selection_for(&self, agent: &Arc<Agent>) -> anyhow::Result<Arc<ApiModelSelection>> {
        install_api_model_selection(
            agent,
            self.options.default_model_selection.clone(),
            &self.selections,
        )
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
            selections: self.selections.clone(),
            image_admissions: self.image_admissions.clone(),
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

fn wire_model_selection(selection: AgentModelSelection) -> SessionModelSelection {
    SessionModelSelection {
        provider: selection.provider.into_string(),
        model: selection.model.into_string(),
        reasoning_effort: selection
            .reasoning_effort
            .map(ReasoningEffortId::into_string),
    }
}

fn default_model_selection(selection: AgentModelSelection) -> crate::ModelSelection {
    crate::ModelSelection {
        provider: selection.provider.into_string(),
        model: selection.model.into_string(),
        reasoning_effort: selection
            .reasoning_effort
            .map(ReasoningEffortId::into_string),
    }
}

fn agent_has_visible_image(agent: &Agent) -> bool {
    let messages = agent.session().derive_messages();
    let next_turn = agent.inbox().next_turn();
    let next_step = agent.inbox().next_step();
    messages
        .iter()
        .any(|message| content_has_image(message.content()))
        || next_turn
            .iter()
            .any(|message| content_has_image(message.content()))
        || next_step
            .iter()
            .any(|message| content_has_image(message.content()))
}

fn model_unavailable(
    request: RpcRequest<Value>,
    provider: &str,
    model: &str,
    error: &anyhow::Error,
) -> RpcResponse<Value> {
    failure(
        request,
        "model-unavailable",
        error.to_string(),
        Map::from_iter([
            ("provider".to_owned(), Value::String(provider.to_owned())),
            ("model".to_owned(), Value::String(model.to_owned())),
        ]),
    )
}

fn prompt_failure(request: RpcRequest<Value>, error: &anyhow::Error) -> RpcResponse<Value> {
    if error.downcast_ref::<AttachmentError>().is_some() {
        return attachment_failure(request, error);
    }
    failure(
        request,
        "agent-busy",
        "prompt rejected",
        Map::from_iter([("reason".to_owned(), Value::String(error.to_string()))]),
    )
}

fn attachment_failure(request: RpcRequest<Value>, error: &anyhow::Error) -> RpcResponse<Value> {
    let attachment = error
        .downcast_ref::<AttachmentError>()
        .expect("caller established attachment error");
    failure(
        request,
        "attachment-error",
        attachment.to_string(),
        Map::from_iter([("reason".to_owned(), Value::String(attachment.code.clone()))]),
    )
}

fn referenced_image(
    events: &[seekdeep_core::session::SessionEvent],
    attachment_id: &str,
) -> Option<ImageAttachmentRef> {
    events
        .iter()
        .find_map(|event| image_in_event(event, attachment_id))
}

fn image_in_event(
    event: &seekdeep_core::session::SessionEvent,
    attachment_id: &str,
) -> Option<ImageAttachmentRef> {
    for content in [
        event.data.get("content"),
        event
            .data
            .get("message")
            .and_then(|message| message.get("content")),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(reference) = image_in_content(content, attachment_id) {
            return Some(reference);
        }
    }
    if let Some(inserted) = event.data.get("inserted").and_then(Value::as_array) {
        for message in inserted {
            if let Some(reference) = message
                .get("content")
                .and_then(|content| image_in_content(content, attachment_id))
            {
                return Some(reference);
            }
        }
    }
    if event.event_type == "assistant/chunk"
        && event
            .data
            .get("chunk")
            .and_then(|chunk| chunk.get("type"))
            .and_then(Value::as_str)
            == Some("block-end")
        && let Some(block) = event.data.get("chunk").and_then(|chunk| chunk.get("block"))
    {
        return image_in_content(&Value::Array(vec![block.clone()]), attachment_id);
    }
    None
}

fn image_in_content(content: &Value, attachment_id: &str) -> Option<ImageAttachmentRef> {
    for block in content.as_array()? {
        if block.get("type").and_then(Value::as_str) == Some("image")
            && let Some(reference) = block
                .get("attachment")
                .cloned()
                .and_then(|value| serde_json::from_value::<ImageAttachmentRef>(value).ok())
            && reference.attachment_id.as_str() == attachment_id
        {
            return Some(reference);
        }
        if block.get("type").and_then(Value::as_str) == Some("tool-result")
            && let Some(reference) = block
                .get("content")
                .and_then(|nested| image_in_content(nested, attachment_id))
        {
            return Some(reference);
        }
    }
    None
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
