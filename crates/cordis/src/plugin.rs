//! Plugin registration and dependency-epoch lifecycle management.

use std::{
    any::Any,
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use futures::FutureExt as _;
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::{
    Context, CordisError, EventArgs, Fiber, FiberState,
    fiber::{DisposeFuture, EffectHandle},
};

/// Boxed plugin startup computation.
pub type PluginFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'static>>;

type PluginCallback = Arc<dyn Fn(Context, Value) -> PluginFuture + Send + Sync>;
type ConfigValidator = Arc<dyn Fn(&Value) -> anyhow::Result<Value> + Send + Sync>;
type ConfigResolver = Arc<dyn Fn(&Context, &Value) -> anyhow::Result<Value> + Send + Sync>;

/// Shared plugin entrypoint metadata and executable body.
#[derive(Clone)]
pub struct Plugin {
    id: Uuid,
    name: String,
    inject: Vec<String>,
    inject_intercepts: Vec<(String, Value)>,
    callback: PluginCallback,
    config_resolver: Option<ConfigResolver>,
    validator: Option<ConfigValidator>,
}

impl std::fmt::Debug for Plugin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Plugin")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("inject", &self.inject)
            .finish_non_exhaustive()
    }
}

impl Plugin {
    /// Defines a plugin with required service names and an asynchronous startup body.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        inject: impl IntoIterator<Item = impl Into<String>>,
        callback: impl Fn(Context, Value) -> PluginFuture + Send + Sync + 'static,
    ) -> Self {
        Self {
            id: Uuid::now_v7(),
            name: name.into(),
            inject: inject.into_iter().map(Into::into).collect(),
            inject_intercepts: Vec::new(),
            callback: Arc::new(callback),
            config_resolver: None,
            validator: None,
        }
    }

    /// Adds entry-local configuration resolution that runs after every
    /// declared dependency is active and before schema validation.
    ///
    /// Loader expression interpolation uses this seam so a provider
    /// replacement resolves the same raw configuration against the new
    /// dependency generation without teaching Cordis about a source language.
    #[must_use]
    pub fn with_config_resolver(
        mut self,
        resolver: impl Fn(&Context, &Value) -> anyhow::Result<Value> + Send + Sync + 'static,
    ) -> Self {
        self.config_resolver = Some(Arc::new(resolver));
        self
    }

    /// Merges loader-entry dependency declarations into plugin metadata.
    /// Existing declarations retain their order and duplicates are ignored.
    #[must_use]
    pub fn with_additional_inject(
        mut self,
        inject: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        for name in inject.into_iter().map(Into::into) {
            if !self.inject.contains(&name) {
                self.inject.push(name);
            }
        }
        self
    }

    /// Adds a required service together with the intercept config visible to
    /// the plugin context while that dependency generation is active.
    #[must_use]
    pub fn with_inject_config(mut self, name: impl Into<String>, config: Value) -> Self {
        let name = name.into();
        if !self.inject.contains(&name) {
            self.inject.push(name.clone());
        }
        self.inject_intercepts.push((name, config));
        self
    }

    /// Adds synchronous configuration validation and normalization.
    #[must_use]
    pub fn with_config_validator(
        mut self,
        validator: impl Fn(&Value) -> anyhow::Result<Value> + Send + Sync + 'static,
    ) -> Self {
        self.validator = Some(Arc::new(validator));
        self
    }

    /// Stable identity shared by every mount of this plugin value.
    #[must_use]
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Diagnostic display name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Required service names.
    #[must_use]
    pub fn inject(&self) -> &[String] {
        &self.inject
    }
}

#[derive(Default)]
struct RegistryInner {
    fibers: Mutex<Vec<Weak<PluginFiber>>>,
    next_uid: AtomicU64,
}

/// Source-compatible runtime record for one plugin identity.
#[derive(Clone, Debug)]
pub struct PluginRuntimeSnapshot {
    /// Stable plugin value identity.
    pub plugin_id: Uuid,
    /// Diagnostic plugin name.
    pub name: String,
    /// Live mounts in insertion order.
    pub fibers: Vec<Arc<PluginFiber>>,
    /// Whether the runtime validates configuration.
    pub has_config_validator: bool,
}

/// Root-owned registry of every mounted plugin fiber.
#[derive(Clone, Default)]
pub struct PluginRegistry {
    inner: Arc<RegistryInner>,
}

