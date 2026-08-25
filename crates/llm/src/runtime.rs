//! Adapter registry, exact-model resolution, and normalized streaming runtime.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context as TaskContext, Poll},
};

use async_stream::stream;
use async_trait::async_trait;
use futures::{FutureExt, Stream, StreamExt, future::BoxFuture};
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, EventArgs, ServiceKey, fiber::EffectHandle};
use uuid::Uuid;

use crate::{
    LlmError, MessageRole, ModelId, ProviderId,
    adapter_failure::{AdapterRejection, normalize_adapter_rejection, normalize_llm_failure},
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

/// Boxed fallible chunk iterator used when middleware wraps an existing LLM
/// stream while retaining its managed close state.
pub type BoxLlmChunkStream =
    Pin<Box<dyn Stream<Item = anyhow::Result<StreamChunk>> + Send + 'static>>;
type BoxAdapterChunkStream =
    Pin<Box<dyn Stream<Item = Result<StreamChunk, AdapterRejection>> + Send + 'static>>;
type CleanupFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'static>>;

/// One fallible asynchronous adapter-iterator cleanup operation.
pub type AdapterCleanup = Box<dyn FnOnce() -> CleanupFuture + Send + 'static>;

/// A fallible provider chunk stream before runtime normalization.
///
/// `with_cleanup` represents JavaScript async-iterator `return()`. The runtime
/// invokes it only for downstream early close, never after normal completion
/// or an adapter iteration failure.
pub struct AdapterStream {
    inner: BoxAdapterChunkStream,
    cleanup: Option<AdapterCleanup>,
}

impl AdapterStream {
    /// Boxes an adapter stream with no iterator-return operation.
    pub fn new<S>(stream: S) -> Self
    where
        S: Stream<Item = anyhow::Result<StreamChunk>> + Send + 'static,
    {
        Self {
            inner: Box::pin(stream.map(|result| result.map_err(AdapterRejection::Native))),
            cleanup: None,
        }
    }

    /// Boxes an adapter stream whose iterator can reject with an arbitrary
    /// compatibility-boundary value.
    pub fn from_rejections<S>(stream: S) -> Self
    where
        S: Stream<Item = Result<StreamChunk, AdapterRejection>> + Send + 'static,
    {
        Self {
            inner: Box::pin(stream),
            cleanup: None,
        }
    }

    /// Boxes an adapter stream with a fallible asynchronous close operation.
    pub fn with_cleanup<S, F, Fut>(stream: S, cleanup: F) -> Self
    where
        S: Stream<Item = anyhow::Result<StreamChunk>> + Send + 'static,
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = anyhow::Result<()>> + Send + 'static,
    {
        Self {
            inner: Box::pin(stream.map(|result| result.map_err(AdapterRejection::Native))),
            cleanup: Some(Box::new(move || Box::pin(cleanup()))),
        }
    }

    fn into_parts(self) -> (BoxAdapterChunkStream, Option<AdapterCleanup>) {
        (self.inner, self.cleanup)
    }
}

impl Stream for AdapterStream {
    type Item = Result<StreamChunk, AdapterRejection>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.inner.as_mut().poll_next(context)
    }
}

#[derive(Default)]
struct StreamCloseState {
    cleanup: Mutex<Option<AdapterCleanup>>,
    closed: AtomicBool,
}

impl StreamCloseState {
    fn install(&self, cleanup: Option<AdapterCleanup>) {
        let mut slot = self.cleanup.lock();
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        *slot = cleanup;
    }

    fn complete(&self) {
        self.closed.store(true, Ordering::Release);
        self.cleanup.lock().take();
    }

    fn take_for_close(&self) -> Option<AdapterCleanup> {
        self.closed.store(true, Ordering::Release);
        self.cleanup.lock().take()
    }
}

/// Provider-neutral stream whose adapter failures are terminal chunks while
/// middleware, invariant, consumer, and iterator-cleanup failures remain
/// errors.
pub struct LlmStream {
    inner: Option<BoxLlmChunkStream>,
    close: Arc<StreamCloseState>,
    close_on_drop: bool,
}

impl LlmStream {
    /// Boxes a stream which owns no adapter cleanup operation.
    pub fn new<S>(stream: S) -> Self
    where
        S: Stream<Item = anyhow::Result<StreamChunk>> + Send + 'static,
    {
        Self {
            inner: Some(Box::pin(stream)),
            close: Arc::new(StreamCloseState::default()),
            close_on_drop: true,
        }
    }

