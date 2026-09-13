//! Target-neutral lazy CJS table and exact resolution branch ordering.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Weak},
};

use futures::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use parking_lot::Mutex;

use crate::{BootModuleRow, ClientModuleId};

/// Synchronous CJS factory registered by one executed Client bundle.
pub type ClientModuleFactory<T> =
    Arc<dyn Fn(ClientModuleRequire<T>) -> anyhow::Result<T> + Send + Sync>;

/// Bundle transport used by stage-one arrival.
pub trait ClientBundleLoader<T>: Send + Sync + 'static
where
    T: Clone + Send + Sync + 'static,
{
    /// Loads and executes one bundle, which must register through `registrar`.
    fn load(
        &self,
        row: BootModuleRow,
        registrar: ClientFactoryRegistrar<T>,
    ) -> BoxFuture<'static, anyhow::Result<()>>;
}

/// Platform style inventory performed immediately after factory execution.
pub trait ClientStyleClaimer: Send + Sync + 'static {
    /// Claims unowned styles and returns owned CSS identities.
    fn claim(&self, id: &ClientModuleId) -> Vec<String>;
}

/// Materialized module record.
#[derive(Clone)]
pub struct ClientModuleRecord<T> {
    /// Module identity.
    pub id: ClientModuleId,
    /// Memoized exports.
    pub exports: T,
    /// Owned style identities.
    pub styles: Vec<String>,
    /// Original synchronous require specifiers.
    pub edges: BTreeSet<String>,
}

impl<T: std::fmt::Debug> std::fmt::Debug for ClientModuleRecord<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientModuleRecord")
            .field("id", &self.id)
            .field("exports", &self.exports)
            .field("styles", &self.styles)
            .field("edges", &self.edges)
            .finish()
    }
}

type Arrival = Shared<BoxFuture<'static, Result<(), Arc<str>>>>;

struct ModuleState<T> {
    statics: BTreeMap<String, T>,
    factories: BTreeMap<ClientModuleId, ClientModuleFactory<T>>,
    pending: BTreeMap<ClientModuleId, Arrival>,
    materializing: BTreeSet<ClientModuleId>,
    cache: BTreeMap<ClientModuleId, ClientModuleRecord<T>>,
}

struct ModuleInner<T> {
    seed: BTreeMap<String, T>,
    graph: BTreeMap<ClientModuleId, BootModuleRow>,
    loader: Arc<dyn ClientBundleLoader<T>>,
    styles: Arc<dyn ClientStyleClaimer>,
    state: Mutex<ModuleState<T>>,
}

/// Rust-owned lazy Client module system.
pub struct ClientModuleSystem<T> {
    inner: Arc<ModuleInner<T>>,
}

impl<T> Clone for ClientModuleSystem<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> std::fmt::Debug for ClientModuleSystem<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.inner.state.lock();
        formatter
            .debug_struct("ClientModuleSystem")
            .field("graph", &self.inner.graph.len())
            .field("statics", &state.statics.len())
            .field("factories", &state.factories.len())
            .field("pending", &state.pending.len())
            .field("cache", &state.cache.len())
            .finish_non_exhaustive()
    }
}

