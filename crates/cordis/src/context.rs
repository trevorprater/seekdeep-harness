//! Scoped dependency container.

use std::{
    collections::{BTreeMap, HashMap},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use parking_lot::Mutex;

use serde_json::Value;
use uuid::Uuid;

use crate::{
    events::EventBus,
    fiber::{CordisError, EffectHandle, Fiber},
    plugin::{Plugin, PluginFiber, PluginRegistry},
    service::{Service, ServiceKey, ServiceSlot, ServiceStore},
};

type EventFilter = Arc<dyn Fn(&Context) -> bool + Send + Sync>;
type ServiceChangeListener = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct ServiceChangeBus {
    listeners: Mutex<HashMap<Uuid, ServiceChangeListener>>,
}

impl ServiceChangeBus {
    fn notify(&self) {
        let listeners = self.listeners.lock().values().cloned().collect::<Vec<_>>();
        for listener in listeners {
            let _ = catch_unwind(AssertUnwindSafe(|| listener()));
        }
    }
}

struct Root {
    events: EventBus,
    services: Arc<ServiceStore>,
    service_changes: Arc<ServiceChangeBus>,
    plugins: PluginRegistry,
    next_isolation: AtomicU64,
    named_isolations: Mutex<HashMap<(String, String), Weak<IsolationRealm>>>,
}

#[derive(Debug)]
struct IsolationRealm {
    id: Uuid,
}

/// Cloneable context view carrying service isolation and plugin ownership.
#[derive(Clone)]
pub struct Context {
    root: Arc<Root>,
    fiber: Arc<Fiber>,
    tree_owner: Arc<Fiber>,
    isolation: Arc<HashMap<String, Arc<IsolationRealm>>>,
    intercepts: Arc<HashMap<String, Vec<Value>>>,
    metadata: Arc<HashMap<String, Value>>,
    filter: Option<EventFilter>,
}

impl std::fmt::Debug for Context {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Context")
            .field("fiber", &self.fiber.name())
            .field("isolation", &self.isolation)
            .field("intercepts", &self.intercepts)
            .finish_non_exhaustive()
    }
}

impl Default for Context {
    fn default() -> Self {
        Self::new()
    }
}

impl Context {
    /// Creates a root context with built-in event and service registries.
    #[must_use]
    pub fn new() -> Self {
        let fiber = Fiber::root();
        let root = Arc::new(Root {
            events: EventBus::new(),
            services: Arc::new(ServiceStore::default()),
            service_changes: Arc::new(ServiceChangeBus::default()),
            plugins: PluginRegistry::new(),
            next_isolation: AtomicU64::new(1),
            named_isolations: Mutex::new(HashMap::new()),
        });
        Self {
            root,
            fiber: fiber.clone(),
            tree_owner: fiber,
            isolation: Arc::new(HashMap::new()),
            intercepts: Arc::new(HashMap::new()),
            metadata: Arc::new(HashMap::new()),
            filter: None,
        }
    }

    /// Root-owned event bus shared by every child context.
    #[must_use]
    pub fn events(&self) -> &EventBus {
        &self.root.events
    }

    /// Fiber that owns effects registered through this context.
    #[must_use]
    pub fn fiber(&self) -> &Arc<Fiber> {
        &self.fiber
    }

    /// Fiber that owns the complete context tree.
    #[must_use]
    pub fn root_fiber(&self) -> &Arc<Fiber> {
        &self.tree_owner
    }

    /// Creates a child context owned by `fiber`.
    #[must_use]
    pub fn with_fiber(&self, fiber: Arc<Fiber>) -> Self {
        Self {
            root: self.root.clone(),
            fiber,
            tree_owner: self.tree_owner.clone(),
            isolation: self.isolation.clone(),
            intercepts: self.intercepts.clone(),
            metadata: self.metadata.clone(),
            filter: self.filter.clone(),
        }
    }

    /// Replaces the current lifecycle owner and declares it as the root seen
    /// by every descendant plugin context.
    #[must_use]
    pub fn with_root_fiber(&self, fiber: Arc<Fiber>) -> Self {
        Self {
            root: self.root.clone(),
            fiber: fiber.clone(),
            tree_owner: fiber,
            isolation: self.isolation.clone(),
            intercepts: self.intercepts.clone(),
            metadata: self.metadata.clone(),
            filter: self.filter.clone(),
        }
    }

