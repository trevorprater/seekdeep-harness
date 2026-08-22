//! Exact-run page-local package convergence, snapshots, failures, and teardown.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use futures::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use parking_lot::Mutex;
use seekdeep_cordis_dynamic_types::{
    CordisDynamicPackageId, CordisDynamicPluginId, CordisDynamicPluginRunId,
    DynamicCordisRenderFailure,
};
use seekdeep_identity::SessionId;

use crate::{
    ClientLoadErrorCause, ClientLoadRequest, ClientLoadResult, ClientTaskSpawner,
    DYNAMIC_CLIENT_REDIRECTS,
};

/// One page-local live Client package summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicCordisLivePackage {
    /// Stable Plugin.
    pub plugin_id: CordisDynamicPluginId,
    /// Immutable Package.
    pub package_id: CordisDynamicPackageId,
    /// Exact activation loaded in this page.
    pub plugin_run_id: CordisDynamicPluginRunId,
    /// Package label.
    pub name: String,
    /// Unique Slot names contributed here.
    pub slots: Vec<String>,
    /// Live package-owned style count.
    pub style_count: usize,
}

/// Browser engine's successful mount projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountedClientPackage {
    /// Services the settled Fiber still waits for.
    pub waiting_for: Vec<String>,
    /// Slot ledger rows, including duplicates before projection.
    pub slots: Vec<String>,
    /// Live style tag count.
    pub style_count: usize,
}

/// Browser-specific evaluator/module/Loader/Guard mounting seam.
pub trait ClientMountEngine: Send + Sync + 'static {
    /// Installs the page-local Slot crash callback after the runtime exists.
    fn watch(&self, _listener: ClientRenderCrashListener) {}
    /// Evaluates and mounts one exact activation.
    fn mount(
        &self,
        request: ClientLoadRequest,
    ) -> BoxFuture<'static, Result<MountedClientPackage, ClientMountError>>;
    /// Removes one exact live activation and all browser contributions.
    fn teardown(
        &self,
        plugin_id: CordisDynamicPluginId,
        plugin_run_id: CordisDynamicPluginRunId,
    ) -> BoxFuture<'static, anyhow::Result<()>>;
    /// Stops watching browser crash seams.
    fn unwatch(&self);
}

/// Browser-engine callback for one component-owned Slot crash.
pub type ClientRenderCrashListener = Arc<
    dyn Fn(SessionId, CordisDynamicPluginId, CordisDynamicPluginRunId, DynamicCordisRenderFailure)
        + Send
        + Sync,
>;

/// Mount failure classified at the browser boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientMountFailure {
    /// Evaluation, module handoff, or activation stage.
    pub cause: ClientLoadErrorCause,
    /// Original message.
    pub message: String,
    /// Original stack when supplied.
    pub stack: Option<String>,
}

/// Browser failure outside the three deliberately classified load stages.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ClientMountRejection {
    /// Original message.
    pub message: String,
    /// Original stack when supplied.
    pub stack: Option<String>,
}

impl ClientMountRejection {
    fn from_error(error: &anyhow::Error) -> Self {
        Self {
            message: format!("{error:#}"),
            stack: None,
        }
    }
}

/// Either an answered load failure or a rejected runner operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientMountError {
    /// Evaluation, module import, or activation answered normally.
    Classified(ClientMountFailure),
    /// Infrastructure failure rejects the load Promise itself.
    Rejected(ClientMountRejection),
}

/// Fire-and-forget Host report for one post-settle render crash.
pub type RenderFailureReporter = Arc<
    dyn Fn(SessionId, CordisDynamicPluginId, CordisDynamicPluginRunId, DynamicCordisRenderFailure)
        + Send
        + Sync,
>;

#[derive(Clone, Debug)]
struct LiveRecord {
    request: ClientLoadRequest,
    mounted: MountedClientPackage,
}