    fn with_close_state(stream: BoxLlmChunkStream, close: Arc<StreamCloseState>) -> Self {
        Self {
            inner: Some(stream),
            close,
            close_on_drop: true,
        }
    }

    /// Wraps the chunk iterator while preserving its asynchronous close path.
    #[must_use]
    pub fn wrap(mut self, wrapper: impl FnOnce(BoxLlmChunkStream) -> BoxLlmChunkStream) -> Self {
        let inner = self
            .inner
            .take()
            .unwrap_or_else(|| Box::pin(futures::stream::empty()));
        let close = self.close.clone();
        self.close_on_drop = false;
        Self::with_close_state(wrapper(inner), close)
    }

    /// Performs and awaits adapter iterator cleanup after downstream early
    /// close. Cleanup failures remain thrown to the closing caller.
    ///
    /// # Errors
    ///
    /// Returns the adapter's iterator-return failure.
    pub async fn close(&mut self) -> anyhow::Result<()> {
        self.close_on_drop = false;
        self.inner.take();
        match self.close.take_for_close() {
            Some(cleanup) => cleanup().await,
            None => Ok(()),
        }
    }
}

impl Stream for LlmStream {
    type Item = anyhow::Result<StreamChunk>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.inner
            .as_mut()
            .map_or(Poll::Ready(None), |inner| inner.as_mut().poll_next(context))
    }
}

impl Drop for LlmStream {
    fn drop(&mut self) {
        if !self.close_on_drop {
            return;
        }
        let Some(cleanup) = self.close.take_for_close() else {
            return;
        };
        let cleanup = cleanup();
        let run = async move {
            if let Err(error) = cleanup.await {
                tracing::warn!(%error, "LLM adapter cleanup failed after stream drop");
            }
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(run);
        } else {
            std::thread::spawn(move || futures::executor::block_on(run));
        }
    }
}

/// Continuation supplied to one stream middleware.
pub type LlmStreamNext = Box<dyn FnOnce(GenerateOptions) -> LlmStream + Send + 'static>;

/// Around-middleware for routing, retry, replay, and observation.
pub type LlmStreamMiddleware =
    Arc<dyn Fn(GenerateOptions, LlmStreamNext) -> LlmStream + Send + Sync + 'static>;

/// Exact provider/model route that reached the adapter dispatch boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LlmDispatchRoute {
    /// Dispatched provider route.
    pub provider: ProviderId,
    /// Dispatched model id.
    pub model: ModelId,
}

/// Per-call observation populated when middleware delegates to adapter dispatch.
#[derive(Clone, Default)]
pub struct LlmDispatchTrace(Arc<Mutex<Option<LlmDispatchRoute>>>);

impl std::fmt::Debug for LlmDispatchTrace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("LlmDispatchTrace")
            .field(&self.route())
            .finish()
    }
}

impl LlmDispatchTrace {
    /// Returns the last route admitted to this call's adapter boundary.
    #[must_use]
    pub fn route(&self) -> Option<LlmDispatchRoute> {
        self.0.lock().clone()
    }
}