impl<T> ClientModuleSystem<T>
where
    T: Clone + Send + Sync + 'static,
{
    /// Builds a module table over boot rows, singleton seed words, and adapters.
    ///
    /// # Errors
    ///
    /// Rejects the first duplicate graph identity.
    pub fn new(
        modules: Vec<BootModuleRow>,
        seed: impl IntoIterator<Item = (String, T)>,
        loader: Arc<dyn ClientBundleLoader<T>>,
        styles: Arc<dyn ClientStyleClaimer>,
    ) -> anyhow::Result<Self> {
        let mut graph = BTreeMap::new();
        for row in modules {
            if graph.insert(row.id.clone(), row.clone()).is_some() {
                anyhow::bail!(
                    "client-modules: duplicate graph entry {:?}",
                    row.id.as_str()
                );
            }
        }
        Ok(Self {
            inner: Arc::new(ModuleInner {
                seed: seed.into_iter().collect(),
                graph,
                loader,
                styles,
                state: Mutex::new(ModuleState {
                    statics: BTreeMap::new(),
                    factories: BTreeMap::new(),
                    pending: BTreeMap::new(),
                    materializing: BTreeSet::new(),
                    cache: BTreeMap::new(),
                }),
            }),
        })
    }

    /// Registrar handed to bundle transports and the browser global sink.
    #[must_use]
    pub fn registrar(&self) -> ClientFactoryRegistrar<T> {
        ClientFactoryRegistrar {
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// Registers a shell-owned module.
    ///
    /// # Errors
    ///
    /// Rejects duplicate static identities.
    pub fn register_static(&self, id: impl Into<String>, module: T) -> anyhow::Result<()> {
        let id = id.into();
        let mut state = self.inner.state.lock();
        if state.statics.contains_key(&id) {
            anyhow::bail!("client-modules: shell-own module {id:?} registered twice");
        }
        state.statics.insert(id, module);
        Ok(())
    }

    /// Loads one graph row far enough to register its factory.
    ///
    /// # Errors
    ///
    /// Rejects unknown rows, transport failures, and bundles that fail to register.
    pub async fn prefetch(&self, id: &ClientModuleId) -> anyhow::Result<()> {
        if self.inner.state.lock().statics.contains_key(id.as_str()) {
            return Ok(());
        }
        let row = self.inner.graph.get(id).cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "client-modules: prefetch({:?}) — not a graph entry",
                id.as_str()
            )
        })?;
        self.arrive(row)
            .await
            .map_err(|message| anyhow::anyhow!(message.to_string()))
    }

    /// Imports one seed, cached, static, or graph module.
    ///
    /// # Errors
    ///
    /// Returns arrival, registration, factory, cycle, and resolution failures.
    pub async fn import(&self, specifier: &str) -> anyhow::Result<T> {
        if let Some(seed) = self.inner.seed.get(specifier) {
            return Ok(seed.clone());
        }
        let id = ClientModuleId::new(specifier);
        {
            let mut state = self.inner.state.lock();
            if let Some(record) = state.cache.get(&id) {
                return Ok(record.exports.clone());
            }
            if let Some(exports) = state.statics.get(specifier).cloned() {
                state.cache.insert(
                    id.clone(),
                    ClientModuleRecord {
                        id,
                        exports: exports.clone(),
                        styles: Vec::new(),
                        edges: BTreeSet::new(),
                    },
                );
                return Ok(exports);
            }
        }
        if !self.inner.state.lock().factories.contains_key(&id) {
            let row = self.inner.graph.get(&id).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "client-modules: cannot resolve {specifier:?} — not a seed word, not a shell-own module, and not a row in the boot graph (the runtime mirror of the bundle purity gate)"
                )
            })?;
            self.arrive(row)
                .await
                .map_err(|message| anyhow::anyhow!(message.to_string()))?;
        }
        Ok(self.materialize(&id)?.exports)
    }

    /// Drops one registered factory and materialized record.
    pub fn invalidate(&self, id: &ClientModuleId) {
        let mut state = self.inner.state.lock();
        state.factories.remove(id);
        state.cache.remove(id);
    }

    /// Snapshots the materialized cache for browser projection and tests.
    #[must_use]
    pub fn cache_snapshot(&self) -> BTreeMap<ClientModuleId, ClientModuleRecord<T>> {
        self.inner.state.lock().cache.clone()
    }

    fn arrive(&self, row: BootModuleRow) -> Arrival {
        {
            let state = self.inner.state.lock();
            if let Some(pending) = state.pending.get(&row.id) {
                return pending.clone();
            }
            if state.factories.contains_key(&row.id) {
                return futures::future::ready(Ok(())).boxed().shared();
            }
        }
        let system = self.clone();
        let loader = self.inner.loader.clone();
        let registrar = self.registrar();
        let id = row.id.clone();
        let pending_id = id.clone();
        let url = row.url.clone();
        let cleanup_id = id.clone();
        let arrival = async move {
            let result = loader.load(row, registrar).await.and_then(|()| {
                anyhow::ensure!(
                    system.inner.state.lock().factories.contains_key(&id),
                    "client-modules: bundle {url} loaded without registering {:?} via __ModuleLoader__.load",
                    id.as_str()
                );
                Ok(())
            });
            system.inner.state.lock().pending.remove(&cleanup_id);
            result.map_err(|error| Arc::<str>::from(format!("{error:#}")))
        }
        .boxed()
        .shared();
        self.inner
            .state
            .lock()
            .pending
            .insert(pending_id, arrival.clone());
        arrival
    }

    fn materialize(&self, id: &ClientModuleId) -> anyhow::Result<ClientModuleRecord<T>> {
        let factory = {
            let mut state = self.inner.state.lock();
            if let Some(record) = state.cache.get(id) {
                return Ok(record.clone());
            }
            let factory = state.factories.get(id).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "client-modules: no registered factory for {:?}",
                    id.as_str()
                )
            })?;
            if !state.materializing.insert(id.clone()) {
                anyhow::bail!(
                    "client-modules: require cycle through {:?} (factory-form CJS cannot deliver partial exports)",
                    id.as_str()
                );
            }
            factory
        };
        let edges = Arc::new(Mutex::new(BTreeSet::new()));
        let result = factory(ClientModuleRequire {
            system: self.clone(),
            edges: edges.clone(),
        });
        self.inner.state.lock().materializing.remove(id);
        let exports = result?;
        let record = ClientModuleRecord {
            id: id.clone(),
            exports,
            styles: self.inner.styles.claim(id),
            edges: edges.lock().clone(),
        };
        self.inner
            .state
            .lock()
            .cache
            .insert(id.clone(), record.clone());
        Ok(record)
    }

    fn require(&self, specifier: &str, edges: &Mutex<BTreeSet<String>>) -> anyhow::Result<T> {
        edges.lock().insert(specifier.to_owned());
        if let Some(seed) = self.inner.seed.get(specifier) {
            return Ok(seed.clone());
        }
        {
            let state = self.inner.state.lock();
            if let Some(value) = state.statics.get(specifier) {
                return Ok(value.clone());
            }
        }
        let id = ClientModuleId::new(strip_client_suffix(specifier));
        if let Some(record) = self.inner.state.lock().cache.get(&id) {
            return Ok(record.exports.clone());
        }
        if self.inner.state.lock().factories.contains_key(&id) {
            return Ok(self.materialize(&id)?.exports);
        }
        anyhow::bail!(
            "client-modules: require({specifier:?}) missed the module table — not a platform seed word, not a shell-own module, and no registered factory (a build-time externals drift, or a forbidden cross-plugin value import)"
        )
    }
}