impl PluginRegistry {
    /// Creates an empty plugin registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mounts a plugin and schedules dependency resolution.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] when the parent context is inactive.
    pub fn mount(
        &self,
        parent: &Context,
        plugin: Plugin,
        config: Value,
    ) -> Result<Arc<PluginFiber>, CordisError> {
        let fiber = Fiber::child_of(plugin.name.clone(), parent.fiber());
        let uid = self.inner.next_uid.fetch_add(1, Ordering::AcqRel) + 1;
        let plugin_context = plugin
            .inject_intercepts
            .iter()
            .fold(parent.clone(), |context, (name, config)| {
                context.intercept(name, config.clone())
            })
            .with_fiber(fiber.clone());
        let mounted = Arc::new(PluginFiber {
            fiber: fiber.clone(),
            uid: AtomicU64::new(uid),
            context: plugin_context,
            plugin,
            additional_inject: Mutex::new(Vec::new()),
            config: Mutex::new(config),
            epoch: Mutex::new(None),
            error: Mutex::new(None),
            transition: tokio::sync::Mutex::new(()),
            updates: tokio::sync::Mutex::new(()),
            scheduled: AtomicBool::new(false),
            dirty: AtomicBool::new(false),
            disposed: AtomicBool::new(false),
            settled: Notify::new(),
            registry: Arc::downgrade(&self.inner),
        });
        let owned = mounted.clone();
        let structural = EffectHandle::new("ctx.plugin()", move || -> DisposeFuture {
            Box::pin(async move { owned.dispose().await })
        });
        parent.own(structural)?;
        self.inner.fibers.lock().push(Arc::downgrade(&mounted));
        if let Err(error) = parent.events().emit(
            parent,
            "internal/plugin",
            &EventArgs::one_shared(mounted.clone()),
        ) {
            let cleanup = mounted.clone();
            spawn_background(async move {
                if let Err(cleanup_error) = cleanup.dispose().await {
                    tracing::error!(%cleanup_error, "failed plugin publication rollback failed");
                }
            });
            return Err(CordisError::PluginPublication(format!("{error:#}")));
        }
        mounted.schedule();
        Ok(mounted)
    }

    /// Re-evaluates dependency epochs for every live plugin.
    pub fn notify_service_change(&self) {
        let fibers = {
            let mut fibers = self.inner.fibers.lock();
            let live = fibers.iter().filter_map(Weak::upgrade).collect::<Vec<_>>();
            fibers.retain(|fiber| fiber.strong_count() > 0);
            live
        };
        for fiber in fibers {
            fiber.schedule();
        }
    }

    /// Returns the runtime record for one plugin identity.
    #[must_use]
    pub fn get(&self, plugin: &Plugin) -> Option<PluginRuntimeSnapshot> {
        let fibers = self.fibers_for(plugin.id());
        (!fibers.is_empty()).then(|| PluginRuntimeSnapshot {
            plugin_id: plugin.id(),
            name: plugin.name().to_owned(),
            has_config_validator: plugin.validator.is_some(),
            fibers,
        })
    }

    /// Whether at least one undisposed mount exists for this plugin identity.
    #[must_use]
    pub fn has(&self, plugin: &Plugin) -> bool {
        !self.fibers_for(plugin.id()).is_empty()
    }

