//! Adapter registry, exact-model resolution, and normalized streaming runtime.

use std::{
    collections::{HashMap, HashSet},
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use async_stream::stream;
use async_trait::async_trait;
use futures::{FutureExt, Stream, StreamExt, future::BoxFuture};
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventArgs, ServiceKey, fiber::EffectHandle};
use uuid::Uuid;

use crate::{
    LlmError, MessageRole,
    adapter_failure::normalize_llm_failure,
    call_config::call_config_equals,
    retry_policy::{ResolvedRetryPolicy, resolve_retry_policy},
    types::{
        AbortSignal, FinishReason, GenerateOptions, LlmCallConfig, LlmCallConfigAdapterDefaults,
        LlmConfigurableProvider, LlmDiscoveredModel, LlmModelContext, LlmModelDiscoveryRequest,
        LlmModelInfo, LlmProviderInfo, LlmResolvedModelInfo, StreamChunk,
    },
};

/// Typed Cordis service key for the LLM runtime.
pub const LLM: ServiceKey<LlmRuntime> = ServiceKey::new("llm");

/// A fallible provider chunk stream before runtime normalization.
pub type AdapterStream = Pin<Box<dyn Stream<Item = anyhow::Result<StreamChunk>> + Send + 'static>>;

/// Provider-neutral stream whose adapter failures are terminal chunks while
/// middleware and invariant failures remain errors.
pub type LlmStream = Pin<Box<dyn Stream<Item = anyhow::Result<StreamChunk>> + Send + 'static>>;

/// Continuation supplied to one stream middleware.
pub type LlmStreamNext = Box<dyn FnOnce(GenerateOptions) -> LlmStream + Send + 'static>;

/// Around-middleware for routing, retry, replay, and observation.
pub type LlmStreamMiddleware =
    Arc<dyn Fn(GenerateOptions, LlmStreamNext) -> LlmStream + Send + Sync + 'static>;

/// Provider-wire adapter for Seekdeep's message and stream vocabulary.
#[async_trait]
pub trait LlmAdapter: Send + Sync + 'static {
    /// Describes one route owned by this adapter.
    fn provider_info(&self, provider: &str) -> LlmProviderInfo {
        LlmProviderInfo {
            id: provider.to_owned(),
            name: provider.to_owned(),
        }
    }

    /// Provider-owned retry policy, or normal defaults when absent.
    fn provider_retry_policy(&self, _provider: &str) -> Option<ResolvedRetryPolicy> {
        None
    }

    /// Advisory catalog for one provider.
    async fn list_models(&self, _provider: &str) -> anyhow::Result<Vec<LlmModelInfo>> {
        Ok(Vec::new())
    }

    /// Resolves exact-model metadata independently of catalog membership.
    async fn resolve_model(
        &self,
        provider: &str,
        model: &str,
        _signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LlmResolvedModelInfo> {
        Ok(LlmResolvedModelInfo {
            provider: provider.to_owned(),
            id: model.to_owned(),
            name: model.to_owned(),
            description: None,
            input_modalities: None,
            context: None,
            default_max_tokens: None,
            reasoning: None,
        })
    }

    /// Starts one raw provider stream.
    fn stream(&self, options: GenerateOptions) -> AdapterStream;
}

#[derive(Clone)]
struct AdapterRegistration {
    owner: Uuid,
    adapter: Arc<dyn LlmAdapter>,
    provider: LlmProviderInfo,
    retry_policy: ResolvedRetryPolicy,
}

#[derive(Clone)]
struct DirectoryEntry {
    owner: Uuid,
    value: LlmConfigurableProvider,
}

type ModelDiscovery = Arc<
    dyn Fn(LlmModelDiscoveryRequest) -> BoxFuture<'static, anyhow::Result<Vec<LlmDiscoveredModel>>>
        + Send
        + Sync,
>;

#[derive(Clone)]
struct DiscoveryEntry {
    owner: Uuid,
    discover: ModelDiscovery,
}

#[derive(Clone)]
struct StreamMiddlewareEntry {
    id: Uuid,
    middleware: LlmStreamMiddleware,
}

#[derive(Default)]
struct RuntimeState {
    adapters: IndexMap<String, AdapterRegistration>,
    registrations: HashSet<Uuid>,
    directory: IndexMap<String, DirectoryEntry>,
    directory_registrations: HashSet<Uuid>,
    discoveries: HashMap<String, DiscoveryEntry>,
    stream_middlewares: Vec<StreamMiddlewareEntry>,
}

/// Adapter registry and provider-neutral streaming service.
pub struct LlmRuntime {
    context: Context,
    state: Arc<Mutex<RuntimeState>>,
    stream_invariants: Arc<AtomicUsize>,
}

impl std::fmt::Debug for LlmRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LlmRuntime")
            .field("providers", &self.state.lock().adapters.len())
            .finish_non_exhaustive()
    }
}

impl LlmRuntime {
    /// Installs one runtime service into a context.
    ///
    /// # Errors
    ///
    /// Returns when the service slot is occupied or the context is inactive.
    pub fn install(context: &Context) -> Result<Arc<Self>, seekdeep_cordis::CordisError> {
        let runtime = Arc::new(Self {
            context: context.clone(),
            state: Arc::new(Mutex::new(RuntimeState::default())),
            stream_invariants: Arc::new(AtomicUsize::new(0)),
        });
        context.provide(LLM, runtime.clone())?;
        Ok(runtime)
    }