type SharedLoad = Shared<BoxFuture<'static, Result<ClientLoadResult, ClientMountRejection>>>;
type SharedTail = Shared<BoxFuture<'static, ()>>;
type ChangeListener = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct RuntimeState {
    live: BTreeMap<CordisDynamicPluginId, LiveRecord>,
    queues: BTreeMap<CordisDynamicPluginId, SharedTail>,
    failures: BTreeMap<CordisDynamicPluginId, DynamicCordisRenderFailure>,
    listeners: BTreeMap<u64, ChangeListener>,
    next_listener: u64,
    snapshot_cache: Option<Arc<Vec<DynamicCordisLivePackage>>>,
    failure_cache: Option<Arc<BTreeMap<CordisDynamicPluginId, DynamicCordisRenderFailure>>>,
}

/// Page-local dynamic Client package runtime.
pub struct DynamicCordisClientRuntime {
    engine: Arc<dyn ClientMountEngine>,
    spawner: Arc<dyn ClientTaskSpawner>,
    report_render_failure: RenderFailureReporter,
    state: Mutex<RuntimeState>,
}

/// Object-safe orchestrator adapter retaining the runtime's shared owner.
#[derive(Clone, Debug)]
pub struct DynamicCordisRuntimeRunner {
    runtime: Arc<DynamicCordisClientRuntime>,
}

impl DynamicCordisRuntimeRunner {
    /// Wraps one page-local runtime for Host-to-Client orchestration.
    #[must_use]
    pub fn new(runtime: Arc<DynamicCordisClientRuntime>) -> Self {
        Self { runtime }
    }

    /// Returns the underlying page runtime.
    #[must_use]
    pub fn runtime(&self) -> &Arc<DynamicCordisClientRuntime> {
        &self.runtime
    }
}

impl crate::DynamicCordisPackageRunner for DynamicCordisRuntimeRunner {
    fn load(
        &self,
        request: ClientLoadRequest,
    ) -> BoxFuture<'static, Result<ClientLoadResult, ClientMountRejection>> {
        self.runtime.load(request).boxed()
    }
}

impl std::fmt::Debug for DynamicCordisClientRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock();
        formatter
            .debug_struct("DynamicCordisClientRuntime")
            .field("live", &state.live.len())
            .field("queues", &state.queues.len())
            .field("failures", &state.failures.len())
            .finish_non_exhaustive()
    }
}

impl DynamicCordisClientRuntime {
    /// Creates one page runtime over injected browser and Host seams.
    #[must_use]
    pub fn new(
        engine: Arc<dyn ClientMountEngine>,
        spawner: Arc<dyn ClientTaskSpawner>,
        report_render_failure: RenderFailureReporter,
    ) -> Arc<Self> {
        let runtime = Arc::new(Self {
            engine,
            spawner,
            report_render_failure,
            state: Mutex::new(RuntimeState::default()),
        });
        let weak = Arc::downgrade(&runtime);
        runtime.engine.watch(Arc::new(
            move |agent_id, plugin_id, plugin_run_id, failure| {
                if let Some(runtime) = weak.upgrade() {
                    runtime.report_render_failure(&agent_id, &plugin_id, &plugin_run_id, &failure);
                }
            },
        ));
        runtime
    }

    /// Stable live-set snapshot reference until the next convergence.
    #[must_use]
    pub fn snapshot(&self) -> Arc<Vec<DynamicCordisLivePackage>> {
        let mut state = self.state.lock();
        if let Some(snapshot) = &state.snapshot_cache {
            return snapshot.clone();
        }
        let snapshot = Arc::new(state.live.values().map(live_summary).collect::<Vec<_>>());
        state.snapshot_cache = Some(snapshot.clone());
        snapshot
    }