    /// Snapshots all runtime records in first-mount order.
    #[must_use]
    pub fn values(&self) -> Vec<PluginRuntimeSnapshot> {
        let live = self.live_fibers();
        let mut ids = Vec::new();
        for fiber in &live {
            let id = fiber.plugin.id();
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids.into_iter()
            .filter_map(|id| {
                let first = live.iter().find(|fiber| fiber.plugin.id() == id)?;
                Some(PluginRuntimeSnapshot {
                    plugin_id: id,
                    name: first.plugin.name().to_owned(),
                    has_config_validator: first.plugin.validator.is_some(),
                    fibers: live
                        .iter()
                        .filter(|fiber| fiber.plugin.id() == id)
                        .cloned()
                        .collect(),
                })
            })
            .collect()
    }

    /// Stable plugin identities in first-mount order.
    #[must_use]
    pub fn keys(&self) -> Vec<Uuid> {
        self.values()
            .into_iter()
            .map(|runtime| runtime.plugin_id)
            .collect()
    }

    /// Runtime entries in first-mount order.
    #[must_use]
    pub fn entries(&self) -> Vec<(Uuid, PluginRuntimeSnapshot)> {
        self.values()
            .into_iter()
            .map(|runtime| (runtime.plugin_id, runtime))
            .collect()
    }

    /// Visits every runtime in first-mount order.
    pub fn for_each(&self, mut callback: impl FnMut(&PluginRuntimeSnapshot, Uuid)) {
        for runtime in self.values() {
            let id = runtime.plugin_id;
            callback(&runtime, id);
        }
    }

    /// Removes one runtime and starts disposal of all mounts without waiting.
    pub fn delete(&self, plugin: &Plugin) -> Option<PluginRuntimeSnapshot> {
        let runtime = self.get(plugin)?;
        self.inner.fibers.lock().retain(|fiber| {
            fiber
                .upgrade()
                .is_some_and(|fiber| fiber.plugin.id() != plugin.id())
        });
        for fiber in &runtime.fibers {
            fiber.uid.store(0, Ordering::Release);
            fiber.fiber.request_disposal();
            let fiber = fiber.clone();
            spawn_background(async move {
                let _ = fiber.dispose().await;
            });
        }
        Some(runtime)
    }

    /// Removes one runtime and joins deterministic disposal of every mount.
    pub async fn delete_joined(&self, plugin: &Plugin) -> Option<PluginRuntimeSnapshot> {
        let runtime = self.get(plugin)?;
        self.inner.fibers.lock().retain(|fiber| {
            fiber
                .upgrade()
                .is_some_and(|fiber| fiber.plugin.id() != plugin.id())
        });
        for fiber in &runtime.fibers {
            let _ = fiber.dispose().await;
        }
        Some(runtime)
    }

    /// Number of registered plugin runtimes, not mount count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.keys().len()
    }

    /// Whether the registry contains no reachable plugin mounts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Number of live mounts across all runtimes.
    #[must_use]
    pub fn fiber_count(&self) -> usize {
        self.live_fibers().len()
    }

    fn fibers_for(&self, plugin_id: Uuid) -> Vec<Arc<PluginFiber>> {
        self.live_fibers()
            .into_iter()
            .filter(|fiber| fiber.plugin.id() == plugin_id)
            .collect()
    }

    fn live_fibers(&self) -> Vec<Arc<PluginFiber>> {
        let mut registry = self.inner.fibers.lock();
        let live = registry
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|fiber| !fiber.disposed.load(Ordering::Acquire))
            .collect::<Vec<_>>();
        registry.retain(|fiber| {
            fiber
                .upgrade()
                .is_some_and(|fiber| !fiber.disposed.load(Ordering::Acquire))
        });
        live
    }
}

/// One mounted plugin and its current dependency epoch.
pub struct PluginFiber {
    fiber: Arc<Fiber>,
    uid: AtomicU64,
    context: Context,
    plugin: Plugin,
    additional_inject: Mutex<Vec<String>>,
    config: Mutex<Value>,
    epoch: Mutex<Option<Vec<Uuid>>>,
    error: Mutex<Option<String>>,
    transition: tokio::sync::Mutex<()>,
    updates: tokio::sync::Mutex<()>,
    scheduled: AtomicBool,
    dirty: AtomicBool,
    disposed: AtomicBool,
    settled: Notify,
    registry: Weak<RegistryInner>,
}

impl std::fmt::Debug for PluginFiber {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PluginFiber")
            .field("plugin", &self.plugin)
            .field("state", &self.fiber.state())
            .field("epoch", &self.epoch.lock())
            .field("error", &self.error.lock())
            .finish_non_exhaustive()
    }
}

impl PluginFiber {
    /// Monotonic runtime uid, absent after disposal.
    #[must_use]
    pub fn uid(&self) -> Option<u64> {
        match self.uid.load(Ordering::Acquire) {
            0 => None,
            uid => Some(uid),
        }
    }

    /// Stable plugin identity shared by sibling mounts.
    #[must_use]
    pub fn plugin_id(&self) -> Uuid {
        self.plugin.id()
    }

    /// Loader-facing plugin name for activation diagnostics.
    #[must_use]
    pub fn plugin_name(&self) -> &str {
        self.plugin.name()
    }

