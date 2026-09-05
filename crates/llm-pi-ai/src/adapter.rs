//! Snapshot-owned multi-provider LLM adapter over native protocol executors.

use std::{collections::HashMap, error::Error, fmt, pin::Pin, sync::Arc};

use async_trait::async_trait;
use futures::{Stream, future::BoxFuture};
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_attachment::AttachmentStore;
use seekdeep_llm::{
    AbortSignal, AdapterStream, GenerateOptions, LlmAdapter, LlmDiscoveredModel, LlmError,
    LlmModelContext, LlmModelInfo, LlmModelReasoningInfo, LlmProviderInfo, LlmReasoningEffortInfo,
    LlmResolvedModelInfo, ModelId, ModelModality, ProviderId, ReasoningEffortId,
    ResolvedRetryPolicy, SessionId, attribution_headers, content_has_image,
};
use seekdeep_util::timeout::{IdleWatchdog, timeout_of};
use serde_json::{Map, Value};

use crate::{
    catalog::{PiModality, PiModel, PiThinkingLevel, THINKING_LEVELS},
    config::{PiCacheRetention, PiThinkingBudgets, PiTransport, ResolvedPiProviderProfile},
    context::{PiContext, to_pi_context, to_pi_context_with_images},
    provider::PiProvider,
    replay::{PiAssistantMessage, PiAssistantRole, PiStopReason, PiUsage},
    stream::{PiAssistantEvent, to_stream_chunks},
};

/// Fallible native pi-ai event stream.
pub type BoxPiEventStream =
    Pin<Box<dyn Stream<Item = anyhow::Result<PiAssistantEvent>> + Send + 'static>>;

/// Provider-native authentication resolved once for an immutable call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PiResolvedAuth {
    /// Whether the provider's auth method resolved at all.
    pub configured: bool,
    /// Highest-priority provider API-key or bearer-token override.
    pub api_key: Option<String>,
    /// Provider auth headers; `None` removes a model header case-insensitively.
    pub headers: HashMap<String, Option<String>>,
    /// Provider environment facts used to materialize endpoint/auth behavior.
    pub environment: HashMap<String, String>,
}

impl PiResolvedAuth {
    /// Creates a configured API-key resolution, including a deliberately
    /// keyless configured value used by provider-native credential chains.
    #[must_use]
    pub fn api_key(api_key: Option<String>) -> Self {
        Self {
            configured: true,
            api_key,
            headers: HashMap::new(),
            environment: HashMap::new(),
        }
    }
}

/// Immutable request options handed to one native protocol engine.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PiStreamOptions {
    /// Harness-resolved API key override.
    pub api_key: Option<String>,
    /// Enabled reasoning level; `off` becomes absence exactly as in pi-ai.
    pub reasoning: Option<PiThinkingLevel>,
    /// Provider-specific reasoning budgets.
    pub thinking_budgets: Option<PiThinkingBudgets>,
    /// Prompt-cache preference.
    pub cache_retention: Option<PiCacheRetention>,
    /// Transport preference.
    pub transport: Option<PiTransport>,
    /// Provider SDK timeout.
    pub timeout_ms: Option<u64>,
    /// WebSocket connection timeout.
    pub websocket_connect_timeout_ms: Option<u64>,
    /// Request temperature.
    pub temperature: Option<f64>,
    /// Request output cap.
    pub max_tokens: Option<u64>,
    /// Session identity rendered for provider integrations.
    pub session_id: Option<SessionId>,
    /// Stable fused caller/consumer/timeout signal.
    pub signal: AbortSignal,
    /// Deployment headers with Harness attribution winning collisions.
    pub headers: HashMap<String, String>,
    /// Provider-native environment captured for this call.
    pub auth_environment: HashMap<String, String>,
    /// SDK retries are always disabled; agent recovery owns visible attempts.
    pub max_retries: u64,
}