    /// Registers one adapter over a non-empty route set atomically.
    ///
    /// # Errors
    ///
    /// Returns an `INVALID_ADAPTER`, `DUPLICATE_ADAPTER`, or inactive-owner failure.
    pub fn register_adapter(
        self: &Arc<Self>,
        providers: &[String],
        adapter: Arc<dyn LlmAdapter>,
    ) -> anyhow::Result<AdapterRegistrationHandle> {
        if providers.is_empty() {
            return Err(llm_error(
                "an adapter must register at least one provider",
                "INVALID_ADAPTER",
            ));
        }
        let owner = Uuid::now_v7();
        let prepared = self.prepare_routes(providers, &adapter, owner, None)?;
        {
            let mut state = self.state.lock();
            ensure_routes_available(&state, &prepared, None)?;
            state.registrations.insert(owner);
            for registration in prepared {
                state
                    .adapters
                    .insert(registration.provider.id.clone(), registration);
            }
        }
        self.emit_adapters_updated()?;

        let runtime = Arc::downgrade(self);
        let effect = EffectHandle::synchronous("llm.register_adapter()", move || {
            if let Some(runtime) = runtime.upgrade() {
                runtime.release_registration(owner);
            }
            Ok(())
        });
        if let Err(error) = self.context.own(effect.clone()) {
            self.release_registration(owner);
            return Err(error.into());
        }
        Ok(AdapterRegistrationHandle {
            runtime: Arc::downgrade(self),
            owner,
            adapter,
            effect,
            disposed: AtomicBool::new(false),
        })
    }

    fn prepare_routes(
        &self,
        providers: &[String],
        adapter: &Arc<dyn LlmAdapter>,
        owner: Uuid,
        current_owner: Option<Uuid>,
    ) -> anyhow::Result<Vec<AdapterRegistration>> {
        let mut unique = HashSet::new();
        let mut registrations = Vec::with_capacity(providers.len());
        for provider in providers {
            if provider.is_empty() {
                return Err(llm_error(
                    "adapter provider names must be non-empty",
                    "INVALID_ADAPTER",
                ));
            }
            if !unique.insert(provider.clone()) {
                return Err(llm_error(
                    format!("an adapter for provider \"{provider}\" is already registered"),
                    "DUPLICATE_ADAPTER",
                ));
            }
            if self
                .state
                .lock()
                .adapters
                .get(provider)
                .is_some_and(|registration| Some(registration.owner) != current_owner)
            {
                return Err(llm_error(
                    format!("an adapter for provider \"{provider}\" is already registered"),
                    "DUPLICATE_ADAPTER",
                ));
            }
            let info = adapter.provider_info(provider);
            if info.id != *provider || info.name.is_empty() {
                return Err(llm_error(
                    format!(
                        "adapter metadata for provider \"{provider}\" must preserve its id and have a non-empty name"
                    ),
                    "INVALID_ADAPTER",
                ));
            }
            let retry_policy = adapter.provider_retry_policy(provider).map_or_else(
                || resolve_retry_policy(None, &format!("llm: provider \"{provider}\" retryPolicy")),
                Ok,
            )?;
            registrations.push(AdapterRegistration {
                owner,
                adapter: adapter.clone(),
                provider: info,
                retry_policy,
            });
        }
        Ok(registrations)
    }

    fn replace_routes(
        &self,
        owner: Uuid,
        adapter: &Arc<dyn LlmAdapter>,
        providers: &[String],
    ) -> anyhow::Result<()> {
        if !self.state.lock().registrations.contains(&owner) {
            return Err(llm_error(
                "a disposed adapter registration cannot replace its routes",
                "REGISTRATION_DISPOSED",
            ));
        }
        let prepared = self.prepare_routes(providers, adapter, owner, Some(owner))?;
        {
            let mut state = self.state.lock();
            ensure_routes_available(&state, &prepared, Some(owner))?;
            state
                .adapters
                .retain(|_, registration| registration.owner != owner);
            for registration in prepared {
                state
                    .adapters
                    .insert(registration.provider.id.clone(), registration);
            }
        }
        self.emit_adapters_updated()
    }

    fn release_registration(&self, owner: Uuid) {
        let changed = {
            let mut state = self.state.lock();
            if state.registrations.remove(&owner) {
                state
                    .adapters
                    .retain(|_, registration| registration.owner != owner);
                true
            } else {
                false
            }
        };
        if changed && let Err(error) = self.emit_adapters_updated() {
            tracing::warn!(%error, "llm/adapters-updated dispatch failed during disposal");
        }
    }

    fn emit_adapters_updated(&self) -> anyhow::Result<()> {
        let emission = self.context.events().prepare_emit(
            &self.context,
            "llm/adapters-updated",
            &EventArgs::new(),
        )?;
        let mut invariant = None;
        emission.emit_contained(|error| {
            let code = error
                .downcast_ref::<LlmError>()
                .map(LlmError::code)
                .or_else(|| {
                    error
                        .downcast_ref::<crate::HarnessError>()
                        .map(crate::HarnessError::code)
                });
            if code == Some("INVARIANT") && invariant.is_none() {
                invariant = Some(error);
            } else {
                tracing::warn!(%error, "llm/adapters-updated listener failed");
            }
        });
        invariant.map_or(Ok(()), Err)
    }

    /// Lists detached provider display metadata in registration order.
    #[must_use]
    pub fn list_providers(&self) -> Vec<LlmProviderInfo> {
        self.state
            .lock()
            .adapters
            .values()
            .map(|registration| registration.provider.clone())
            .collect()
    }