    /// Configured Loader package name when this Fiber belongs to an entry.
    #[must_use]
    pub fn entry_name(&self) -> Option<String> {
        self.context
            .meta("loader.entry_name")
            .and_then(|value| value.as_str().map(str::to_owned))
    }

    /// Stable Loader entry identity when present.
    #[must_use]
    pub fn entry_id(&self) -> Option<String> {
        self.context
            .meta("loader.entry_id")
            .and_then(|value| value.as_str().map(str::to_owned))
    }

    /// Required service names for pending-fiber diagnostics.
    #[must_use]
    pub fn inject(&self) -> Vec<String> {
        self.required_services()
    }

    /// Adds a dependency during synchronous `internal/plugin` publication.
    ///
    /// # Errors
    ///
    /// Rejects mutation after lifecycle scheduling or disposal begins.
    pub fn add_inject(&self, name: impl Into<String>) -> Result<(), CordisError> {
        if self.scheduled.load(Ordering::Acquire) || self.disposed.load(Ordering::Acquire) {
            return Err(CordisError::InactiveEffect);
        }
        let name = name.into();
        let mut additional = self.additional_inject.lock();
        if !self.plugin.inject.contains(&name) && !additional.contains(&name) {
            additional.push(name);
        }
        Ok(())
    }

    /// Whether permanent disposal has been admitted.
    #[must_use]
    pub fn is_disposed(&self) -> bool {
        self.disposed.load(Ordering::Acquire)
    }

    /// Underlying lifecycle owner.
    #[must_use]
    pub fn fiber(&self) -> &Arc<Fiber> {
        &self.fiber
    }

    /// Plugin-scoped context.
    #[must_use]
    pub fn context(&self) -> &Context {
        &self.context
    }

    /// Last startup error after rendering its causal chain.
    #[must_use]
    pub fn error(&self) -> Option<String> {
        self.error.lock().clone()
    }

    /// Waits until currently scheduled lifecycle work reaches a stable state.
    ///
    /// # Errors
    ///
    /// Returns the plugin's most recent configuration or startup failure.
    pub async fn await_settled(&self) -> anyhow::Result<()> {
        loop {
            let notified = self.settled.notified();
            if !self.scheduled.load(Ordering::Acquire)
                && !matches!(
                    self.fiber.state(),
                    FiberState::Loading | FiberState::Unloading
                )
            {
                return self
                    .error
                    .lock()
                    .clone()
                    .map_or(Ok(()), |message| Err(anyhow::anyhow!(message)));
            }
            notified.await;
        }
    }

    fn request_update(self: &Arc<Self>, config: Value) {
        *self.config.lock() = config;
        *self.epoch.lock() = None;
        self.schedule();
    }

    /// Replaces configuration transactionally, restoring the exact previous
    /// active generation and raw configuration when candidate activation fails.
    ///
    /// Concurrent callers serialize in admission order.
    ///
    /// # Errors
    ///
    /// Returns candidate activation, rollback activation, or disposal failures.
    pub async fn update(self: &Arc<Self>, config: Value) -> anyhow::Result<()> {
        let _update = self.updates.lock().await;
        anyhow::ensure!(
            !self.disposed.load(Ordering::Acquire),
            "plugin {:?} is disposed",
            self.plugin.name
        );
        let previous = self.config.lock().clone();
        self.request_update(config);
        match self.await_settled().await {
            Ok(()) if self.fiber.state() != FiberState::Failed => Ok(()),
            Ok(()) => Err(anyhow::anyhow!(
                "plugin {:?} failed without a retained error",
                self.plugin.name
            )),
            Err(candidate) => {
                self.request_update(previous);
                match self.await_settled().await {
                    Ok(()) if self.fiber.state() == FiberState::Active => Err(candidate),
                    Ok(()) => Err(anyhow::anyhow!(
                        "{candidate:#}\nplugin {:?} rollback did not reactivate (state {:?})",
                        self.plugin.name,
                        self.fiber.state()
                    )),
                    Err(rollback) => Err(anyhow::anyhow!(
                        "{candidate:#}\nplugin {:?} rollback failed: {rollback:#}",
                        self.plugin.name
                    )),
                }
            }
        }
    }