/// Provider-wire adapter for `SeekDeep Harness`'s message and stream vocabulary.
#[async_trait]
pub trait LlmAdapter: Send + Sync + 'static {
    /// Describes one route owned by this adapter.
    fn provider_info(&self, provider: &str) -> LlmProviderInfo {
        LlmProviderInfo {
            id: ProviderId::new(provider),
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
            provider: ProviderId::new(provider),
            id: ModelId::new(model),
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
                    .insert(registration.provider.id.to_string(), registration);
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
            if info.id.as_str() != provider || info.name.is_empty() {
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
                    .insert(registration.provider.id.to_string(), registration);
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
        let on_async_error: Arc<dyn Fn(anyhow::Error) + Send + Sync> =
            Arc::new(|error| warn_adapters_listener_failure(&error));
        emission.emit_contained_with_async_errors(
            |error| {
                let code = error
                    .downcast_ref::<LlmError>()
                    .map(LlmError::code)
                    .or_else(|| {
                        error
                            .downcast_ref::<crate::HarnessError>()
                            .map(crate::HarnessError::code)
                    })
                    .or_else(|| {
                        error
                            .downcast_ref::<seekdeep_invariants::InvariantError>()
                            .map(|error| error.code)
                    });
                if code == Some("INVARIANT") && invariant.is_none() {
                    invariant = Some(error);
                } else {
                    warn_adapters_listener_failure(&error);
                }
            },
            &on_async_error,
        );
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
        if current_owner.is_some() && !self.state.lock().directory_registrations.contains(&owner) {
            return Err(llm_error(
                "this configurable-provider registration was disposed",
                "REGISTRATION_DISPOSED",
            ));
        }
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
            if !unique.insert(entry.provider.clone()) {
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
            if let Some(entry) = candidates.iter().find(|entry| {
                state
                    .directory
                    .get(entry.provider.as_str())
                    .is_some_and(|held| Some(held.owner) != current_owner)
            }) {
                return Err(llm_error(
                    format!(
                        "configurable provider \"{}\" is already declared",
                        entry.provider
                    ),
                    "DUPLICATE_DIRECTORY",
                ));
            }
            state.directory.retain(|_, held| held.owner != owner);
            for value in candidates {
                state.directory.insert(
                    value.provider.to_string(),
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
            if model.provider.as_str() != provider
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
        if info.provider.as_str() != provider.as_str()
            || info.id.as_str() != model
            || info.name.is_empty()
        {
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
        if info
            .default_max_tokens
            .is_some_and(|value| value == 0 || value > 9_007_199_254_740_991)
        {
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

    /// Streams one call and returns a call-local trace of the route that
    /// middleware ultimately delegated to adapter dispatch.
    #[must_use]
    pub fn stream_with_dispatch_trace(
        self: &Arc<Self>,
        options: GenerateOptions,
    ) -> (LlmStream, LlmDispatchTrace) {
        let trace = LlmDispatchTrace::default();
        let capture = trace.clone();
        let tracer = StreamMiddlewareEntry {
            id: Uuid::now_v7(),
            middleware: Arc::new(move |options, next| {
                *capture.0.lock() = Some(LlmDispatchRoute {
                    provider: options.provider.clone(),
                    model: options.model.clone(),
                });
                next(options)
            }),
        };
        let mut middlewares = self.state.lock().stream_middlewares.clone();
        middlewares.push(tracer);
        let middlewares: Arc<[StreamMiddlewareEntry]> = middlewares.into();
        (
            build_middleware_chain(self, &middlewares, 0, options, None),
            trace,
        )
    }

    fn stream_with_registration(
        self: &Arc<Self>,
        options: GenerateOptions,
        prepared: Option<(AdapterRegistration, LlmCallConfig)>,
    ) -> LlmStream {
        let middlewares: Arc<[StreamMiddlewareEntry]> =
            self.state.lock().stream_middlewares.clone().into();
        build_middleware_chain(self, &middlewares, 0, options, prepared)
    }

    fn adapter_stream_with_registration(
        self: &Arc<Self>,
        options: GenerateOptions,
        prepared: Option<(AdapterRegistration, LlmCallConfig)>,
    ) -> LlmStream {
        let runtime = self.clone();
        let close = Arc::new(StreamCloseState::default());
        let stream_close = close.clone();
        let stream = Box::pin(stream! {
            let setup = runtime.prepare_adapter_stream(&options, prepared).await;
            let (mut adapter_stream, cleanup) = match setup {
                Ok(stream) => stream.into_parts(),
                Err(error) => {
                    stream_close.complete();
                    yield Ok(adapter_failure_chunk(&error, options.signal.as_ref()));
                    return;
                }
            };
            stream_close.install(cleanup);
            loop {
                let next = AssertUnwindSafe(adapter_stream.next()).catch_unwind().await;
                match next {
                    Ok(Some(Ok(chunk))) => yield Ok(chunk),
                    Ok(Some(Err(error))) => {
                        stream_close.complete();
                        yield Ok(adapter_rejection_chunk(&error, options.signal.as_ref()));
                        return;
                    }
                    Ok(None) => {
                        stream_close.complete();
                        return;
                    }
                    Err(payload) => {
                        stream_close.complete();
                        let error = panic_as_error(payload.as_ref());
                        yield Ok(adapter_failure_chunk(&error, options.signal.as_ref()));
                        return;
                    }
                }
            }
        });
        LlmStream::with_close_state(stream, close)
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
        let unresolved = config_of(options);
        let resolved_options = if call_config_equals(&unresolved, &resolved) {
            options.clone_preserving_agent_loop_request()
        } else {
            apply_config(options.clone(), &resolved)
        };
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
        let mut rebuilt = false;
        for message in &mut options.messages {
            if message.role() != MessageRole::Assistant
                || message.source().kind != "model"
                || !message.source().fields.contains_key("replayState")
            {
                continue;
            }
            let provider = message
                .source()
                .fields
                .get("provider")
                .and_then(serde_json::Value::as_str);
            let same_adapter = provider
                .and_then(|provider| self.state.lock().adapters.get(provider).cloned())
                .is_some_and(|registration| Arc::ptr_eq(&registration.adapter, adapter));
            if !same_adapter {
                rebuilt = true;
                let mut detached = serde_json::Map::new();
                for key in ["provider", "model"] {
                    if let Some(value) = message.source().fields.get(key).cloned() {
                        detached.insert(key.to_owned(), value);
                    }
                }
                *message = message.clone().with_source(crate::MessageSource {
                    kind: "model".to_owned(),
                    fields: detached,
                });
            }
        }
        if rebuilt {
            options.clear_agent_loop_request();
        }
        options
    }
}

fn warn_adapters_listener_failure(error: &anyhow::Error) {
    tracing::warn!("llm: an llm/adapters-updated listener failed");
    tracing::warn!(%error);
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
            LlmStream::new(futures::stream::once(async move { Err(error) }))
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
            .get(provider.as_str())
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
    terminal_failure_chunk(failure, signal)
}

fn adapter_rejection_chunk(
    rejection: &AdapterRejection,
    signal: Option<&AbortSignal>,
) -> StreamChunk {
    let failure = normalize_adapter_rejection(rejection);
    terminal_failure_chunk(failure, signal)
}

fn terminal_failure_chunk(failure: crate::LlmFailure, signal: Option<&AbortSignal>) -> StreamChunk {
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
    config: LlmCallConfig,
    /// Captured provider retry policy.
    retry_policy: ResolvedRetryPolicy,
    /// Exact-model context metadata.
    context: Option<LlmModelContext>,
    /// Fields defaulted by the adapter.
    adapter_defaults: LlmCallConfigAdapterDefaults,
    dispatched: AtomicBool,
}

impl PreparedLlmCall {
    /// Resolved detached call configuration.
    #[must_use]
    pub const fn config(&self) -> &LlmCallConfig {
        &self.config
    }

    /// Retry policy captured with the adapter registration.
    #[must_use]
    pub const fn retry_policy(&self) -> &ResolvedRetryPolicy {
        &self.retry_policy
    }

    /// Exact-model context metadata.
    #[must_use]
    pub const fn context(&self) -> Option<&LlmModelContext> {
        self.context.as_ref()
    }

    /// Fields materialized by exact adapter resolution.
    #[must_use]
    pub const fn adapter_defaults(&self) -> &LlmCallConfigAdapterDefaults {
        &self.adapter_defaults
    }

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
    use std::sync::atomic::AtomicUsize;

    use futures::stream;

    use super::*;

    #[derive(Debug)]
    struct EchoAdapter;

    #[async_trait]
    impl LlmAdapter for EchoAdapter {
        fn stream(&self, _options: GenerateOptions) -> AdapterStream {
            AdapterStream::new(stream::iter(vec![Ok(StreamChunk::Finish {
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
                provider: ProviderId::new(provider),
                id: ModelId::new(model),
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
            AdapterStream::new(stream::iter(vec![Ok(StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            })]))
        }
    }

    #[derive(Debug)]
    struct OversizedDefaultAdapter;

    #[async_trait]
    impl LlmAdapter for OversizedDefaultAdapter {
        async fn resolve_model(
            &self,
            provider: &str,
            model: &str,
            _signal: Option<&AbortSignal>,
        ) -> anyhow::Result<LlmResolvedModelInfo> {
            Ok(LlmResolvedModelInfo {
                provider: ProviderId::new(provider),
                id: ModelId::new(model),
                name: model.to_owned(),
                description: None,
                input_modalities: None,
                context: None,
                default_max_tokens: Some(9_007_199_254_740_992),
                reasoning: None,
            })
        }

        fn stream(&self, _options: GenerateOptions) -> AdapterStream {
            AdapterStream::new(stream::empty())
        }
    }

    #[derive(Debug)]
    struct CleanupAdapter {
        cleanup_calls: Arc<AtomicUsize>,
        fail_cleanup: bool,
        fail_iteration: bool,
    }

    #[derive(Debug)]
    struct MarkerDefaultAdapter {
        seen: Arc<Mutex<Vec<bool>>>,
        default_max_tokens: Option<u64>,
    }

    #[async_trait]
    impl LlmAdapter for MarkerDefaultAdapter {
        async fn resolve_model(
            &self,
            provider: &str,
            model: &str,
            _signal: Option<&AbortSignal>,
        ) -> anyhow::Result<LlmResolvedModelInfo> {
            Ok(LlmResolvedModelInfo {
                provider: ProviderId::new(provider),
                id: ModelId::new(model),
                name: model.to_owned(),
                description: None,
                input_modalities: None,
                context: None,
                default_max_tokens: self.default_max_tokens,
                reasoning: None,
            })
        }

        fn stream(&self, options: GenerateOptions) -> AdapterStream {
            self.seen.lock().push(options.is_agent_loop_request());
            AdapterStream::new(stream::iter([Ok(StreamChunk::Finish {
                reason: FinishReason::Stop,
                replay_state: None,
            })]))
        }
    }

    #[async_trait]
    impl LlmAdapter for CleanupAdapter {
        fn stream(&self, _options: GenerateOptions) -> AdapterStream {
            let chunks = if self.fail_iteration {
                vec![Err(anyhow::anyhow!("iteration failed"))]
            } else {
                vec![Ok(StreamChunk::BlockStart {
                    index: 0,
                    block_type: "text".to_owned(),
                })]
            };
            let calls = self.cleanup_calls.clone();
            let fail_cleanup = self.fail_cleanup;
            AdapterStream::with_cleanup(stream::iter(chunks), move || async move {
                calls.fetch_add(1, Ordering::AcqRel);
                if fail_cleanup {
                    anyhow::bail!("cleanup failed");
                }
                Ok(())
            })
        }
    }

    fn options(provider: &str) -> GenerateOptions {
        GenerateOptions::new(ProviderId::new(provider), ModelId::new("m"), Vec::new())
    }

    #[tokio::test]
    async fn registration_routes_and_disposes() {
        let context = Context::new();
        let runtime = LlmRuntime::install(&context).expect("runtime");
        let handle = runtime
            .register_adapter(&["mock".to_owned()], Arc::new(EchoAdapter))
            .expect("register");
        assert_eq!(runtime.list_providers()[0].id.as_str(), "mock");
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
                    request.provider = ProviderId::new("routed");
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
        assert_eq!(prepared.config().max_tokens, Some(8_192));
        assert_eq!(
            prepared
                .config
                .reasoning_effort
                .as_ref()
                .map(crate::ReasoningEffortId::as_str),
            Some("high")
        );
        assert_eq!(prepared.adapter_defaults().max_tokens, Some(true));
        assert_eq!(prepared.adapter_defaults().reasoning_effort, Some(true));
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
            provider: ProviderId::new("route"),
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
                            id: ModelId::new("m"),
                            name: None,
                            context_window: None,
                            max_tokens: None,
                        },
                        LlmDiscoveredModel {
                            id: ModelId::new("m"),
                            name: Some("duplicate".to_owned()),
                            context_window: None,
                            max_tokens: None,
                        },
                        LlmDiscoveredModel {
                            id: ModelId::new(""),
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

    #[tokio::test]
    async fn exact_model_default_max_tokens_must_be_a_js_safe_integer() {
        let context = Context::new();
        let runtime = LlmRuntime::install(&context).expect("runtime");
        runtime
            .register_adapter(&["oversized".to_owned()], Arc::new(OversizedDefaultAdapter))
            .expect("register");
        let error = runtime
            .resolve_model_info("oversized", "m", None)
            .await
            .expect_err("unsafe integer must fail");
        assert_eq!(
            error.downcast_ref::<LlmError>().map(LlmError::code),
            Some("INVALID_MODEL_MAX_TOKENS")
        );
    }

    #[tokio::test]
    async fn topology_invariant_rethrows_an_incoherent_registry_notification() {
        let context = Context::new();
        let invariants = seekdeep_invariants::InvariantRegistry::install(
            &context,
            &seekdeep_invariants::InvariantConfig::default(),
        )
        .expect("invariants");
        let registration = crate::register_invariant(&invariants).expect("reserve companion");
        let runtime = LlmRuntime::install(&context).expect("runtime");
        registration
            .await_ready()
            .await
            .expect("activate companion");

        let owner = Uuid::now_v7();
        runtime.state.lock().adapters.insert(
            "unreadable-key".to_owned(),
            AdapterRegistration {
                owner,
                adapter: Arc::new(EchoAdapter),
                provider: LlmProviderInfo {
                    id: ProviderId::new("reported-route"),
                    name: "Reported route".to_owned(),
                },
                retry_policy: resolve_retry_policy(None, "test policy").expect("policy"),
            },
        );

        let error = runtime
            .emit_adapters_updated()
            .expect_err("incoherent registry must fail");
        let invariant = error
            .downcast_ref::<seekdeep_invariants::InvariantError>()
            .expect("package invariant error");
        assert_eq!(invariant.code, "INVARIANT");
        assert!(invariant.message.contains("no readable registration"));
    }

    #[tokio::test]
    async fn downstream_close_awaits_adapter_cleanup_and_propagates_its_failure() {
        let context = Context::new();
        let runtime = LlmRuntime::install(&context).expect("runtime");
        let cleanup_calls = Arc::new(AtomicUsize::new(0));
        runtime
            .register_adapter(
                &["cleanup".to_owned()],
                Arc::new(CleanupAdapter {
                    cleanup_calls: cleanup_calls.clone(),
                    fail_cleanup: true,
                    fail_iteration: false,
                }),
            )
            .expect("register");

        let mut output = runtime.stream(options("cleanup"));
        assert!(output.next().await.is_some());
        let error = output.close().await.expect_err("cleanup failure");
        assert_eq!(error.to_string(), "cleanup failed");
        assert_eq!(cleanup_calls.load(Ordering::Acquire), 1);
        assert!(output.next().await.is_none());
        output.close().await.expect("close remains idempotent");
        assert_eq!(cleanup_calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn adapter_failure_marks_iteration_complete_without_running_return_cleanup() {
        let context = Context::new();
        let runtime = LlmRuntime::install(&context).expect("runtime");
        let cleanup_calls = Arc::new(AtomicUsize::new(0));
        runtime
            .register_adapter(
                &["failure".to_owned()],
                Arc::new(CleanupAdapter {
                    cleanup_calls: cleanup_calls.clone(),
                    fail_cleanup: false,
                    fail_iteration: true,
                }),
            )
            .expect("register");

        let mut output = runtime.stream(options("failure"));
        let chunk = output.next().await.expect("terminal chunk").expect("chunk");
        assert!(matches!(
            chunk,
            StreamChunk::Finish {
                reason: FinishReason::Error { .. },
                ..
            }
        ));
        assert!(output.next().await.is_none());
        output
            .close()
            .await
            .expect("no return cleanup after failure");
        assert_eq!(cleanup_calls.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn adapter_default_reconstruction_clears_exact_object_agent_loop_identity() {
        let context = Context::new();
        let runtime = LlmRuntime::install(&context).expect("runtime");
        let unchanged_seen = Arc::new(Mutex::new(Vec::new()));
        let defaulted_seen = Arc::new(Mutex::new(Vec::new()));
        runtime
            .register_adapter(
                &["unchanged".to_owned()],
                Arc::new(MarkerDefaultAdapter {
                    seen: unchanged_seen.clone(),
                    default_max_tokens: None,
                }),
            )
            .expect("unchanged");
        runtime
            .register_adapter(
                &["defaulted".to_owned()],
                Arc::new(MarkerDefaultAdapter {
                    seen: defaulted_seen.clone(),
                    default_max_tokens: Some(256),
                }),
            )
            .expect("defaulted");

        runtime
            .stream(options("unchanged").mark_agent_loop_request())
            .collect::<Vec<_>>()
            .await;
        runtime
            .stream(options("defaulted").mark_agent_loop_request())
            .collect::<Vec<_>>()
            .await;
        assert_eq!(*unchanged_seen.lock(), [true]);
        assert_eq!(*defaulted_seen.lock(), [false]);
    }
}
