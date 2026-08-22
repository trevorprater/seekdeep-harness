//! Page-local Client inspect providers, manifest publication, queries, and cancellation.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use futures::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use indexmap::IndexMap;
use parking_lot::Mutex;
use seekdeep_cordis::{Context, ServiceKey, fiber::EffectHandle};
use seekdeep_cordis_dynamic_types::{
    CordisInspectFailureReason, CordisInspectProviderManifest, CordisInspectQueryRequest,
    CordisInspectQueryResolution, CordisInspectRequestId,
};
use seekdeep_identity::SessionId;
use serde_json::Value;

use crate::ClientTaskSpawner;

/// Cloneable page-local inspect cancellation signal.
#[derive(Clone, Debug, Default)]
pub struct ClientAbortSignal(Arc<AtomicBool>);

impl ClientAbortSignal {
    /// Requests cancellation; repeated calls are idempotent.
    pub fn abort(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether cancellation has been requested.
    #[must_use]
    pub fn is_aborted(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Client Cordis service slot for capability discovery.
pub const CLIENT_CORDIS_INSPECT: ServiceKey<ClientCordisInspectRegistry> =
    ServiceKey::new("cordisInspect");

/// Context supplied to one Client inspect query.
#[derive(Clone, Debug)]
pub struct ClientCordisInspectQueryContext {
    /// Cancellation broadcast by Host settlement.
    pub signal: ClientAbortSignal,
    /// Session whose model requested the query.
    pub session_id: SessionId,
}

/// One read-only Client provider method dispatcher.
pub type ClientCordisInspectQuery = Arc<
    dyn Fn(
            String,
            Option<Value>,
            ClientCordisInspectQueryContext,
        ) -> BoxFuture<'static, anyhow::Result<Value>>
        + Send
        + Sync,
>;

/// Client provider registration retained beside its manifest.
#[derive(Clone)]
pub struct ClientCordisInspectProviderRegistration {
    /// Serializable provider directory.
    pub manifest: CordisInspectProviderManifest,
    /// Declared query dispatcher.
    pub query: ClientCordisInspectQuery,
}

impl std::fmt::Debug for ClientCordisInspectProviderRegistration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientCordisInspectProviderRegistration")
            .field("manifest", &self.manifest)
            .finish_non_exhaustive()
    }
}

/// Folded Host operations consumed by the Client inspect registry.
pub trait ClientCordisInspectHost: Send + Sync + 'static {
    /// Replaces the Host's mirrored Client directory.
    fn sync(
        &self,
        providers: Vec<CordisInspectProviderManifest>,
    ) -> BoxFuture<'static, anyhow::Result<()>>;
    /// Submits one local result; the first accepted page wins Host-side.
    fn resolve(
        &self,
        session_id: SessionId,
        request_id: CordisInspectRequestId,
        resolution: CordisInspectQueryResolution,
    ) -> BoxFuture<'static, anyhow::Result<()>>;
}

/// Browser `queueMicrotask` abstraction.
pub trait ClientMicrotaskScheduler: Send + Sync + 'static {
    /// Queues one non-reentrant callback.
    fn queue(&self, callback: Box<dyn FnOnce() + Send>);
}

type SyncTail = Shared<BoxFuture<'static, ()>>;

struct InspectState {
    providers: IndexMap<String, ClientCordisInspectProviderRegistration>,
    active: IndexMap<CordisInspectRequestId, ClientAbortSignal>,
    publish_queued: bool,
    sync_tail: SyncTail,
}

impl Default for InspectState {
    fn default() -> Self {
        Self {
            providers: IndexMap::new(),
            active: IndexMap::new(),
            publish_queued: false,
            sync_tail: futures::future::ready(()).boxed().shared(),
        }
    }
}

/// Client provider registry, manifest publisher, and query dispatcher.
pub struct ClientCordisInspectRegistry {
    host: Arc<dyn ClientCordisInspectHost>,
    microtasks: Arc<dyn ClientMicrotaskScheduler>,
    spawner: Arc<dyn ClientTaskSpawner>,
    state: Mutex<InspectState>,
}

impl std::fmt::Debug for ClientCordisInspectRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock();
        formatter
            .debug_struct("ClientCordisInspectRegistry")
            .field("providers", &state.providers.len())
            .field("active", &state.active.len())
            .field("publish_queued", &state.publish_queued)
            .finish_non_exhaustive()
    }
}

impl ClientCordisInspectRegistry {
    /// Creates one page-local registry.
    #[must_use]
    pub fn new(
        host: Arc<dyn ClientCordisInspectHost>,
        microtasks: Arc<dyn ClientMicrotaskScheduler>,
        spawner: Arc<dyn ClientTaskSpawner>,
    ) -> Arc<Self> {
        Arc::new(Self {
            host,
            microtasks,
            spawner,
            state: Mutex::new(InspectState::default()),
        })
    }

    /// Provides this page-local registry through Client Cordis Context.
    ///
    /// # Errors
    ///
    /// Returns ordinary duplicate-Service or inactive-owner failures.
    pub fn provide(
        self: &Arc<Self>,
        context: &Context,
    ) -> Result<EffectHandle, seekdeep_cordis::CordisError> {
        context.provide(CLIENT_CORDIS_INSPECT, self.clone())
    }