    /// Explicit name for [`Self::update`] at transaction-oriented call sites.
    ///
    /// # Errors
    ///
    /// Returns the same candidate or rollback failure as [`Self::update`].
    pub async fn update_transactional(self: &Arc<Self>, config: Value) -> anyhow::Result<()> {
        self.update(config).await
    }

    /// Exact raw configuration currently committed to this fiber.
    #[must_use]
    pub fn config(&self) -> Value {
        self.config.lock().clone()
    }

    /// Permanently disposes this mount and all plugin-owned effects.
    ///
    /// # Errors
    ///
    /// Returns aggregated plugin cleanup failures.
    pub async fn dispose(self: &Arc<Self>) -> anyhow::Result<()> {
        if self.disposed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let on_async_error: Arc<dyn Fn(anyhow::Error) + Send + Sync> = Arc::new(|error| {
            tracing::error!(%error, "internal/plugin disposal observer failed");
        });
        match self.context.events().prepare_emit(
            &self.context,
            "internal/plugin",
            &EventArgs::one_shared(self.clone()),
        ) {
            Ok(emission) => emission.emit_contained_with_async_errors(
                |error| tracing::error!(%error, "internal/plugin disposal observer failed"),
                &on_async_error,
            ),
            Err(error) => {
                tracing::error!(%error, "internal/plugin disposal publication failed");
            }
        }
        self.fiber.request_disposal();
        let _transition = self.transition.lock().await;
        let result = self.fiber.dispose().await;
        self.uid.store(0, Ordering::Release);
        self.notify_registry();
        self.settled.notify_waiters();
        result
    }

    fn schedule(self: &Arc<Self>) {
        if self.disposed.load(Ordering::Acquire) {
            return;
        }
        self.dirty.store(true, Ordering::Release);
        if self.scheduled.swap(true, Ordering::AcqRel) {
            return;
        }
        let fiber = self.clone();
        spawn_background(async move {
            loop {
                fiber.dirty.store(false, Ordering::Release);
                fiber.reconcile().await;
                fiber.settled.notify_waiters();
                if !fiber.dirty.load(Ordering::Acquire) {
                    break;
                }
            }
            fiber.scheduled.store(false, Ordering::Release);
            if fiber.dirty.swap(false, Ordering::AcqRel) {
                fiber.schedule();
            }
            fiber.settled.notify_waiters();
        });
    }

    fn resolve_config(&self, raw_config: &Value) -> anyhow::Result<Value> {
        let config = self.plugin.config_resolver.as_ref().map_or_else(
            || Ok(raw_config.clone()),
            |resolver| {
                catch_unwind(AssertUnwindSafe(|| resolver(&self.context, raw_config)))
                    .unwrap_or_else(|panic| {
                        Err(anyhow::anyhow!(
                            "plugin config resolution panicked: {}",
                            panic_detail(panic.as_ref())
                        ))
                    })
            },
        )?;
        self.plugin
            .validator
            .as_ref()
            .map_or(Ok(config.clone()), |validator| {
                catch_unwind(AssertUnwindSafe(|| validator(&config))).unwrap_or_else(|panic| {
                    Err(anyhow::anyhow!(
                        "plugin config validation panicked: {}",
                        panic_detail(panic.as_ref())
                    ))
                })
            })
    }

    async fn run_startup(&self, config: Value) -> anyhow::Result<()> {
        let startup = catch_unwind(AssertUnwindSafe(|| {
            (self.plugin.callback)(self.context.clone(), config)
        }));
        match startup {
            Ok(startup) => AssertUnwindSafe(startup)
                .catch_unwind()
                .await
                .unwrap_or_else(|panic| {
                    Err(anyhow::anyhow!(
                        "plugin startup panicked: {}",
                        panic_detail(panic.as_ref())
                    ))
                }),
            Err(panic) => Err(anyhow::anyhow!(
                "plugin startup panicked: {}",
                panic_detail(panic.as_ref())
            )),
        }
    }

