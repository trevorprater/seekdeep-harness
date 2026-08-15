//! Plugin registration and dependency-epoch lifecycle management.

use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
};

use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::{
    Context, CordisError, Fiber, FiberState,
    fiber::{DisposeFuture, EffectHandle},
};

/// Boxed plugin startup computation.
pub type PluginFuture = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'static>>;

type PluginCallback = Arc<dyn Fn(Context, Value) -> PluginFuture + Send + Sync>;
type ConfigValidator = Arc<dyn Fn(&Value) -> anyhow::Result<Value> + Send + Sync>;

/// Shared plugin entrypoint metadata and executable body.
#[derive(Clone)]
pub struct Plugin {
    id: Uuid,
    name: String,
    inject: Vec<String>,
    callback: PluginCallback,
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
            callback: Arc::new(callback),
            validator: None,
        }
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
        let fiber = Fiber::child(plugin.name.clone());
        let mounted = Arc::new(PluginFiber {
            fiber: fiber.clone(),
            context: parent.with_fiber(fiber),
            plugin,
            config: Mutex::new(config),
            epoch: Mutex::new(None),
            error: Mutex::new(None),
            transition: tokio::sync::Mutex::new(()),
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

    /// Number of currently reachable plugin mounts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .fibers
            .lock()
            .iter()
            .filter(|fiber| fiber.strong_count() > 0)
            .count()
    }

    /// Whether the registry contains no reachable plugin mounts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One mounted plugin and its current dependency epoch.
pub struct PluginFiber {
    fiber: Arc<Fiber>,
    context: Context,
    plugin: Plugin,
    config: Mutex<Value>,
    epoch: Mutex<Option<Vec<Uuid>>>,
    error: Mutex<Option<String>>,
    transition: tokio::sync::Mutex<()>,
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

    /// Replaces raw configuration and restarts when dependencies are available.
    pub fn update(self: &Arc<Self>, config: Value) {
        *self.config.lock() = config;
        *self.epoch.lock() = None;
        self.schedule();
    }

    /// Permanently disposes this mount and all plugin-owned effects.
    ///
    /// # Errors
    ///
    /// Returns aggregated plugin cleanup failures.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        if self.disposed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let _transition = self.transition.lock().await;
        let result = self.fiber.dispose().await;
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

    async fn reconcile(&self) {
        let _transition = self.transition.lock().await;
        if self.disposed.load(Ordering::Acquire) {
            return;
        }
        let next_epoch = self
            .plugin
            .inject
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

        if self.fiber.state() == FiberState::Active && previous_epoch.as_ref() == Some(&next_epoch)
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
        let config = self.plugin.validator.as_ref().map_or_else(
            || Ok(raw_config.clone()),
            |validator| validator(&raw_config),
        );
        let result = match config {
            Ok(config) => (self.plugin.callback)(self.context.clone(), config).await,
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
                *self.epoch.lock() = None;
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
}

fn spawn_background(future: impl Future<Output = ()> + Send + 'static) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(future);
    } else {
        std::thread::spawn(move || futures::executor::block_on(future));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    use crate::ServiceKey;

    use super::*;

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
}