    /// Creates an independently scoped slot for one service name.
    #[must_use]
    pub fn isolate<T: Service>(&self, key: ServiceKey<T>) -> Self {
        self.isolate_named(key.name())
    }

    /// Creates an independently scoped slot for a dynamically named service.
    #[must_use]
    pub fn isolate_named(&self, name: &str) -> Self {
        self.with_isolation(name, self.new_isolation())
    }

    /// Creates or reuses a named isolation realm shared by contexts carrying
    /// the same service name and label in this root.
    #[must_use]
    pub fn isolate_named_as(&self, name: &str, label: &str) -> Self {
        let key = (name.to_owned(), label.to_owned());
        let realm = {
            let mut realms = self.root.named_isolations.lock();
            realms.retain(|_, realm| realm.strong_count() > 0);
            realms.get(&key).and_then(Weak::upgrade).unwrap_or_else(|| {
                let realm = self.new_isolation();
                realms.insert(key, Arc::downgrade(&realm));
                realm
            })
        };
        self.with_isolation(name, realm)
    }

    fn new_isolation(&self) -> Arc<IsolationRealm> {
        Arc::new(IsolationRealm {
            id: Uuid::from_u128(u128::from(
                self.root.next_isolation.fetch_add(1, Ordering::Relaxed),
            )),
        })
    }

    fn with_isolation(&self, name: &str, realm: Arc<IsolationRealm>) -> Self {
        let mut isolation = (*self.isolation).clone();
        isolation.insert(name.to_owned(), realm);
        Self {
            root: self.root.clone(),
            fiber: self.fiber.clone(),
            tree_owner: self.tree_owner.clone(),
            isolation: Arc::new(isolation),
            intercepts: self.intercepts.clone(),
            metadata: self.metadata.clone(),
            filter: self.filter.clone(),
        }
    }

    /// Adds service-specific configuration merged below this context.
    #[must_use]
    pub fn intercept(&self, name: &str, config: Value) -> Self {
        let mut intercepts = (*self.intercepts).clone();
        intercepts.entry(name.to_owned()).or_default().push(config);
        Self {
            root: self.root.clone(),
            fiber: self.fiber.clone(),
            tree_owner: self.tree_owner.clone(),
            isolation: self.isolation.clone(),
            intercepts: Arc::new(intercepts),
            metadata: self.metadata.clone(),
            filter: self.filter.clone(),
        }
    }

    /// Returns accumulated intercept configuration for a service using the
    /// source runtime's shallow `Object.assign` semantics.
    #[must_use]
    pub fn intercepted(&self, name: &str) -> Option<Value> {
        self.intercepts
            .get(name)
            .map(|configs| shallow_merge(configs.iter()))
    }

    /// Resolves optional base and head values around inherited intercepts.
    ///
    /// The order is base, root-to-leaf intercepts, then head. Nested objects
    /// replace each other wholesale, matching Cordis `Object.assign` rather
    /// than recursively merging.
    #[must_use]
    pub fn resolve_intercepted(
        &self,
        name: &str,
        base: Option<&Value>,
        head: Option<&Value>,
    ) -> Value {
        shallow_merge(
            base.into_iter()
                .chain(self.intercepts.get(name).into_iter().flatten())
                .chain(head),
        )
    }

    /// Resolves the same ordered layers with a service-owned merge function.
    #[must_use]
    pub fn resolve_intercepted_with(
        &self,
        name: &str,
        base: Option<&Value>,
        head: Option<&Value>,
        merge: impl FnOnce(&[Value]) -> Value,
    ) -> Value {
        let layers = base
            .into_iter()
            .chain(self.intercepts.get(name).into_iter().flatten())
            .chain(head)
            .cloned()
            .collect::<Vec<_>>();
        merge(&layers)
    }

