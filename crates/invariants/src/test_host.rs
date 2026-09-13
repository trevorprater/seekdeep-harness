//! Root-scoped native companion startup for integration tests.

use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, OnceLock, Weak},
};

use futures::{
    FutureExt as _,
    future::{BoxFuture, Shared, try_join_all},
};
use parking_lot::{Mutex, RwLock};
use seekdeep_cordis::{
    Context, EventOptions, EventReply, Fiber, FiberState, Plugin, PluginFiber, ServiceKey,
    TEST_INVARIANT_READY_SERVICE, test_invariant_companion_paths, uses_manual_invariant_tree,
};
use serde_json::Value;

use crate::{InvariantConfig, InvariantRegistry};

const READY: ServiceKey<bool> = ServiceKey::new(TEST_INVARIANT_READY_SERVICE);
const ATTACHMENT_COMPANION: &str = "../packages/attachment/attachment-local/src/invariant.ts";

/// Retained cause shared by every caller joining one startup attempt.
pub type TestInvariantFailure = Arc<anyhow::Error>;
/// Lazy loader for a compiled native companion plugin.
pub type TestInvariantLoader =
    Arc<dyn Fn() -> BoxFuture<'static, Result<Plugin, TestInvariantFailure>> + Send + Sync>;
/// Live source-shaped companion map, also used by package-topology tests.
pub type TestInvariantCompanions = Arc<RwLock<BTreeMap<String, TestInvariantLoader>>>;
type Readiness = Shared<BoxFuture<'static, Result<(), TestInvariantFailure>>>;
type Spawn = Arc<dyn Fn(BoxFuture<'static, ()>) + Send + Sync>;

/// Wraps a lazy native module load without erasing the failure identity.
#[must_use]
pub fn companion_loader<F, Fut>(load: F) -> TestInvariantLoader
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Plugin, TestInvariantFailure>> + Send + 'static,
{
    Arc::new(move || Box::pin(load()))
}

struct StartedHost {
    by_callback: Mutex<Vec<(Plugin, Arc<PluginFiber>)>>,
    host_plugins: Mutex<Vec<Plugin>>,
    barrier_owners: Mutex<Vec<Weak<Fiber>>>,
    ready: OnceLock<Readiness>,
}

impl StartedHost {
    fn owns(&self, context: &Context) -> bool {
        self.barrier_owners
            .lock()
            .iter()
            .filter_map(Weak::upgrade)
            .any(|owner| {
                matches!(owner.state(), FiberState::Loading | FiberState::Active)
                    && context.fiber().is_within(&owner)
            })
    }

    fn readiness(&self) -> Readiness {
        self.ready
            .get()
            .expect("host readiness initialized before publication")
            .clone()
    }
}

struct Configuration {
    root: Context,
    test_path: String,
    companions: TestInvariantCompanions,
    registry: Plugin,
    attachment: Option<Plugin>,
    spawn: Spawn,
    started: Mutex<Option<Arc<StartedHost>>>,
}

/// Native entrypoint for tests that share an automatic invariant tree.
///
/// Mount through [`Self::plugin`] to join companion readiness. Plugins created
/// inside callbacks use ordinary Cordis contexts; the root's registration
/// observer gates pending descendants and admits active causal descendants.
#[derive(Clone)]
pub struct TestInvariantHost(Arc<Configuration>);

impl TestInvariantHost {
    /// Binds one root to its lazy native companion loaders and task executor.
    ///
    /// The attachment store is supplied by attachment-aware harnesses so this
    /// base registry crate does not depend on its own service consumers.
    #[must_use]
    pub fn new(
        root: Context,
        test_path: impl Into<String>,
        companions: TestInvariantCompanions,
        attachment: Option<Plugin>,
        spawn: impl Fn(BoxFuture<'static, ()>) + Send + Sync + 'static,
    ) -> Self {
        let registry = Plugin::new(
            "invariants",
            std::iter::empty::<String>(),
            |context, config| {
                Box::pin(async move {
                    let config = if config.is_null() {
                        InvariantConfig::default()
                    } else {
                        serde_json::from_value(config)?
                    };
                    InvariantRegistry::install(&context, &config)?;
                    Ok(())
                })
            },
        );
        Self(Arc::new(Configuration {
            root,
            test_path: test_path.into(),
            companions,
            registry,
            attachment,
            spawn: Arc::new(spawn),
            started: Mutex::new(None),
        }))
    }

    /// Stable service-plugin identity, shared by explicit and automatic mounts.
    #[must_use]
    pub fn registry_plugin(&self) -> Plugin {
        self.0.registry.clone()
    }

    /// Mounts or reuses a plugin and joins its root's invariant startup.
    ///
    /// # Errors
    /// Preserves companion-selection and synchronous registration failures.
    pub fn plugin(
        &self,
        context: &Context,
        plugin: Plugin,
        config: Value,
    ) -> anyhow::Result<TestInvariantPlugin> {
        anyhow::ensure!(
            Arc::ptr_eq(context.root_fiber(), self.0.root.root_fiber()),
            "test invariants: plugin context belongs to another root"
        );
        if uses_manual_invariant_tree(&self.0.test_path) {
            return Ok(TestInvariantPlugin::new(
                context.plugin(plugin, config)?,
                None,
                false,
            ));
        }
        let state = self.start()?;
        let existing = state
            .by_callback
            .lock()
            .iter()
            .find(|(mounted, _)| mounted.id() == plugin.id())
            .map(|(_, fiber)| fiber.clone());
        let bypass = state.owns(context);
        let (fiber, initially_pending) = if let Some(existing) = existing {
            (existing, false)
        } else {
            let fiber = context.plugin(plugin, config)?;
            let pending = fiber.fiber().state() == FiberState::Pending;
            (fiber, pending)
        };
        Ok(TestInvariantPlugin::new(
            fiber,
            (!bypass).then(|| state.readiness()),
            initially_pending && !bypass,
        ))
    }

    fn start(&self) -> anyhow::Result<Arc<StartedHost>> {
        let mut existing = self.0.started.lock();
        if let Some(state) = existing.as_ref() {
            return Ok(state.clone());
        }
        let state = Arc::new(StartedHost {
            by_callback: Mutex::new(Vec::new()),
            host_plugins: Mutex::new(Vec::new()),
            barrier_owners: Mutex::new(Vec::new()),
            ready: OnceLock::new(),
        });
        observe_mounts(&self.0.root, &state)?;
        let service = mount(
            &self.0.root,
            &state,
            self.0.registry.clone(),
            serde_json::json!({ "enabled": true }),
        )?;
        let paths = test_invariant_companion_paths(
            &self.0.test_path,
            self.0.companions.read().keys().cloned(),
        )
        .map_err(anyhow::Error::msg)?;
        let ready = Startup {
            root: self.0.root.clone(),
            state: Arc::downgrade(&state),
            companions: self.0.companions.clone(),
            paths,
            attachment: self.0.attachment.clone(),
            service,
            spawn: self.0.spawn.clone(),
        }
        .run()
        .boxed()
        .shared();
        state
            .ready
            .set(ready.clone())
            .unwrap_or_else(|_| unreachable!("fresh host readiness"));
        *existing = Some(state.clone());
        (self.0.spawn)(
            async move {
                let _ = ready.await;
            }
            .boxed(),
        );
        Ok(state)
    }
}

fn observe_mounts(root: &Context, state: &Arc<StartedHost>) -> anyhow::Result<()> {
    let observed = Arc::downgrade(state);
    root.events().on_sync(
        root,
        "internal/plugin",
        move |parent, args| {
            let Some(state) = observed.upgrade() else {
                return Ok(EventReply::Undefined);
            };
            let Some(fiber) = args.get::<PluginFiber>(0) else {
                return Ok(EventReply::Undefined);
            };
            let host_plugin = state
                .host_plugins
                .lock()
                .iter()
                .any(|plugin| plugin.id() == fiber.plugin_id());
            let gated = !host_plugin && !state.owns(&parent);
            if gated {
                fiber.add_inject(TEST_INVARIANT_READY_SERVICE)?;
            }
            if gated || host_plugin {
                state
                    .barrier_owners
                    .lock()
                    .push(Arc::downgrade(fiber.fiber()));
            }
            Ok(EventReply::Undefined)
        },
        EventOptions::default(),
    )?;
    Ok(())
}

struct Startup {
    root: Context,
    state: Weak<StartedHost>,
    companions: TestInvariantCompanions,
    paths: Vec<String>,
    attachment: Option<Plugin>,
    service: Arc<PluginFiber>,
    spawn: Spawn,
}

impl Startup {
    async fn run(self) -> Result<(), TestInvariantFailure> {
        require_active(&self.service, "invariant service").await?;
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| Arc::new(anyhow::anyhow!("test invariant host was disposed")))?;
        let attachment = if self.paths.iter().any(|path| path == ATTACHMENT_COMPANION) {
            let plugin = self.attachment.ok_or_else(|| {
                Arc::new(anyhow::anyhow!(
                    "test invariants: attachment companion requires a test attachment store"
                ))
            })?;
            Some(mount(&self.root, &state, plugin, Value::Null).map_err(Arc::new)?)
        } else {
            None
        };
        let loads = self
            .paths
            .into_iter()
            .map(|path| {
                let loaded = load_companion(&self.companions, path).shared();
                let running = loaded.clone();
                (self.spawn)(
                    async move {
                        let _ = running.await;
                    }
                    .boxed(),
                );
                loaded
            })
            .collect::<Vec<_>>();
        let modules = try_join_all(loads).await?;
        let fibers = modules
            .into_iter()
            .map(|(path, plugin)| {
                mount(&self.root, &state, plugin, Value::Null)
                    .map(|fiber| (path, fiber))
                    .map_err(Arc::new)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut activations = fibers
            .iter()
            .map(|(path, fiber)| require_active(fiber, path).boxed())
            .collect::<Vec<_>>();
        if let Some(fiber) = &attachment {
            activations.insert(0, require_active(fiber, "test attachment store").boxed());
        }
        try_join_all(activations).await?;
        self.root
            .provide(READY, Arc::new(true))
            .map_err(|error| Arc::new(error.into()))?;
        Ok(())
    }
}

fn load_companion(
    companions: &TestInvariantCompanions,
    path: String,
) -> BoxFuture<'static, Result<(String, Plugin), TestInvariantFailure>> {
    let load = companions.read().get(&path).cloned();
    let loaded = load.map(|load| load());
    async move {
        let loaded = loaded.ok_or_else(|| {
            Arc::new(anyhow::anyhow!(
                "test invariants: selected companion vanished at {path}"
            ))
        })?;
        let plugin = loaded.await?;
        if !plugin.inject().iter().any(|name| name == "invariants") {
            return Err(Arc::new(anyhow::anyhow!(
                "test invariants: {path} must inject the invariant service"
            )));
        }
        Ok((path, plugin))
    }
    .boxed()
}

fn mount(
    root: &Context,
    state: &StartedHost,
    plugin: Plugin,
    config: Value,
) -> anyhow::Result<Arc<PluginFiber>> {
    state.host_plugins.lock().push(plugin.clone());
    let fiber = root.plugin(plugin.clone(), config).inspect_err(|_| {
        let mut plugins = state.host_plugins.lock();
        if let Some(index) = plugins
            .iter()
            .rposition(|mounted| mounted.id() == plugin.id())
        {
            plugins.remove(index);
        }
    })?;
    state.by_callback.lock().push((plugin, fiber.clone()));
    Ok(fiber)
}

async fn require_active(fiber: &Arc<PluginFiber>, label: &str) -> Result<(), TestInvariantFailure> {
    fiber
        .await_settled()
        .await
        .map_err(|error| fiber.failure().unwrap_or_else(|| Arc::new(error)))?;
    if fiber.fiber().state() != FiberState::Active {
        return Err(Arc::new(anyhow::anyhow!(
            "test invariants: {label} settled without becoming active"
        )));
    }
    Ok(())
}

/// Plugin fiber with a separately joined invariant readiness barrier.
#[derive(Clone)]
pub struct TestInvariantPlugin {
    raw: Arc<PluginFiber>,
    ready: Readiness,
}

impl TestInvariantPlugin {
    fn new(
        raw: Arc<PluginFiber>,
        readiness: Option<Readiness>,
        dispose_pending_validation: bool,
    ) -> Self {
        let awaited = raw.clone();
        let ready = async move {
            if let Some(readiness) = readiness {
                readiness.await?;
            }
            match awaited.await_settled().await {
                Ok(()) => Ok(()),
                Err(error) => {
                    let failure = awaited.failure().unwrap_or_else(|| Arc::new(error));
                    if dispose_pending_validation
                        && failure
                            .downcast_ref::<seekdeep_schemastery::ValidationError>()
                            .is_some()
                    {
                        awaited.dispose().await.map_err(Arc::new)?;
                    }
                    Err(failure)
                }
            }
        }
        .boxed()
        .shared();
        Self { raw, ready }
    }

    /// Real native fiber whose disposal never waits on the companion barrier.
    #[must_use]
    pub fn raw(&self) -> &Arc<PluginFiber> {
        &self.raw
    }

    /// Joins invariant and plugin startup, retaining one failure for every waiter.
    ///
    /// # Errors
    /// Returns the original retained companion or plugin startup failure.
    pub async fn await_ready(&self) -> Result<(), TestInvariantFailure> {
        self.ready.clone().await
    }

    /// Disposes the real plugin immediately, including while companions are pending.
    ///
    /// # Errors
    /// Returns ordinary Cordis teardown failures.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        self.raw.dispose().await
    }
}