    /// Registers waterfall middleware around every stream call.
    ///
    /// The continuation accepts the possibly routed replacement request.
    ///
    /// # Errors
    ///
    /// Returns when the owning context is inactive.
    pub fn register_stream_middleware(
        &self,
        owner: &Context,
        middleware: LlmStreamMiddleware,
        prepend: bool,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        let id = Uuid::now_v7();
        let entry = StreamMiddlewareEntry { id, middleware };
        if prepend {
            self.state.lock().stream_middlewares.insert(0, entry);
        } else {
            self.state.lock().stream_middlewares.push(entry);
        }
        let state = self.state.clone();
        let effect = EffectHandle::synchronous("llm stream middleware", move || {
            state
                .lock()
                .stream_middlewares
                .retain(|entry| entry.id != id);
            Ok(())
        });
        match owner.own(effect.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                self.state
                    .lock()
                    .stream_middlewares
                    .retain(|entry| entry.id != id);
                Err(error)
            }
        }
    }

    /// Registers a non-empty configurable-provider directory contribution.
    ///
    /// # Errors
    ///
    /// Returns invalid, duplicate, topology, or inactive-owner failures.
    pub fn register_configurable_providers(
        self: &Arc<Self>,
        entries: &[LlmConfigurableProvider],
    ) -> anyhow::Result<DirectoryRegistrationHandle> {
        if entries.is_empty() {
            return Err(llm_error(
                "a configurable-provider registration must declare at least one provider",
                "INVALID_DIRECTORY",
            ));
        }
        let owner = Uuid::now_v7();
        self.commit_directory(owner, entries, None)?;
        let runtime = Arc::downgrade(self);
        let effect =
            EffectHandle::synchronous("llm.register_configurable_providers()", move || {
                if let Some(runtime) = runtime.upgrade() {
                    runtime.release_directory(owner);
                }
                Ok(())
            });
        if let Err(error) = self.context.own(effect.clone()) {
            self.release_directory(owner);
            return Err(error.into());
        }
        Ok(DirectoryRegistrationHandle {
            runtime: Arc::downgrade(self),
            owner,
            effect,
        })
    }

    fn commit_directory(
        &self,
        owner: Uuid,
        candidates: &[LlmConfigurableProvider],
        current_owner: Option<Uuid>,
    ) -> anyhow::Result<()> {
        let mut unique = HashSet::new();
        for entry in candidates {
            if entry.provider.is_empty()
                || entry.display_name.is_empty()
                || entry.settings_ns.is_empty()
            {
                return Err(llm_error(
                    "configurable providers need a non-empty provider, displayName, and settingsNs",
                    "INVALID_DIRECTORY",
                ));
            }
            if entry.settings_path.iter().any(String::is_empty) {
                return Err(llm_error(
                    format!(
                        "configurable provider \"{}\" has an empty settingsPath segment",
                        entry.provider
                    ),
                    "INVALID_DIRECTORY",
                ));
            }
            if !unique.insert(entry.provider.clone())
                || self
                    .state
                    .lock()
                    .directory
                    .get(&entry.provider)
                    .is_some_and(|held| Some(held.owner) != current_owner)
            {
                return Err(llm_error(
                    format!(
                        "configurable provider \"{}\" is already declared",
                        entry.provider
                    ),
                    "DUPLICATE_DIRECTORY",
                ));
            }
        }
        {
            let mut state = self.state.lock();
            if current_owner.is_some() && !state.directory_registrations.contains(&owner) {
                return Err(llm_error(
                    "this configurable-provider registration was disposed",
                    "REGISTRATION_DISPOSED",
                ));
            }
            state.directory.retain(|_, held| held.owner != owner);
            for value in candidates {
                state.directory.insert(
                    value.provider.clone(),
                    DirectoryEntry {
                        owner,
                        value: value.clone(),
                    },
                );
            }
            state.directory_registrations.insert(owner);
        }
        self.emit_adapters_updated()
    }

    fn release_directory(&self, owner: Uuid) {
        let changed = {
            let mut state = self.state.lock();
            if state.directory_registrations.remove(&owner) {
                state.directory.retain(|_, entry| entry.owner != owner);
                true
            } else {
                false
            }
        };
        if changed && let Err(error) = self.emit_adapters_updated() {
            tracing::warn!(%error, "llm directory topology dispatch failed during disposal");
        }
    }

    /// Lists detached configurable-provider entries in declaration order.
    #[must_use]
    pub fn list_configurable_providers(&self) -> Vec<LlmConfigurableProvider> {
        self.state
            .lock()
            .directory
            .values()
            .map(|entry| entry.value.clone())
            .collect()
    }

    /// Registers one model-discovery function by settings namespace.
    ///
    /// # Errors
    ///
    /// Returns invalid, duplicate, or inactive-owner failures.
    pub fn register_model_discovery<F>(
        self: &Arc<Self>,
        settings_ns: &str,
        discover: F,
    ) -> anyhow::Result<ModelDiscoveryHandle>
    where
        F: Fn(
                LlmModelDiscoveryRequest,
            ) -> BoxFuture<'static, anyhow::Result<Vec<LlmDiscoveredModel>>>
            + Send
            + Sync
            + 'static,
    {
        if settings_ns.is_empty() {
            return Err(llm_error(
                "model discovery needs a non-empty settings namespace",
                "INVALID_DISCOVERY",
            ));
        }
        let owner = Uuid::now_v7();
        {
            let mut state = self.state.lock();
            if state.discoveries.contains_key(settings_ns) {
                return Err(llm_error(
                    format!("model discovery for \"{settings_ns}\" is already registered"),
                    "DUPLICATE_DISCOVERY",
                ));
            }
            state.discoveries.insert(
                settings_ns.to_owned(),
                DiscoveryEntry {
                    owner,
                    discover: Arc::new(discover),
                },
            );
        }
        let namespace = settings_ns.to_owned();
        let runtime = Arc::downgrade(self);
        let effect = EffectHandle::synchronous("llm.register_model_discovery()", move || {
            if let Some(runtime) = runtime.upgrade() {
                let mut state = runtime.state.lock();
                if state
                    .discoveries
                    .get(&namespace)
                    .is_some_and(|entry| entry.owner == owner)
                {
                    state.discoveries.remove(&namespace);
                }
            }
            Ok(())
        });
        if let Err(error) = self.context.own(effect.clone()) {
            futures::executor::block_on(effect.dispose()).ok();
            return Err(error.into());
        }
        Ok(ModelDiscoveryHandle { effect })
    }

    /// Interrogates one draft provider endpoint and normalizes its model list.
    ///
    /// # Errors
    ///
    /// Returns missing-discovery, invalid-request, or adapter failures.
    pub async fn discover_models(
        &self,
        settings_ns: &str,
        request: LlmModelDiscoveryRequest,
    ) -> anyhow::Result<Vec<LlmDiscoveredModel>> {
        let discovery = self
            .state
            .lock()
            .discoveries
            .get(settings_ns)
            .cloned()
            .ok_or_else(|| {
                llm_error(
                    format!("no model discovery is registered for \"{settings_ns}\""),
                    "NO_DISCOVERY",
                )
            })?;
        if request.provider.as_deref().unwrap_or_default().is_empty()
            && request.base_url.as_deref().unwrap_or_default().is_empty()
        {
            return Err(llm_error(
                "model discovery needs a provider route or a baseURL",
                "INVALID_DISCOVERY",
            ));
        }
        let discovered = (discovery.discover)(request).await?;
        let mut seen = HashSet::new();
        Ok(discovered
            .into_iter()
            .filter(|model| !model.id.is_empty() && seen.insert(model.id.clone()))
            .collect())
    }

    /// Returns the immutable policy captured with one provider route.
    ///
    /// # Errors
    ///
    /// Returns `NO_ADAPTER` for an unknown provider.
    pub fn provider_retry_policy(&self, provider: &str) -> anyhow::Result<ResolvedRetryPolicy> {
        Ok(self.registration(provider)?.retry_policy)
    }

    /// Returns a validated detached advisory model catalog.
    ///
    /// # Errors
    ///
    /// Returns route, adapter, or invalid-catalog failures.
    pub async fn list_models(&self, provider: &str) -> anyhow::Result<Vec<LlmModelInfo>> {
        let registration = self.registration(provider)?;
        let models = AssertUnwindSafe(registration.adapter.list_models(provider))
            .catch_unwind()
            .await
            .map_err(|payload| panic_as_error(payload.as_ref()))??;
        let mut seen = HashSet::new();
        for model in &models {
            if model.provider != provider
                || model.id.is_empty()
                || model.name.is_empty()
                || !seen.insert(model.id.clone())
            {
                return Err(llm_error(
                    format!(
                        "adapter returned invalid or duplicate model metadata for provider \"{provider}\""
                    ),
                    "INVALID_CATALOG",
                ));
            }
        }
        Ok(models)
    }

    /// Resolves and validates metadata for one exact model.
    ///
    /// # Errors
    ///
    /// Returns route, adapter, identity, capacity, or reasoning metadata failures.
    pub async fn resolve_model_info(
        &self,
        provider: &str,
        model: &str,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LlmResolvedModelInfo> {
        let registration = self.registration(provider)?;
        self.resolve_model_info_for(&registration, model, signal)
            .await
    }

    async fn resolve_model_info_for(
        &self,
        registration: &AdapterRegistration,
        model: &str,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LlmResolvedModelInfo> {
        let provider = &registration.provider.id;
        let info = AssertUnwindSafe(registration.adapter.resolve_model(provider, model, signal))
            .catch_unwind()
            .await
            .map_err(|payload| panic_as_error(payload.as_ref()))??;
        if info.provider != *provider || info.id != model || info.name.is_empty() {
            return Err(llm_error(
                format!(
                    "adapter returned invalid exact model metadata for provider \"{provider}\" model \"{model}\""
                ),
                "INVALID_MODEL_INFO",
            ));
        }
        if info
            .context
            .as_ref()
            .is_some_and(|context| context.context_window == 0)
        {
            return Err(llm_error(
                format!(
                    "adapter returned invalid context metadata for provider \"{provider}\" model \"{model}\""
                ),
                "INVALID_MODEL_CONTEXT",
            ));
        }
        if info.default_max_tokens == Some(0) {
            return Err(llm_error(
                format!(
                    "adapter returned invalid default maxTokens for provider \"{provider}\" model \"{model}\""
                ),
                "INVALID_MODEL_MAX_TOKENS",
            ));
        }
        validate_reasoning(provider, model, &info)?;
        Ok(info)
    }

    /// Resolves adapter-owned defaults for a call configuration.
    ///
    /// # Errors
    ///
    /// Returns exact-model or unsupported-reasoning failures.
    pub async fn resolve_call_config(
        &self,
        config: &LlmCallConfig,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<LlmCallConfig> {
        let registration = self.registration(&config.provider)?;
        Ok(self
            .resolve_call_for(&registration, config, signal)
            .await?
            .0)
    }

    async fn resolve_call_for(
        &self,
        registration: &AdapterRegistration,
        config: &LlmCallConfig,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<(LlmCallConfig, Option<LlmModelContext>)> {
        let info = self
            .resolve_model_info_for(registration, &config.model, signal)
            .await?;
        let mut resolved = config.clone();
        if resolved.max_tokens.is_none() {
            resolved.max_tokens = info.default_max_tokens;
        }
        match (&info.reasoning, &resolved.reasoning_effort) {
            (None, Some(effort)) => {
                return Err(unsupported_reasoning(config, effort));
            }
            (Some(reasoning), requested) => {
                let effective = requested
                    .clone()
                    .or_else(|| reasoning.default_effort.clone());
                if let Some(effective) = effective {
                    if !reasoning.efforts.iter().any(|item| item.id == effective) {
                        return Err(unsupported_reasoning(config, &effective));
                    }
                    resolved.reasoning_effort = Some(effective);
                }
            }
            (None, None) => {}
        }
        Ok((resolved, info.context))
    }

    /// Resolves one one-shot registration-bound call.
    ///
    /// # Errors
    ///
    /// Returns route or exact-model resolution failures.
    pub async fn prepare_call(
        self: &Arc<Self>,
        config: &LlmCallConfig,
        signal: Option<&AbortSignal>,
    ) -> anyhow::Result<PreparedLlmCall> {
        let registration = self.registration(&config.provider)?;
        let (resolved, context) = self.resolve_call_for(&registration, config, signal).await?;
        let adapter_defaults = LlmCallConfigAdapterDefaults {
            reasoning_effort: (config.reasoning_effort.is_none()
                && resolved.reasoning_effort.is_some())
            .then_some(true),
            max_tokens: (config.max_tokens.is_none() && resolved.max_tokens.is_some())
                .then_some(true),
        };
        Ok(PreparedLlmCall {
            runtime: self.clone(),
            retry_policy: registration.retry_policy.clone(),
            registration,
            config: resolved,
            context,
            adapter_defaults,
            dispatched: AtomicBool::new(false),
        })
    }

    fn registration(&self, provider: &str) -> anyhow::Result<AdapterRegistration> {
        self.state
            .lock()
            .adapters
            .get(provider)
            .cloned()
            .ok_or_else(|| {
                llm_error(
                    format!("no adapter registered for provider \"{provider}\""),
                    "NO_ADAPTER",
                )
            })
    }

    /// Streams one call, normalizing adapter failures into terminal chunks.
    #[must_use]
    pub fn stream(self: &Arc<Self>, options: GenerateOptions) -> LlmStream {
        self.stream_with_registration(options, None)
    }

    fn stream_with_registration(
        self: &Arc<Self>,
        options: GenerateOptions,
        prepared: Option<(AdapterRegistration, LlmCallConfig)>,
    ) -> LlmStream {
        let middlewares: Arc<[StreamMiddlewareEntry]> =
            self.state.lock().stream_middlewares.clone().into();
        let stream = build_middleware_chain(self, &middlewares, 0, options, prepared);
        if self.stream_invariants.load(Ordering::Acquire) > 0 {
            crate::invariant::validate_stream(stream)
        } else {
            stream
        }
    }

    fn adapter_stream_with_registration(
        self: &Arc<Self>,
        options: GenerateOptions,
        prepared: Option<(AdapterRegistration, LlmCallConfig)>,
    ) -> LlmStream {
        let runtime = self.clone();
        Box::pin(stream! {
            let setup = runtime.prepare_adapter_stream(&options, prepared).await;
            let mut adapter_stream = match setup {
                Ok(stream) => stream,
                Err(error) => {
                    yield Ok(adapter_failure_chunk(&error, options.signal.as_ref()));
                    return;
                }
            };
            loop {
                let next = AssertUnwindSafe(adapter_stream.next()).catch_unwind().await;
                match next {
                    Ok(Some(Ok(chunk))) => yield Ok(chunk),
                    Ok(Some(Err(error))) => {
                        yield Ok(adapter_failure_chunk(&error, options.signal.as_ref()));
                        return;
                    }
                    Ok(None) => return,
                    Err(payload) => {
                        let error = panic_as_error(payload.as_ref());
                        yield Ok(adapter_failure_chunk(&error, options.signal.as_ref()));
                        return;
                    }
                }
            }
        })
    }

    /// Enables stream-grammar invariants until the returned effect is disposed.
    ///
    /// # Errors
    ///
    /// Returns when the owning context is inactive.
    pub fn enable_stream_invariants(&self) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        self.stream_invariants.fetch_add(1, Ordering::AcqRel);
        let count = self.stream_invariants.clone();
        let effect = EffectHandle::synchronous("llm stream invariants", move || {
            count.fetch_sub(1, Ordering::AcqRel);
            Ok(())
        });
        match self.context.own(effect.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                self.stream_invariants.fetch_sub(1, Ordering::AcqRel);
                Err(error)
            }
        }
    }

    async fn prepare_adapter_stream(
        &self,
        options: &GenerateOptions,
        prepared: Option<(AdapterRegistration, LlmCallConfig)>,
    ) -> anyhow::Result<AdapterStream> {
        let registration = match &prepared {
            Some((registration, _)) => registration.clone(),
            None => self.registration(&options.provider)?,
        };
        let resolved = match &prepared {
            Some((_, config)) => config.clone(),
            None => {
                self.resolve_call_for(&registration, &config_of(options), options.signal.as_ref())
                    .await?
                    .0
            }
        };
        if prepared.is_some() && !call_config_equals(&config_of(options), &resolved) {
            return Err(llm_error(
                "prepared LLM call config changed before adapter dispatch",
                "INVALID_PREPARED_CALL",
            ));
        }
        let resolved_options = apply_config(options.clone(), &resolved);
        let resolved_options = self.for_adapter(resolved_options, &registration.adapter);
        catch_unwind(AssertUnwindSafe(|| {
            registration.adapter.stream(resolved_options)
        }))
        .map_err(|payload| panic_as_error(payload.as_ref()))
    }

    fn for_adapter(
        &self,
        mut options: GenerateOptions,
        adapter: &Arc<dyn LlmAdapter>,
    ) -> GenerateOptions {
        for message in &mut options.messages {
            if message.role != MessageRole::Assistant
                || message.source.kind != "model"
                || !message.source.fields.contains_key("replayState")
            {
                continue;
            }
            let provider = message
                .source
                .fields
                .get("provider")
                .and_then(serde_json::Value::as_str);
            let same_adapter = provider
                .and_then(|provider| self.state.lock().adapters.get(provider).cloned())
                .is_some_and(|registration| Arc::ptr_eq(&registration.adapter, adapter));
            if !same_adapter {
                message.source.fields.shift_remove("replayState");
            }
        }
        options
    }
}

fn build_middleware_chain(
    runtime: &Arc<LlmRuntime>,
    middlewares: &Arc<[StreamMiddlewareEntry]>,
    index: usize,
    options: GenerateOptions,
    prepared: Option<(AdapterRegistration, LlmCallConfig)>,
) -> LlmStream {
    let Some(entry) = middlewares.get(index).cloned() else {
        return runtime.adapter_stream_with_registration(options, prepared);
    };

    let next_runtime = runtime.clone();
    let next_middlewares = middlewares.clone();
    let next = Box::new(move |next_options| {
        build_middleware_chain(
            &next_runtime,
            &next_middlewares,
            index + 1,
            next_options,
            prepared,
        )
    });
    match catch_unwind(AssertUnwindSafe(|| (entry.middleware)(options, next))) {
        Ok(stream) => stream,
        Err(payload) => {
            let error = panic_as_error(payload.as_ref());
            Box::pin(futures::stream::once(async move { Err(error) }))
        }
    }
}

fn ensure_routes_available(
    state: &RuntimeState,
    prepared: &[AdapterRegistration],
    current_owner: Option<Uuid>,
) -> anyhow::Result<()> {
    for registration in prepared {
        let provider = &registration.provider.id;
        if state
            .adapters
            .get(provider)
            .is_some_and(|existing| Some(existing.owner) != current_owner)
        {
            return Err(llm_error(
                format!("an adapter for provider \"{provider}\" is already registered"),
                "DUPLICATE_ADAPTER",
            ));
        }
    }
    Ok(())
}

fn validate_reasoning(
    provider: &str,
    model: &str,
    info: &LlmResolvedModelInfo,
) -> anyhow::Result<()> {
    let Some(reasoning) = &info.reasoning else {
        return Ok(());
    };
    if reasoning.efforts.is_empty() {
        return Err(invalid_reasoning(provider, model));
    }
    let mut seen = HashSet::new();
    if reasoning.efforts.iter().any(|effort| {
        effort.id.as_str().is_empty()
            || effort.name.is_empty()
            || !seen.insert(effort.id.as_str().to_owned())
    }) {
        return Err(invalid_reasoning(provider, model));
    }
    if reasoning
        .default_effort
        .as_ref()
        .is_some_and(|default| !seen.contains(default.as_str()))
    {
        return Err(llm_error(
            format!(
                "adapter returned an unknown default reasoning effort for provider \"{provider}\" model \"{model}\""
            ),
            "INVALID_MODEL_REASONING",
        ));
    }
    Ok(())
}

fn invalid_reasoning(provider: &str, model: &str) -> anyhow::Error {
    llm_error(
        format!(
            "adapter returned invalid or duplicate reasoning effort metadata for provider \"{provider}\" model \"{model}\""
        ),
        "INVALID_MODEL_REASONING",
    )
}

fn unsupported_reasoning(
    config: &LlmCallConfig,
    effort: &crate::ReasoningEffortId,
) -> anyhow::Error {
    llm_error(
        format!(
            "provider \"{}\" model \"{}\" does not support reasoning effort \"{effort}\"",
            config.provider, config.model
        ),
        "UNSUPPORTED_REASONING_EFFORT",
    )
}

fn config_of(options: &GenerateOptions) -> LlmCallConfig {
    LlmCallConfig {
        provider: options.provider.clone(),
        model: options.model.clone(),
        reasoning_effort: options.reasoning_effort.clone(),
        temperature: options.temperature,
        max_tokens: options.max_tokens,
        stop: options.stop.clone(),
    }
}

fn apply_config(mut options: GenerateOptions, config: &LlmCallConfig) -> GenerateOptions {
    options.provider.clone_from(&config.provider);
    options.model.clone_from(&config.model);
    options
        .reasoning_effort
        .clone_from(&config.reasoning_effort);
    options.temperature = config.temperature;
    options.max_tokens = config.max_tokens;
    options.stop.clone_from(&config.stop);
    options
}

fn adapter_failure_chunk(error: &anyhow::Error, signal: Option<&AbortSignal>) -> StreamChunk {
    let failure = normalize_llm_failure(error);
    let aborted = signal.is_some_and(AbortSignal::is_aborted) || failure.code == "ABORTED";
    StreamChunk::Finish {
        reason: if aborted {
            FinishReason::Aborted { failure }
        } else {
            FinishReason::Error { failure }
        },
        replay_state: None,
    }
}

fn panic_as_error(payload: &(dyn std::any::Any + Send)) -> anyhow::Error {
    let message = payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("adapter panicked");
    anyhow::anyhow!(message.to_owned())
}

fn llm_error(message: impl Into<String>, code: impl Into<String>) -> anyhow::Error {
    LlmError::simple(message, code).into()
}

/// A live adapter registration with atomic route replacement.
pub struct AdapterRegistrationHandle {
    runtime: Weak<LlmRuntime>,
    owner: Uuid,
    adapter: Arc<dyn LlmAdapter>,
    effect: EffectHandle,
    disposed: AtomicBool,
}

impl std::fmt::Debug for AdapterRegistrationHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AdapterRegistrationHandle")
            .field("owner", &self.owner)
            .field("disposed", &self.disposed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl AdapterRegistrationHandle {
    /// Atomically replaces the complete route set.
    ///
    /// # Errors
    ///
    /// Returns validation, collision, topology, or disposed-registration failures.
    pub fn replace(&self, providers: &[String]) -> anyhow::Result<()> {
        let runtime = self.runtime.upgrade().ok_or_else(|| {
            llm_error(
                "a disposed adapter registration cannot replace its routes",
                "REGISTRATION_DISPOSED",
            )
        })?;
        runtime.replace_routes(self.owner, &self.adapter, providers)
    }

    /// Releases every route currently held by this registration.
    ///
    /// # Errors
    ///
    /// Returns a cleanup failure shared with the owning fiber.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        self.disposed.store(true, Ordering::Release);
        self.effect.dispose().await
    }
}

/// A live configurable-provider contribution.
#[derive(Debug)]
pub struct DirectoryRegistrationHandle {
    runtime: Weak<LlmRuntime>,
    owner: Uuid,
    effect: EffectHandle,
}

impl DirectoryRegistrationHandle {
    /// Atomically replaces this contribution, including with an empty set.
    ///
    /// # Errors
    ///
    /// Returns validation, collision, topology, or disposed-registration failures.
    pub fn replace(&self, entries: &[LlmConfigurableProvider]) -> anyhow::Result<()> {
        let runtime = self.runtime.upgrade().ok_or_else(|| {
            llm_error(
                "this configurable-provider registration was disposed",
                "REGISTRATION_DISPOSED",
            )
        })?;
        runtime.commit_directory(self.owner, entries, Some(self.owner))
    }

    /// Withdraws this contribution once.
    ///
    /// # Errors
    ///
    /// Returns a cleanup failure shared with the owning fiber.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        self.effect.dispose().await
    }
}