/// Complete native protocol-engine request.
#[derive(Clone, Debug, PartialEq)]
pub struct PiExecutionRequest {
    /// Built provider dispatch and auth metadata.
    pub provider: PiProvider,
    /// Exact materialized model.
    pub model: PiModel,
    /// Converted native history and tools.
    pub context: PiContext,
    /// Frozen per-call options.
    pub options: PiStreamOptions,
}

/// Native protocol implementation boundary replacing pi-ai's JavaScript `Models` collection.
pub trait PiProtocolExecutor: Send + Sync + 'static {
    /// Opens one provider-native event stream.
    ///
    /// # Errors
    ///
    /// Returns synchronous provider/transport setup failures unchanged.
    fn stream(&self, request: PiExecutionRequest) -> anyhow::Result<BoxPiEventStream>;
}

/// Supplies an identity-stable current profile map.
pub trait PiProfileSource: Send + Sync + 'static {
    /// Current validated routes; unchanged resolution reuses the same `Arc`.
    fn profiles(&self) -> Arc<IndexMap<String, ResolvedPiProviderProfile>>;
}

/// Resolves one already-validated route's provider authentication exactly once.
#[async_trait]
pub trait PiApiKeyResolver: Send + Sync + 'static {
    /// Resolves explicit or provider-native auth without mutating process state.
    async fn resolve(
        &self,
        provider: &ProviderId,
        profile: &ResolvedPiProviderProfile,
    ) -> anyhow::Result<PiResolvedAuth>;
}

/// Resolves the optional durable attachment service at request time.
pub trait PiAttachmentResolver: Send + Sync + 'static {
    /// Current service, including one installed after the adapter.
    fn resolve(&self) -> Option<AttachmentStore>;
}

/// Constructor dependencies owned by the package plugin.
pub struct PiAiAdapterOptions {
    /// Dynamic profile snapshot source.
    pub profiles: Arc<dyn PiProfileSource>,
    /// Per-call credential resolver.
    pub api_keys: Arc<dyn PiApiKeyResolver>,
    /// Native protocol implementations.
    pub executor: Arc<dyn PiProtocolExecutor>,
    /// Optional late-bound attachment resolver.
    pub attachments: Option<Arc<dyn PiAttachmentResolver>>,
}

impl std::fmt::Debug for PiAiAdapterOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PiAiAdapterOptions")
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct PiAiSnapshot {
    profiles: Arc<IndexMap<String, ResolvedPiProviderProfile>>,
}

struct AdapterCore {
    config: PiAiAdapterOptions,
    snapshot: Mutex<Option<Arc<PiAiSnapshot>>>,
}

/// Generic native multi-provider adapter.
#[derive(Clone)]
pub struct PiAiAdapter {
    core: Arc<AdapterCore>,
}

impl std::fmt::Debug for PiAiAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PiAiAdapter")
            .finish_non_exhaustive()
    }
}

impl PiAiAdapter {
    /// Creates an adapter over dynamic profile and protocol capabilities.
    #[must_use]
    pub fn new(config: PiAiAdapterOptions) -> Self {
        Self {
            core: Arc::new(AdapterCore {
                config,
                snapshot: Mutex::new(None),
            }),
        }
    }

    fn current(&self) -> Arc<PiAiSnapshot> {
        let profiles = self.core.config.profiles.profiles();
        let mut slot = self.core.snapshot.lock();
        if let Some(snapshot) = slot.as_ref()
            && Arc::ptr_eq(&snapshot.profiles, &profiles)
        {
            return snapshot.clone();
        }
        let snapshot = Arc::new(PiAiSnapshot { profiles });
        *slot = Some(snapshot.clone());
        snapshot
    }
}

#[async_trait]
impl LlmAdapter for PiAiAdapter {
    fn provider_info(&self, provider: &str) -> LlmProviderInfo {
        LlmProviderInfo {
            id: ProviderId::new(provider),
            name: self.current().profiles.get(provider).map_or_else(
                || provider.to_owned(),
                |profile| profile.display_name.clone(),
            ),
        }
    }