    /// Stable page-local render-failure snapshot until the next mutation.
    #[must_use]
    pub fn render_failures(
        &self,
    ) -> Arc<BTreeMap<CordisDynamicPluginId, DynamicCordisRenderFailure>> {
        let mut state = self.state.lock();
        if let Some(snapshot) = &state.failure_cache {
            return snapshot.clone();
        }
        let snapshot = Arc::new(state.failures.clone());
        state.failure_cache = Some(snapshot.clone());
        snapshot
    }

    /// Whether this page currently owns any activation of `plugin_id`.
    #[must_use]
    pub fn is_loaded(&self, plugin_id: &CordisDynamicPluginId) -> bool {
        self.state.lock().live.contains_key(plugin_id)
    }

    /// Subscribes to live-set and render-failure changes.
    #[must_use]
    pub fn subscribe(self: &Arc<Self>, listener: ChangeListener) -> ClientRuntimeSubscription {
        let id = {
            let mut state = self.state.lock();
            state.next_listener += 1;
            let id = state.next_listener;
            state.listeners.insert(id, listener);
            id
        };
        ClientRuntimeSubscription {
            runtime: Arc::downgrade(self),
            id,
        }
    }

    /// Loads or converges one exact activation behind prior work for the same Plugin.
    pub fn load(self: &Arc<Self>, request: ClientLoadRequest) -> SharedLoad {
        let previous = self
            .state
            .lock()
            .queues
            .get(&request.plugin_id)
            .cloned()
            .unwrap_or_else(|| futures::future::ready(()).boxed().shared());
        let runtime = self.clone();
        let plugin_id = request.plugin_id.clone();
        let load = async move {
            previous.await;
            runtime.load_now(request).await
        }
        .boxed()
        .shared();
        let tail = load.clone().map(|_| ()).boxed().shared();
        self.state.lock().queues.insert(plugin_id, tail);
        load
    }

    /// Queues exact-generation retraction; newer activations survive.
    pub fn retract(
        self: &Arc<Self>,
        plugin_id: CordisDynamicPluginId,
        plugin_run_id: CordisDynamicPluginRunId,
    ) {
        let previous = self
            .state
            .lock()
            .queues
            .get(&plugin_id)
            .cloned()
            .unwrap_or_else(|| futures::future::ready(()).boxed().shared());
        let runtime = self.clone();
        let queue_id = plugin_id.clone();
        let future = async move {
            previous.await;
            let current = runtime.state.lock().live.get(&plugin_id).cloned();
            let Some(current) =
                current.filter(|current| current.request.plugin_run_id == plugin_run_id)
            else {
                return;
            };
            let _ = runtime
                .engine
                .teardown(plugin_id.clone(), current.request.plugin_run_id)
                .await;
            {
                let mut state = runtime.state.lock();
                state.live.remove(&plugin_id);
                state.failures.remove(&plugin_id);
            }
            runtime.notify();
        }
        .boxed()
        .shared();
        self.state.lock().queues.insert(queue_id, future.clone());
        self.spawner.spawn(Box::pin(future));
    }

    /// Records one crash only when this page still owns the exact run.
    pub fn report_render_failure(
        &self,
        agent_id: &SessionId,
        plugin_id: &CordisDynamicPluginId,
        plugin_run_id: &CordisDynamicPluginRunId,
        crash: &DynamicCordisRenderFailure,
    ) {
        let owner_matches = self.state.lock().live.get(plugin_id).is_some_and(|record| {
            record.request.agent_id == *agent_id && record.request.plugin_run_id == *plugin_run_id
        });
        if !owner_matches {
            return;
        }
        let failure = DynamicCordisRenderFailure {
            slot: crash.slot.clone(),
            message: render_failure_message(&crash.slot, &crash.message),
            stack: crash.stack.clone(),
            abdicated: crash.abdicated,
        };
        (self.report_render_failure)(
            agent_id.clone(),
            plugin_id.clone(),
            plugin_run_id.clone(),
            failure.clone(),
        );
        self.state
            .lock()
            .failures
            .insert(plugin_id.clone(), failure);
        self.notify();
    }