    /// Adds a dispatch filter consulted for non-global listeners.
    #[must_use]
    pub fn with_event_filter(
        &self,
        filter: impl Fn(&Context) -> bool + Send + Sync + 'static,
    ) -> Self {
        let parent = self.filter.clone();
        let mut child = self.clone();
        child.filter = Some(Arc::new(move |listener| {
            parent.as_ref().is_none_or(|parent| parent(listener)) && filter(listener)
        }));
        child
    }

    pub(crate) fn accepts_listener(&self, listener_context: &Context) -> bool {
        self.filter
            .as_ref()
            .is_none_or(|filter| filter(listener_context))
    }

    /// Creates a child view with one nearest-wins metadata entry.
    #[must_use]
    pub fn with_meta(&self, key: impl Into<String>, value: Value) -> Self {
        let mut metadata = (*self.metadata).clone();
        metadata.insert(key.into(), value);
        let mut child = self.clone();
        child.metadata = Arc::new(metadata);
        child
    }

    /// Reads shared context metadata.
    #[must_use]
    pub fn meta(&self, key: &str) -> Option<Value> {
        self.metadata.get(key).cloned()
    }

    /// Registers a reversible effect on the current fiber.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] after the owning fiber begins disposal.
    pub fn own(&self, effect: EffectHandle) -> Result<EffectHandle, CordisError> {
        self.fiber.own(effect)
    }