    async fn reconcile(&self) {
        let _transition = self.transition.lock().await;
        if self.disposed.load(Ordering::Acquire) {
            return;
        }
        let required = self.required_services();
        let next_epoch = required
            .iter()
            .map(|name| self.context.provider_id(name))
            .collect::<Option<Vec<_>>>();
        let previous_epoch = self.epoch.lock().clone();

        let Some(next_epoch) = next_epoch else {
            if !matches!(self.fiber.state(), FiberState::Pending) {
                if let Err(error) = self.fiber.deactivate().await {
                    tracing::error!(plugin = %self.plugin.name, %error, "plugin deactivation failed");
                }
                self.notify_registry();
            }
            *self.epoch.lock() = None;
            *self.error.lock() = None;
            return;
        };

        if matches!(self.fiber.state(), FiberState::Active | FiberState::Failed)
            && previous_epoch.as_ref() == Some(&next_epoch)
        {
            return;
        }
        if self.fiber.state() == FiberState::Active {
            if let Err(error) = self.fiber.deactivate().await {
                tracing::error!(plugin = %self.plugin.name, %error, "plugin reload cleanup failed");
            }
            self.notify_registry();
        }
        self.fiber.set_state(FiberState::Loading);
        let raw_config = self.config.lock().clone();
        let result = match self.resolve_config(&raw_config) {
            Ok(config) => self.run_startup(config).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => {
                *self.epoch.lock() = Some(next_epoch);
                *self.error.lock() = None;
                self.fiber.set_state(FiberState::Active);
            }
            Err(error) => {
                let message = format!("{error:#}");
                if let Err(cleanup_error) = self.fiber.fail().await {
                    tracing::error!(plugin = %self.plugin.name, %cleanup_error, "failed plugin rollback failed");
                }
                // A failed activation is settled for this exact dependency
                // epoch. Keeping `None` here makes the service-change
                // notification below immediately reschedule a dependency-free
                // plugin forever; a config update explicitly clears the epoch,
                // while a provider replacement naturally produces a new one.
                *self.epoch.lock() = Some(next_epoch);
                *self.error.lock() = Some(message.clone());
                tracing::error!(plugin = %self.plugin.name, error = %message, "plugin startup failed");
            }
        }
        self.notify_registry();
    }

    fn notify_registry(&self) {
        if let Some(registry) = self.registry.upgrade() {
            PluginRegistry { inner: registry }.notify_service_change();
        }
    }

    fn required_services(&self) -> Vec<String> {
        let mut required = self.plugin.inject.clone();
        for name in self.additional_inject.lock().iter() {
            if !required.contains(name) {
                required.push(name.clone());
            }
        }
        required
    }
}

fn panic_detail(panic: &(dyn Any + Send)) -> &str {
    panic
        .downcast_ref::<&'static str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_background(future: impl Future<Output = ()> + Send + 'static) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(future);
    } else {
        std::thread::spawn(move || futures::executor::block_on(future));
    }
}