    /// Unloads every live package and stops watching browser crash seams.
    pub async fn dispose(&self) {
        self.engine.unwatch();
        let live = self.state.lock().live.values().cloned().collect::<Vec<_>>();
        for record in live {
            let _ = self
                .engine
                .teardown(
                    record.request.plugin_id.clone(),
                    record.request.plugin_run_id.clone(),
                )
                .await;
        }
        {
            let mut state = self.state.lock();
            state.live.clear();
            state.failures.clear();
        }
        self.notify();
    }

    async fn load_now(
        &self,
        request: ClientLoadRequest,
    ) -> Result<ClientLoadResult, ClientMountRejection> {
        let current = self.state.lock().live.get(&request.plugin_id).cloned();
        if let Some(current) = current {
            if current.request.plugin_run_id == request.plugin_run_id {
                return Ok(settled(&current));
            }
            self.engine
                .teardown(request.plugin_id.clone(), current.request.plugin_run_id)
                .await
                .map_err(|error| ClientMountRejection::from_error(&error))?;
            self.state.lock().live.remove(&request.plugin_id);
        }
        let mounted = match self.engine.mount(request.clone()).await {
            Ok(mounted) => mounted,
            Err(ClientMountError::Classified(failure)) => {
                self.notify();
                return Ok(ClientLoadResult::Failure {
                    cause: failure.cause,
                    error: seekdeep_cordis_dynamic_types::CordisErrorDetails {
                        message: failure.message,
                        stack: failure.stack,
                    },
                });
            }
            Err(ClientMountError::Rejected(error)) => return Err(error),
        };
        let record = LiveRecord {
            request: request.clone(),
            mounted,
        };
        {
            let mut state = self.state.lock();
            state.live.insert(request.plugin_id.clone(), record.clone());
            state.failures.remove(&request.plugin_id);
        }
        self.notify();
        Ok(settled(&record))
    }

    fn notify(&self) {
        let listeners = {
            let mut state = self.state.lock();
            state.snapshot_cache = None;
            state.failure_cache = None;
            state.listeners.values().cloned().collect::<Vec<_>>()
        };
        for listener in listeners {
            listener();
        }
    }
}

/// Idempotent page-runtime subscription disposer.
pub struct ClientRuntimeSubscription {
    runtime: std::sync::Weak<DynamicCordisClientRuntime>,
    id: u64,
}

impl ClientRuntimeSubscription {
    /// Stops notifications.
    pub fn dispose(&self) {
        if let Some(runtime) = self.runtime.upgrade() {
            runtime.state.lock().listeners.remove(&self.id);
        }
    }
}

fn live_summary(record: &LiveRecord) -> DynamicCordisLivePackage {
    let slots = record
        .mounted
        .slots
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    DynamicCordisLivePackage {
        plugin_id: record.request.plugin_id.clone(),
        package_id: record.request.package_id.clone(),
        plugin_run_id: record.request.plugin_run_id.clone(),
        name: record.request.name.clone(),
        slots,
        style_count: record.mounted.style_count,
    }
}

fn settled(record: &LiveRecord) -> ClientLoadResult {
    ClientLoadResult::Success {
        plugin_run_id: record.request.plugin_run_id.clone(),
        waiting_for: (!record.mounted.waiting_for.is_empty())
            .then(|| record.mounted.waiting_for.clone()),
    }
}

/// Adds the first missing browser-global redirect to a render crash.
#[must_use]
pub fn render_failure_message(slot: &str, message: &str) -> String {
    let redirect = DYNAMIC_CLIENT_REDIRECTS
        .iter()
        .find(|(name, redirect)| message.contains(name) && !message.contains(redirect))
        .map(|(_, redirect)| *redirect);
    let base = format!("your entry in slot {slot:?} crashed while React rendered it: {message}");
    redirect.map_or(base.clone(), |redirect| format!("{base}\n{redirect}"))
}