    /// Registers one provider and queues complete-manifest publication.
    ///
    /// # Errors
    ///
    /// Rejects an empty ID, duplicate provider, or duplicate method name.
    pub fn register(
        self: &Arc<Self>,
        registration: ClientCordisInspectProviderRegistration,
    ) -> anyhow::Result<ClientInspectRegistration> {
        let id = registration.manifest.id.clone();
        anyhow::ensure!(
            !id.trim().is_empty(),
            "Client Cordis inspect provider id must not be empty"
        );
        let mut names = std::collections::HashSet::new();
        for method in &registration.manifest.methods {
            anyhow::ensure!(
                names.insert(method.name.clone()),
                "Client Cordis inspect provider \"{id}\" repeats method \"{}\"",
                method.name
            );
        }
        {
            let mut state = self.state.lock();
            anyhow::ensure!(
                !state.providers.contains_key(&id),
                "Client Cordis inspect provider \"{id}\" is already registered"
            );
            state.providers.insert(id.clone(), registration);
        }
        self.publish();
        Ok(ClientInspectRegistration {
            registry: Arc::downgrade(self),
            id,
            disposed: Mutex::new(false),
        })
    }

    /// Queues a complete manifest publication, coalescing same-turn mutations.
    pub fn publish(self: &Arc<Self>) {
        {
            let mut state = self.state.lock();
            if state.publish_queued {
                return;
            }
            state.publish_queued = true;
        }
        let registry = self.clone();
        self.microtasks.queue(Box::new(move || {
            let (manifests, previous) = {
                let mut state = registry.state.lock();
                state.publish_queued = false;
                (
                    state
                        .providers
                        .values()
                        .map(|provider| provider.manifest.clone())
                        .collect::<Vec<_>>(),
                    state.sync_tail.clone(),
                )
            };
            let host = registry.host.clone();
            let sync = async move {
                previous.await;
                let _ = host.sync(manifests).await;
            }
            .boxed()
            .shared();
            registry.state.lock().sync_tail = sync.clone();
            registry.spawner.spawn(Box::pin(sync));
        }));
    }

    /// Executes and answers one Host-broadcast query once per request ID.
    pub async fn query(self: &Arc<Self>, request: CordisInspectQueryRequest) {
        let signal = {
            let mut state = self.state.lock();
            if state.active.contains_key(&request.request_id) {
                return;
            }
            let signal = ClientAbortSignal::default();
            state
                .active
                .insert(request.request_id.clone(), signal.clone());
            signal
        };
        let resolution = self.resolve_local(&request, &signal).await;
        self.state.lock().active.shift_remove(&request.request_id);
        if signal.is_aborted() {
            return;
        }
        let _ = self
            .host
            .resolve(request.agent_id, request.request_id, resolution)
            .await;
    }

    /// Cancels local work after another page or the Host settled the query.
    pub fn close(&self, request_id: &CordisInspectRequestId) {
        if let Some(signal) = self.state.lock().active.shift_remove(request_id) {
            signal.abort();
        }
    }

    /// Current complete manifest in registration order.
    #[must_use]
    pub fn manifests(&self) -> Vec<CordisInspectProviderManifest> {
        self.state
            .lock()
            .providers
            .values()
            .map(|provider| provider.manifest.clone())
            .collect()
    }

    async fn resolve_local(
        &self,
        request: &CordisInspectQueryRequest,
        signal: &ClientAbortSignal,
    ) -> CordisInspectQueryResolution {
        let provider = self.state.lock().providers.get(&request.provider).cloned();
        let Some(provider) = provider else {
            return CordisInspectQueryResolution::Failure {
                reason: CordisInspectFailureReason::ProviderMissing,
                message: format!(
                    "Client inspect provider \"{}\" is unavailable",
                    request.provider
                ),
            };
        };
        if !provider
            .manifest
            .methods
            .iter()
            .any(|method| method.name == request.method)
        {
            return CordisInspectQueryResolution::Failure {
                reason: CordisInspectFailureReason::MethodMissing,
                message: format!(
                    "Client inspect provider \"{}\" has no method \"{}\"",
                    request.provider, request.method
                ),
            };
        }
        let result = (provider.query)(
            request.method.clone(),
            request.input.clone(),
            ClientCordisInspectQueryContext {
                signal: signal.clone(),
                session_id: request.agent_id.clone(),
            },
        )
        .await;
        if signal.is_aborted() {
            return CordisInspectQueryResolution::Failure {
                reason: CordisInspectFailureReason::Cancelled,
                message: "Client inspect query was cancelled".to_owned(),
            };
        }
        match result {
            Ok(data) => CordisInspectQueryResolution::Success { data },
            Err(error) => CordisInspectQueryResolution::Failure {
                reason: CordisInspectFailureReason::ProviderError,
                message: error.to_string(),
            },
        }
    }
}

/// Idempotent provider disposer.
pub struct ClientInspectRegistration {
    registry: std::sync::Weak<ClientCordisInspectRegistry>,
    id: String,
    disposed: Mutex<bool>,
}

impl ClientInspectRegistration {
    /// Removes this exact registration and republishes the complete manifest.
    pub fn dispose(&self) {
        let mut disposed = self.disposed.lock();
        if *disposed {
            return;
        }
        *disposed = true;
        let Some(registry) = self.registry.upgrade() else {
            return;
        };
        if registry
            .state
            .lock()
            .providers
            .shift_remove(&self.id)
            .is_some()
        {
            registry.publish();
        }
    }
}