    /// Observes every successful service provision and withdrawal.
    ///
    /// Listener failures are contained so one integration cannot prevent
    /// plugin dependency reconciliation or later listeners.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] after the owning fiber begins disposal.
    pub fn on_service_change(
        &self,
        listener: impl Fn() + Send + Sync + 'static,
    ) -> Result<EffectHandle, CordisError> {
        let id = Uuid::now_v7();
        self.root
            .service_changes
            .listeners
            .lock()
            .insert(id, Arc::new(listener));
        let changes = self.root.service_changes.clone();
        let effect = EffectHandle::synchronous("ctx.on_service_change", move || {
            changes.listeners.lock().remove(&id);
            Ok(())
        });
        match self.own(effect.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                self.root.service_changes.listeners.lock().remove(&id);
                Err(error)
            }
        }
    }

    /// Provides a typed service until the returned effect is disposed.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] after the owning fiber begins disposal.
    pub fn provide<T: Service>(
        &self,
        key: ServiceKey<T>,
        value: Arc<T>,
    ) -> Result<EffectHandle, CordisError> {
        self.provide_named_inner(key.name(), value, None)
    }

    /// Provides a typed service plus the immutable JSON value visible to
    /// Loader compatibility expressions.
    ///
    /// The projection is data, not a retained callback, so evaluators cannot
    /// invoke Rust methods or smuggle lifecycle owners across the boundary.
    ///
    /// # Errors
    ///
    /// Returns the same inactive or duplicate failures as [`Self::provide`].
    pub fn provide_projected<T: Service>(
        &self,
        key: ServiceKey<T>,
        value: Arc<T>,
        expression_projection: Value,
    ) -> Result<EffectHandle, CordisError> {
        self.provide_named_inner(key.name(), value, Some(expression_projection))
    }

    /// Provides a typed service under a runtime-computed name.
    ///
    /// Generated namespace services use this form while preserving the same
    /// isolation, duplicate, lifecycle, and revision semantics as a static
    /// [`ServiceKey`].
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] after disposal begins or
    /// [`CordisError::DuplicateService`] when the visible slot is occupied.
    pub fn provide_named<T: Service>(
        &self,
        name: &str,
        value: Arc<T>,
    ) -> Result<EffectHandle, CordisError> {
        self.provide_named_inner(name, value, None)
    }

    /// Runtime-name form of [`Self::provide_projected`].
    ///
    /// # Errors
    ///
    /// Returns the same inactive or duplicate failures as [`Self::provide_named`].
    pub fn provide_named_projected<T: Service>(
        &self,
        name: &str,
        value: Arc<T>,
        expression_projection: Value,
    ) -> Result<EffectHandle, CordisError> {
        self.provide_named_inner(name, value, Some(expression_projection))
    }

    fn provide_named_inner<T: Service>(
        &self,
        name: &str,
        value: Arc<T>,
        expression_projection: Option<Value>,
    ) -> Result<EffectHandle, CordisError> {
        let slot = self.slot(name);
        let Some(id) =
            self.root
                .services
                .insert(slot.clone(), &self.fiber, value, expression_projection)
        else {
            return Err(CordisError::DuplicateService(name.to_owned()));
        };
        self.root.plugins.notify_service_change();
        self.root.service_changes.notify();
        let services = self.root.services.clone();
        let plugins = self.root.plugins.clone();
        let service_changes = self.root.service_changes.clone();
        let disposal_slot = slot.clone();
        let effect = EffectHandle::synchronous(format!("ctx.provide({name:?})"), move || {
            if services.remove(&disposal_slot, id) {
                plugins.notify_service_change();
                service_changes.notify();
            }
            Ok(())
        });
        match self.own(effect.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                self.root.services.remove(&slot, id);
                self.root.plugins.notify_service_change();
                self.root.service_changes.notify();
                Err(error)
            }
        }
    }

    /// Resolves the newest provider visible in this context's isolation scope.
    #[must_use]
    pub fn get<T: Service>(&self, key: ServiceKey<T>) -> Option<Arc<T>> {
        self.get_named(key.name())
    }

    /// Resolves a typed service under a runtime-computed name.
    #[must_use]
    pub fn get_named<T: Service>(&self, name: &str) -> Option<Arc<T>> {
        self.root.services.get(&self.slot(name), true)
    }

    /// Resolves a provider even while its owning plugin is loading.
    #[must_use]
    pub fn get_relaxed<T: Service>(&self, key: ServiceKey<T>) -> Option<Arc<T>> {
        self.root.services.get(&self.slot(key.name()), false)
    }

    /// Replaces a service value from the exact fiber that provided it.
    ///
    /// # Errors
    ///
    /// Returns source-compatible missing-provider or cross-fiber ownership
    /// failures. Replacement does not create a new dependency generation.
    pub fn set<T: Service>(&self, key: ServiceKey<T>, value: Arc<T>) -> Result<(), CordisError> {
        self.root
            .services
            .replace(&self.slot(key.name()), &self.fiber, value)
    }

    /// Returns whether a dynamically named service has a provider in this scope.
    #[must_use]
    pub fn has_named(&self, name: &str) -> bool {
        self.root
            .services
            .provider_id(&self.slot(name), true)
            .is_some()
    }

    /// Monotonic revision of successful service provision and withdrawal.
    ///
    /// Consumers use this to invalidate reflection-derived caches without
    /// retaining service instances or subscribing to an untyped event.
    #[must_use]
    pub fn service_revision(&self) -> u64 {
        self.root.services.revision()
    }

    /// Snapshots JSON-compatible services visible in this exact isolation
    /// scope for a source-language compatibility boundary.
    ///
    /// Ordinary typed services remain opaque. JSON values, strings, booleans,
    /// and numeric scalars are projected without granting an evaluator access
    /// to Rust trait objects or lifecycle owners.
    #[must_use]
    pub fn expression_service_snapshot(&self) -> BTreeMap<String, Value> {
        self.root
            .services
            .names()
            .into_iter()
            .filter_map(|name| {
                self.root
                    .services
                    .projected_json(&self.slot(&name), true)
                    .map(|value| (name, value))
            })
            .collect()
    }

    pub(crate) fn provider_id(&self, name: &str) -> Option<Uuid> {
        self.root.services.provider_id(&self.slot(name), true)
    }

    /// Mounts a plugin whose lifecycle is owned by this context.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] when this context is inactive.
    pub fn plugin(&self, plugin: Plugin, config: Value) -> Result<Arc<PluginFiber>, CordisError> {
        self.root.plugins.mount(self, plugin, config)
    }

    fn slot(&self, name: &str) -> ServiceSlot {
        ServiceSlot {
            name: name.to_owned(),
            isolation: self.isolation.get(name).map(|realm| realm.id),
        }
    }
}

fn shallow_merge<'a>(values: impl IntoIterator<Item = &'a Value>) -> Value {
    let mut merged = serde_json::Map::new();
    for value in values {
        if let Value::Object(object) = value {
            merged.extend(object.clone());
        }
    }
    Value::Object(merged)
}