    fn provider_retry_policy(&self, provider: &str) -> Option<ResolvedRetryPolicy> {
        self.current()
            .profiles
            .get(provider)
            .map(|profile| profile.retry_policy.clone())
    }

    async fn list_models(&self, provider: &str) -> anyhow::Result<Vec<LlmModelInfo>> {
        let snapshot = self.current();
        let profile = profile_of(&snapshot, provider)?;
        Ok(profile
            .pi_provider
            .models
            .iter()
            .map(|model| LlmModelInfo {
                provider: ProviderId::new(provider),
                id: model.id.clone(),
                name: model.name.clone(),
                description: None,
                input_modalities: Some(modalities(&model.input)),
            })
            .collect())
    }

    async fn resolve_model(
        &self,
        provider: &str,
        model: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LlmResolvedModelInfo> {
        let snapshot = self.current();
        let profile = profile_of(&snapshot, provider)?;
        let model = model_of(profile, provider, model)?;
        let supported = supported_thinking_levels(model);
        let default = profile
            .options
            .reasoning
            .filter(|level| supported.contains(level));
        Ok(LlmResolvedModelInfo {
            provider: ProviderId::new(provider),
            id: model.id.clone(),
            name: model.name.clone(),
            description: None,
            input_modalities: Some(modalities(&model.input)),
            context: Some(LlmModelContext {
                context_window: model.context_window,
            }),
            default_max_tokens: profile
                .catalog
                .configured_max_tokens
                .get(&model.id)
                .copied(),
            reasoning: reasoning_info(model, default),
        })
    }

    #[allow(clippy::too_many_lines)] // Keeps the single-owner stream lifecycle auditable intact.
    fn stream(&self, options: GenerateOptions) -> AdapterStream {
        let adapter = self.clone();
        let consumer = AbortSignal::default();
        let cleanup_signal = consumer.clone();
        let output = async_stream::try_stream! {
            if options.stop.is_some() {
                Err::<(), _>(LlmError::simple(
                    "llm-pi-ai does not support GenerateOptions.stop",
                    "UNSUPPORTED_OPTION",
                ))?;
            }
            let snapshot = adapter.current();
            let profile = profile_of(&snapshot, options.provider.as_str())?.clone();
            let mut model = model_of(&profile, options.provider.as_str(), options.model.as_str())?.clone();
            let effort = options.reasoning_effort.as_ref().map(ReasoningEffortId::as_str)
                .or_else(|| profile.options.reasoning.map(PiThinkingLevel::as_str));
            let reasoning = resolve_reasoning_level(&model, effort)?;
            let auth = adapter.core.config.api_keys
                .resolve(&options.provider, &profile)
                .await?;
            apply_resolved_auth(&mut model, &auth);
            let upstream = options.signal.as_ref().map_or_else(
                || consumer.clone(),
                |caller| AbortSignal::fuse(caller, &consumer),
            );
            let watchdog = IdleWatchdog::new(
                Some(&upstream),
                profile.stream_idle_timeout_ms,
                "LLM_STREAM_IDLE_TIMEOUT",
            )?;
            let _abort_on_drop = AbortOnDrop(consumer.clone());
            let contains_image = options.messages.iter().any(|message| content_has_image(message.content()));
            if contains_image && !model.input.contains(&PiModality::Image) {
                Err::<(), _>(classified_error(
                    LlmError::simple(
                        format!("pi-ai model \"{}\" does not support image input", model.id.as_str()),
                        "UNSUPPORTED_CONTENT",
                    ).into(),
                    &watchdog,
                    options.signal.as_ref(),
                    profile.stream_idle_timeout_ms,
                ))?;
            }
            let attachments = contains_image
                .then(|| adapter.core.config.attachments.as_ref().and_then(|resolver| resolver.resolve()))
                .flatten();
            if contains_image && attachments.is_none() {
                Err::<(), _>(classified_error(
                    LlmError::simple(
                        "pi-ai image input requires the durable attachment service",
                        "UNSUPPORTED_CONTENT",
                    ).into(),
                    &watchdog,
                    options.signal.as_ref(),
                    profile.stream_idle_timeout_ms,
                ))?;
            }
            let context = match attachments.as_ref() {
                None => to_pi_context(&options).map_err(anyhow::Error::from),
                Some(attachments) => to_pi_context_with_images(&options, attachments).await,
            }
            .map_err(|error| classified_error(
                error,
                &watchdog,
                options.signal.as_ref(),
                profile.stream_idle_timeout_ms,
            ))?;
            let execution = PiExecutionRequest {
                provider: profile.pi_provider.clone(),
                model: model.clone(),
                context,
                options: PiStreamOptions {
                    api_key: auth.api_key.clone(),
                    reasoning: reasoning.filter(|level| *level != PiThinkingLevel::Off),
                    thinking_budgets: profile.options.thinking_budgets.clone(),
                    cache_retention: profile.options.cache_retention,
                    transport: profile.options.transport,
                    timeout_ms: profile.options.timeout_ms,
                    websocket_connect_timeout_ms: profile.options.websocket_connect_timeout_ms,
                    temperature: options.temperature,
                    max_tokens: options.max_tokens,
                    session_id: options.session_id.clone(),
                    signal: watchdog.signal.clone(),
                    headers: request_headers(profile.options.headers.as_ref()),
                    auth_environment: auth.environment.clone(),
                    max_retries: 0,
                },
            };
            let execution_result = if auth.configured {
                adapter.core.config.executor.stream(execution)
            } else {
                Err(anyhow::anyhow!(
                    "Provider is not configured: {}",
                    options.provider.as_str()
                ))
            };
            let events = match execution_result {
                Ok(events) => events,
                Err(error) if timeout_of(&watchdog.signal, Some("LLM_STREAM_IDLE_TIMEOUT")).is_some()
                    || options.signal.as_ref().is_some_and(AbortSignal::is_aborted) => {
                    Err::<BoxPiEventStream, _>(classified_error(
                        error,
                        &watchdog,
                        options.signal.as_ref(),
                        profile.stream_idle_timeout_ms,
                    ))?
                }
                Err(error) => executor_error_events(&model, &error),
            };
            let mut chunks = to_stream_chunks(events, Some(model.context_window));
            loop {
                let next = watchdog.next(&mut chunks).await.map_err(anyhow::Error::from)?;
                if timeout_of(&watchdog.signal, Some("LLM_STREAM_IDLE_TIMEOUT")).is_some() {
                    Err::<(), _>(classified_error(
                        anyhow::anyhow!("pi-ai native stream stopped after idle timeout"),
                        &watchdog,
                        options.signal.as_ref(),
                        profile.stream_idle_timeout_ms,
                    ))?;
                }
                let Some(next) = next else { return };
                match next {
                    Ok(chunk) => yield chunk,
                    Err(error) => Err::<(), _>(classified_error(
                        error,
                        &watchdog,
                        options.signal.as_ref(),
                        profile.stream_idle_timeout_ms,
                    ))?,
                }
            }
        };
        AdapterStream::with_cleanup(output, move || async move {
            cleanup_signal.abort_with_reason(serde_json::json!("pi-ai stream consumer stopped"));
            Ok(())
        })
    }
}

fn executor_error_events(model: &PiModel, error: &anyhow::Error) -> BoxPiEventStream {
    let failed = PiAssistantMessage {
        role: PiAssistantRole::Assistant,
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        usage: PiUsage::default(),
        stop_reason: PiStopReason::Error,
        error_message: Some(error.to_string()),
        timestamp: 0,
    };
    Box::pin(futures::stream::once(async move {
        Ok(PiAssistantEvent::Error {
            reason: PiStopReason::Error,
            error: failed,
        })
    }))
}

fn apply_resolved_auth(model: &mut PiModel, auth: &PiResolvedAuth) {
    for (name, value) in &auth.environment {
        model.base_url = model
            .base_url
            .replace(&format!("{{{name}}}"), value.as_str());
    }
    if auth.headers.is_empty() {
        return;
    }
    let headers = model
        .extra
        .entry("headers".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(headers) = headers.as_object_mut() else {
        return;
    };
    for (name, value) in &auth.headers {
        if let Some(existing) = headers
            .keys()
            .find(|existing| existing.eq_ignore_ascii_case(name))
            .cloned()
        {
            headers.remove(&existing);
        }
        if let Some(value) = value {
            headers.insert(name.clone(), Value::String(value.clone()));
        }
    }
}

fn profile_of<'a>(
    snapshot: &'a PiAiSnapshot,
    provider: &str,
) -> Result<&'a ResolvedPiProviderProfile, LlmError> {
    snapshot.profiles.get(provider).ok_or_else(|| {
        LlmError::simple(
            format!("pi-ai adapter does not own provider \"{provider}\""),
            "NO_ADAPTER",
        )
    })
}