/// Disposable model-discovery registration.
#[derive(Debug)]
pub struct ModelDiscoveryHandle {
    effect: EffectHandle,
}

impl ModelDiscoveryHandle {
    /// Withdraws this namespace offer once.
    ///
    /// # Errors
    ///
    /// Returns a cleanup failure shared with the owning fiber.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        self.effect.dispose().await
    }
}

/// One one-shot call bound to the adapter registration used for resolution.
pub struct PreparedLlmCall {
    runtime: Arc<LlmRuntime>,
    registration: AdapterRegistration,
    /// Resolved call config.
    pub config: LlmCallConfig,
    /// Captured provider retry policy.
    pub retry_policy: ResolvedRetryPolicy,
    /// Exact-model context metadata.
    pub context: Option<LlmModelContext>,
    /// Fields defaulted by the adapter.
    pub adapter_defaults: LlmCallConfigAdapterDefaults,
    dispatched: AtomicBool,
}

impl PreparedLlmCall {
    /// Dispatches this prepared call exactly once.
    ///
    /// # Errors
    ///
    /// Returns `INVALID_PREPARED_CALL` on reuse or config drift.
    pub fn stream(&self, options: GenerateOptions) -> anyhow::Result<LlmStream> {
        if self.dispatched.load(Ordering::Acquire) {
            return Err(llm_error(
                "a prepared LLM call can only be dispatched once",
                "INVALID_PREPARED_CALL",
            ));
        }
        if !call_config_equals(&config_of(&options), &self.config) {
            return Err(llm_error(
                "prepared LLM call config changed before adapter dispatch",
                "INVALID_PREPARED_CALL",
            ));
        }
        if self
            .dispatched
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(llm_error(
                "a prepared LLM call can only be dispatched once",
                "INVALID_PREPARED_CALL",
            ));
        }
        Ok(self.runtime.stream_with_registration(
            options,
            Some((self.registration.clone(), self.config.clone())),
        ))
    }
}