#[cfg(target_arch = "wasm32")]
fn spawn_background(future: impl Future<Output = ()> + Send + 'static) {
    wasm_bindgen_futures::spawn_local(future);
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use crate::ServiceKey;

    use super::*;

    #[tokio::test]
    async fn dependency_free_startup_failure_settles_until_configuration_changes() {
        let context = Context::new();
        let attempts = Arc::new(AtomicUsize::new(0));
        let plugin = Plugin::new("fallible", std::iter::empty::<String>(), {
            let attempts = attempts.clone();
            move |_, config| {
                attempts.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    anyhow::ensure!(config == json!(true), "configured failure");
                    Ok(())
                })
            }
        });
        let fiber = context.plugin(plugin, json!(false)).unwrap();
        assert!(fiber.await_settled().await.is_err());
        tokio::task::yield_now().await;
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        fiber.update(json!(true)).await.unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        assert_eq!(fiber.fiber().state(), FiberState::Active);
    }

    #[derive(Debug)]
    struct Dependency;

    const DEPENDENCY: ServiceKey<Dependency> = ServiceKey::new("dependency");

    #[tokio::test]
    async fn dependency_appearance_and_removal_drive_plugin_lifecycle() {
        let context = Context::new();
        let starts = Arc::new(AtomicUsize::new(0));
        let stops = Arc::new(AtomicUsize::new(0));
        let plugin = Plugin::new("consumer", ["dependency"], {
            let starts = starts.clone();
            let stops = stops.clone();
            move |context, _| {
                starts.fetch_add(1, Ordering::SeqCst);
                let stops = stops.clone();
                Box::pin(async move {
                    context.own(EffectHandle::synchronous("consumer", move || {
                        stops.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }))?;
                    Ok(())
                })
            }
        });
        let mounted = context.plugin(plugin, json!({})).expect("mount");
        mounted.await_settled().await.expect("pending is stable");
        assert_eq!(mounted.fiber().state(), FiberState::Pending);

        let provider = context
            .provide(DEPENDENCY, Arc::new(Dependency))
            .expect("provide dependency");
        mounted.await_settled().await.expect("consumer loads");
        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert_eq!(mounted.fiber().state(), FiberState::Active);

        provider.dispose().await.expect("remove dependency");
        mounted.await_settled().await.expect("consumer unloads");
        assert_eq!(stops.load(Ordering::SeqCst), 1);
        assert_eq!(mounted.fiber().state(), FiberState::Pending);
    }

    #[tokio::test]
    async fn loading_provider_is_hidden_until_plugin_commits() {
        #[derive(Debug)]
        struct Provided;
        const PROVIDED: ServiceKey<Provided> = ServiceKey::new("provided");

        let context = Context::new();
        let plugin = Plugin::new(
            "provider",
            std::iter::empty::<String>(),
            move |context, _| {
                Box::pin(async move {
                    context.provide(PROVIDED, Arc::new(Provided))?;
                    assert!(context.get(PROVIDED).is_none());
                    assert!(context.get_relaxed(PROVIDED).is_some());
                    Ok(())
                })
            },
        );
        let mounted = context.plugin(plugin, Value::Null).expect("mount");
        mounted.await_settled().await.expect("provider loads");
        assert!(context.get(PROVIDED).is_some());
    }

    #[tokio::test]
    async fn transactional_update_restores_previous_config_and_effects_after_failure() {
        const CURRENT: ServiceKey<Value> = ServiceKey::new("current");
        let context = Context::new();
        let plugin = Plugin::new(
            "reloadable",
            std::iter::empty::<String>(),
            move |context, config| {
                Box::pin(async move {
                    anyhow::ensure!(config["fail"] != true, "candidate config failed");
                    context.provide(CURRENT, Arc::new(config))?;
                    Ok(())
                })
            },
        );
        let original = json!({"value": "old"});
        let fiber = context.plugin(plugin, original.clone()).unwrap();
        fiber.await_settled().await.unwrap();
        assert_eq!(context.get(CURRENT).as_deref(), Some(&original));

        let error = fiber
            .update(json!({"fail": true, "value": "bad"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("candidate config failed"));
        assert_eq!(fiber.fiber().state(), FiberState::Active);
        assert_eq!(fiber.config(), original);
        assert_eq!(context.get(CURRENT).as_deref(), Some(&original));

        let next = json!({"value": "new"});
        fiber.update_transactional(next.clone()).await.unwrap();
        assert_eq!(fiber.config(), next);
        assert_eq!(context.get(CURRENT).as_deref(), Some(&next));
    }

    #[tokio::test]
    async fn panicking_config_resolver_settles_as_a_lifecycle_failure() {
        let context = Context::new();
        let plugin = Plugin::new("panic", std::iter::empty::<String>(), |_, _| {
            Box::pin(async { Ok(()) })
        })
        .with_config_resolver(|_, _| panic!("resolver exploded"));
        let fiber = context.plugin(plugin, Value::Null).unwrap();
        let error =
            tokio::time::timeout(std::time::Duration::from_millis(100), fiber.await_settled())
                .await
                .expect("panicking resolver must settle")
                .unwrap_err();
        assert!(error.to_string().contains("resolver exploded"));
        assert_eq!(fiber.fiber().state(), FiberState::Failed);
    }

    #[tokio::test]
    async fn panicking_plugin_future_settles_as_a_lifecycle_failure() {
        let context = Context::new();
        let plugin = Plugin::new("panic", std::iter::empty::<String>(), |_, _| {
            Box::pin(async { panic!("plugin future exploded") })
        });
        let fiber = context.plugin(plugin, Value::Null).unwrap();
        let error =
            tokio::time::timeout(std::time::Duration::from_millis(100), fiber.await_settled())
                .await
                .expect("panicking plugin future must settle")
                .unwrap_err();
        assert!(error.to_string().contains("plugin future exploded"));
        assert_eq!(fiber.fiber().state(), FiberState::Failed);
    }
}