fn model_of<'a>(
    profile: &'a ResolvedPiProviderProfile,
    provider: &str,
    model: &str,
) -> Result<&'a PiModel, LlmError> {
    profile
        .pi_provider
        .models
        .iter()
        .find(|candidate| candidate.id.as_str() == model)
        .ok_or_else(|| {
            LlmError::simple(
                format!("pi-ai provider \"{provider}\" has no configured model \"{model}\""),
                "UNKNOWN_MODEL",
            )
        })
}

fn modalities(input: &[PiModality]) -> Vec<ModelModality> {
    input
        .iter()
        .map(|modality| match modality {
            PiModality::Text => ModelModality("text".to_owned()),
            PiModality::Image => ModelModality("image".to_owned()),
        })
        .collect()
}

fn supported_thinking_levels(model: &PiModel) -> Vec<PiThinkingLevel> {
    if !model.reasoning {
        return vec![PiThinkingLevel::Off];
    }
    THINKING_LEVELS
        .into_iter()
        .filter(|level| {
            model.thinking_level_map.as_ref().map_or(
                matches!(
                    level,
                    PiThinkingLevel::Off
                        | PiThinkingLevel::Minimal
                        | PiThinkingLevel::Low
                        | PiThinkingLevel::Medium
                        | PiThinkingLevel::High
                ),
                |map| match map.get(level.as_str()) {
                    Some(serde_json::Value::Null) => false,
                    Some(_) => true,
                    None => matches!(
                        level,
                        PiThinkingLevel::Off
                            | PiThinkingLevel::Minimal
                            | PiThinkingLevel::Low
                            | PiThinkingLevel::Medium
                            | PiThinkingLevel::High
                    ),
                },
            )
        })
        .collect()
}