/// Synchronous require capability scoped to one materializing factory.
#[derive(Clone)]
pub struct ClientModuleRequire<T> {
    system: ClientModuleSystem<T>,
    edges: Arc<Mutex<BTreeSet<String>>>,
}

impl<T> ClientModuleRequire<T>
where
    T: Clone + Send + Sync + 'static,
{
    /// Resolves one synchronous dependency.
    ///
    /// # Errors
    ///
    /// Returns table misses, factory failures, and require cycles.
    pub fn require(&self, specifier: &str) -> anyhow::Result<T> {
        self.system.require(specifier, &self.edges)
    }
}

/// Cloneable registration sink.
pub struct ClientFactoryRegistrar<T> {
    inner: Weak<ModuleInner<T>>,
}

impl<T> Clone for ClientFactoryRegistrar<T>
where
    T: Clone + Send + Sync + 'static,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> ClientFactoryRegistrar<T>
where
    T: Clone + Send + Sync + 'static,
{
    /// Registers one bundle factory.
    ///
    /// # Errors
    ///
    /// Rejects duplicate execution without invalidation or a dead table.
    pub fn register(
        &self,
        id: ClientModuleId,
        factory: ClientModuleFactory<T>,
    ) -> anyhow::Result<()> {
        let inner = self
            .inner
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("client-modules: module table is disposed"))?;
        let mut state = inner.state.lock();
        if state.factories.contains_key(&id) {
            anyhow::bail!(
                "client-modules: duplicate factory registration for {:?} (bundle executed twice without invalidate?)",
                id.as_str()
            );
        }
        state.factories.insert(id, factory);
        Ok(())
    }
}

fn strip_client_suffix(specifier: &str) -> &str {
    specifier.strip_suffix("/client").unwrap_or(specifier)
}