#[cfg(test)]
mod tests {
    use futures::stream;

    use super::*;

    #[derive(Debug)]
    struct EchoAdapter;

    #[async_trait]
    impl LlmAdapter for EchoAdapter {
        fn stream(&self, _options: GenerateOptions) -> AdapterStream {
            Box::pin(stream::iter(vec![Ok(StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            })]))
        }
    }

    #[derive(Debug)]
    struct DefaultingAdapter;

    #[async_trait]
    impl LlmAdapter for DefaultingAdapter {
        async fn resolve_model(
            &self,
            provider: &str,
            model: &str,
            _signal: Option<&AbortSignal>,
        ) -> anyhow::Result<LlmResolvedModelInfo> {
            Ok(LlmResolvedModelInfo {
                provider: provider.to_owned(),
                id: model.to_owned(),
                name: model.to_owned(),
                description: None,
                input_modalities: None,
                context: Some(LlmModelContext {
                    context_window: 128_000,
                }),
                default_max_tokens: Some(8_192),
                reasoning: Some(crate::LlmModelReasoningInfo {
                    efforts: vec![crate::LlmReasoningEffortInfo {
                        id: crate::ReasoningEffortId::new("high"),
                        name: "High".to_owned(),
                        description: None,
                    }],
                    default_effort: Some(crate::ReasoningEffortId::new("high")),
                }),
            })
        }

        fn stream(&self, _options: GenerateOptions) -> AdapterStream {
            Box::pin(stream::iter(vec![Ok(StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            })]))
        }
    }

    fn options(provider: &str) -> GenerateOptions {
        GenerateOptions {
            provider: provider.to_owned(),
            model: "m".to_owned(),
            reasoning_effort: None,
            messages: Vec::new(),
            system: None,
            tools: None,
            temperature: None,
            max_tokens: None,
            stop: None,
            signal: None,
            session_id: None,
            purpose: None,
        }
    }

    #[tokio::test]
    async fn registration_routes_and_disposes() {
        let context = Context::new();
        let runtime = LlmRuntime::install(&context).expect("runtime");
        let handle = runtime
            .register_adapter(&["mock".to_owned()], Arc::new(EchoAdapter))
            .expect("register");
        assert_eq!(runtime.list_providers()[0].id, "mock");
        let chunks = runtime
            .stream(options("mock"))
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<anyhow::Result<Vec<_>>>()
            .expect("stream");
        assert!(matches!(
            chunks.as_slice(),
            [StreamChunk::Finish {
                reason: FinishReason::Stop,
                ..
            }]
        ));
        handle.dispose().await.expect("dispose");
        assert!(runtime.list_providers().is_empty());
    }

    #[tokio::test]
    async fn unknown_route_becomes_terminal_failure() {
        let context = Context::new();
        let runtime = LlmRuntime::install(&context).expect("runtime");
        let chunks = runtime
            .stream(options("missing"))
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<anyhow::Result<Vec<_>>>()
            .expect("normalized stream");
        let [
            StreamChunk::Finish {
                reason: FinishReason::Error { failure },
                ..
            },
        ] = chunks.as_slice()
        else {
            panic!("one terminal error");
        };
        assert_eq!(failure.code, "NO_ADAPTER");
    }

    #[tokio::test]
    async fn stream_middleware_can_route_and_disposal_withdraws_it() {
        let context = Context::new();
        let runtime = LlmRuntime::install(&context).expect("runtime");
        runtime
            .register_adapter(&["routed".to_owned()], Arc::new(EchoAdapter))
            .expect("register");
        let middleware = runtime
            .register_stream_middleware(
                &context,
                Arc::new(|mut request, next| {
                    request.provider = "routed".to_owned();
                    next(request)
                }),
                false,
            )
            .expect("middleware");

        let chunks = runtime
            .stream(options("unrouted"))
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<anyhow::Result<Vec<_>>>()
            .expect("routed stream");
        assert!(matches!(
            chunks.as_slice(),
            [StreamChunk::Finish {
                reason: FinishReason::Stop,
                ..
            }]
        ));

        middleware.dispose().await.expect("dispose middleware");
        let chunks = runtime
            .stream(options("unrouted"))
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<anyhow::Result<Vec<_>>>()
            .expect("normalized stream");
        let [
            StreamChunk::Finish {
                reason: FinishReason::Error { failure },
                ..
            },
        ] = chunks.as_slice()
        else {
            panic!("one terminal error");
        };
        assert_eq!(failure.code, "NO_ADAPTER");
    }

    #[tokio::test]
    async fn prepared_call_materializes_defaults_and_is_one_shot() {
        let context = Context::new();
        let runtime = LlmRuntime::install(&context).expect("runtime");
        runtime
            .register_adapter(&["route".to_owned()], Arc::new(DefaultingAdapter))
            .expect("register");
        let prepared = runtime
            .prepare_call(&config_of(&options("route")), None)
            .await
            .expect("prepare");
        assert_eq!(prepared.config.max_tokens, Some(8_192));
        assert_eq!(
            prepared
                .config
                .reasoning_effort
                .as_ref()
                .map(crate::ReasoningEffortId::as_str),
            Some("high")
        );
        assert_eq!(prepared.adapter_defaults.max_tokens, Some(true));
        assert_eq!(prepared.adapter_defaults.reasoning_effort, Some(true));
        let mut request = options("route");
        request.max_tokens = Some(8_192);
        request.reasoning_effort = Some(crate::ReasoningEffortId::new("high"));
        prepared
            .stream(request.clone())
            .expect("first dispatch")
            .collect::<Vec<_>>()
            .await;
        assert!(prepared.stream(request).is_err());
    }

    #[tokio::test]
    async fn directory_replacement_and_discovery_are_normalized() {
        let context = Context::new();
        let runtime = LlmRuntime::install(&context).expect("runtime");
        let entry = LlmConfigurableProvider {
            provider: "route".to_owned(),
            display_name: "Route".to_owned(),
            settings_ns: "llm-test".to_owned(),
            settings_path: vec!["providers".to_owned(), "route".to_owned()],
            authentication: crate::LlmProviderAuthentication::ProviderNative,
            declared: None,
        };
        let directory = runtime
            .register_configurable_providers(std::slice::from_ref(&entry))
            .expect("directory");
        assert_eq!(runtime.list_configurable_providers(), [entry]);
        directory.replace(&[]).expect("empty replacement");
        assert!(runtime.list_configurable_providers().is_empty());

        let discovery = runtime
            .register_model_discovery("llm-test", |_request| {
                Box::pin(async {
                    Ok(vec![
                        LlmDiscoveredModel {
                            id: "m".to_owned(),
                            name: None,
                            context_window: None,
                            max_tokens: None,
                        },
                        LlmDiscoveredModel {
                            id: "m".to_owned(),
                            name: Some("duplicate".to_owned()),
                            context_window: None,
                            max_tokens: None,
                        },
                        LlmDiscoveredModel {
                            id: String::new(),
                            name: None,
                            context_window: None,
                            max_tokens: None,
                        },
                    ])
                })
            })
            .expect("discovery");
        let models = runtime
            .discover_models(
                "llm-test",
                LlmModelDiscoveryRequest {
                    base_url: Some("https://example.invalid".to_owned()),
                    ..LlmModelDiscoveryRequest::default()
                },
            )
            .await
            .expect("models");
        assert_eq!(models.len(), 1);
        discovery.dispose().await.expect("dispose discovery");
    }
}