fn resolve_reasoning_level(
    model: &PiModel,
    effort: Option<&str>,
) -> Result<Option<PiThinkingLevel>, LlmError> {
    let Some(effort) = effort else {
        return Ok(None);
    };
    if let Some(level) = supported_thinking_levels(model)
        .into_iter()
        .find(|level| level.as_str() == effort)
    {
        return Ok(Some(level));
    }
    Err(LlmError::simple(
        format!(
            "pi-ai provider \"{}\" model \"{}\" does not support reasoning effort \"{effort}\"",
            model.provider.as_str(),
            model.id.as_str()
        ),
        "UNSUPPORTED_REASONING_EFFORT",
    ))
}

fn reasoning_info(
    model: &PiModel,
    default: Option<PiThinkingLevel>,
) -> Option<LlmModelReasoningInfo> {
    model.reasoning.then(|| LlmModelReasoningInfo {
        efforts: supported_thinking_levels(model)
            .into_iter()
            .map(|level| {
                let value = level.as_str();
                let mut characters = value.chars();
                let name = characters.next().map_or_else(String::new, |first| {
                    first.to_uppercase().collect::<String>() + characters.as_str()
                });
                LlmReasoningEffortInfo {
                    id: ReasoningEffortId::new(value),
                    name,
                    description: None,
                }
            })
            .collect(),
        default_effort: default.map(|level| ReasoningEffortId::new(level.as_str())),
    })
}

fn request_headers(
    headers: Option<&serde_json::Map<String, serde_json::Value>>,
) -> HashMap<String, String> {
    let attribution = attribution_headers();
    let mut output = headers
        .into_iter()
        .flat_map(|headers| headers.iter())
        .filter(|(name, value)| {
            value.is_string()
                && !attribution
                    .keys()
                    .any(|reserved| reserved.eq_ignore_ascii_case(name))
        })
        .map(|(name, value)| (name.clone(), value.as_str().unwrap_or_default().to_owned()))
        .collect::<HashMap<_, _>>();
    output.extend(attribution);
    output
}

fn classified_error(
    error: anyhow::Error,
    watchdog: &IdleWatchdog,
    caller: Option<&AbortSignal>,
    timeout_ms: f64,
) -> anyhow::Error {
    if timeout_of(&watchdog.signal, Some("LLM_STREAM_IDLE_TIMEOUT")).is_some() {
        return LlmError::simple(
            format!(
                "pi-ai stream idle timeout after {}ms",
                ryu_js::Buffer::new().format(timeout_ms)
            ),
            "TIMEOUT",
        )
        .with_cause(AnyhowCause(error))
        .into();
    }
    if caller.is_some_and(AbortSignal::is_aborted) {
        return LlmError::simple("pi-ai request aborted by caller", "ABORTED")
            .with_cause(AnyhowCause(error))
            .into();
    }
    error
}

struct AnyhowCause(anyhow::Error);

impl fmt::Debug for AnyhowCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, formatter)
    }
}

impl fmt::Display for AnyhowCause {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl Error for AnyhowCause {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.0.as_ref())
    }
}

struct AbortOnDrop(AbortSignal);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0
            .abort_with_reason(serde_json::json!("pi-ai stream consumer stopped"));
    }
}

/// Function-backed profile source for bindings and tests.
pub struct FnProfileSource<F>(pub F);

impl<F> PiProfileSource for FnProfileSource<F>
where
    F: Fn() -> Arc<IndexMap<String, ResolvedPiProviderProfile>> + Send + Sync + 'static,
{
    fn profiles(&self) -> Arc<IndexMap<String, ResolvedPiProviderProfile>> {
        (self.0)()
    }
}

/// Function-backed key resolver for bindings and tests.
pub struct FnApiKeyResolver<F>(pub F);

#[async_trait]
impl<F> PiApiKeyResolver for FnApiKeyResolver<F>
where
    F: for<'a> Fn(
            &'a ProviderId,
            &'a ResolvedPiProviderProfile,
        ) -> BoxFuture<'a, anyhow::Result<PiResolvedAuth>>
        + Send
        + Sync
        + 'static,
{
    async fn resolve(
        &self,
        provider: &ProviderId,
        profile: &ResolvedPiProviderProfile,
    ) -> anyhow::Result<PiResolvedAuth> {
        (self.0)(provider, profile).await
    }
}

/// Function-backed attachment resolver for bindings and tests.
pub struct FnAttachmentResolver<F>(pub F);

impl<F> PiAttachmentResolver for FnAttachmentResolver<F>
where
    F: Fn() -> Option<AttachmentStore> + Send + Sync + 'static,
{
    fn resolve(&self) -> Option<AttachmentStore> {
        (self.0)()
    }
}

/// Adapter-local discovery metadata helper retained for package composition.
#[must_use]
pub fn discovered_from_models(models: &[PiModel]) -> Vec<LlmDiscoveredModel> {
    models
        .iter()
        .map(|model| LlmDiscoveredModel {
            id: ModelId::new(model.id.as_str()),
            name: Some(model.name.clone()),
            context_window: Some(model.context_window),
            max_tokens: Some(model.max_tokens),
        })
        .collect()
}
