//! Scoped dependency container.

use std::{
    any::Any,
    collections::{BTreeMap, HashMap},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use parking_lot::{Mutex, RwLock};

use serde_json::Value;
use uuid::Uuid;

use crate::{
    events::EventBus,
    fiber::{CordisError, EffectHandle, Fiber},
    logger::{CordisClock, Logger, LoggerService, SystemCordisClock},
    plugin::{Plugin, PluginFiber, PluginRegistry},
    service::{Service, ServiceKey, ServiceProviderSnapshot, ServiceSlot, ServiceStore},
};

type EventFilter = Arc<dyn Fn(&Context) -> bool + Send + Sync>;
type ServiceChangeListener = Arc<dyn Fn() + Send + Sync>;
type ServiceChangeGuard = Arc<dyn Fn(&str) -> anyhow::Result<()> + Send + Sync>;
/// Type-erased value crossing the reflected property boundary.
pub type DynamicValue = Arc<dyn Any + Send + Sync>;
type AccessorGetter = Arc<dyn Fn(&Context) -> anyhow::Result<Option<DynamicValue>> + Send + Sync>;
type AccessorSetter = Arc<dyn Fn(&Context, DynamicValue) -> anyhow::Result<bool> + Send + Sync>;
type MixinGetter<T> = Arc<dyn Fn(&T) -> DynamicValue + Send + Sync>;
type MixinSetter<T> = Arc<dyn Fn(&T, DynamicValue) -> anyhow::Result<bool> + Send + Sync>;

struct AccessorEntry {
    id: Uuid,
    getter: AccessorGetter,
    setter: Option<AccessorSetter>,
}

/// One typed member exposed from a service through a reflected property.
pub struct MixinMember<T: Service> {
    target: String,
    getter: MixinGetter<T>,
    setter: Option<MixinSetter<T>>,
}

/// Reversible group of reflected members installed by [`Context::mixin`].
pub struct MixinHandle {
    effects: Vec<EffectHandle>,
}

impl MixinHandle {
    /// Disposes every member and aggregates failures.
    ///
    /// # Errors
    ///
    /// Returns all member cleanup failures as one error.
    pub async fn dispose(&self) -> anyhow::Result<()> {
        let failures = futures::future::join_all(self.effects.iter().map(EffectHandle::dispose))
            .await
            .into_iter()
            .filter_map(Result::err)
            .map(|error| format!("{error:#}"))
            .collect::<Vec<_>>();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow::anyhow!(failures.join("\n")))
        }
    }
}

impl<T: Service> MixinMember<T> {
    /// Creates a read-only reflected member.
    #[must_use]
    pub fn read_only<R: Any + Send + Sync>(
        target: impl Into<String>,
        getter: impl Fn(&T) -> Arc<R> + Send + Sync + 'static,
    ) -> Self {
        Self {
            target: target.into(),
            getter: Arc::new(move |service| getter(service)),
            setter: None,
        }
    }

    /// Adds a type-erased setter for the reflected member.
    #[must_use]
    pub fn with_setter(
        mut self,
        setter: impl Fn(&T, DynamicValue) -> anyhow::Result<bool> + Send + Sync + 'static,
    ) -> Self {
        self.setter = Some(Arc::new(setter));
        self
    }
}

#[derive(Default)]
struct ServiceChangeBus {
    listeners: Mutex<HashMap<Uuid, ServiceChangeListener>>,
    guards: Mutex<HashMap<Uuid, ServiceChangeGuard>>,
}

impl ServiceChangeBus {
    fn notify(&self) {
        let listeners = self.listeners.lock().values().cloned().collect::<Vec<_>>();
        for listener in listeners {
            let _ = catch_unwind(AssertUnwindSafe(|| listener()));
        }
    }

    fn check(&self, name: &str) -> anyhow::Result<()> {
        let guards = self.guards.lock().values().cloned().collect::<Vec<_>>();
        for guard in guards {
            guard(name)?;
        }
        Ok(())
    }
}

struct Root {
    events: EventBus,
    services: Arc<ServiceStore>,
    service_changes: Arc<ServiceChangeBus>,
    accessors: Arc<RwLock<HashMap<String, AccessorEntry>>>,
    plugins: PluginRegistry,
    logger: Arc<LoggerService>,
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
        Self::new_with_clock(Arc::new(SystemCordisClock))
    }

    /// Creates a root context with an injected wall-clock boundary.
    #[must_use]
    pub fn new_with_clock(clock: Arc<dyn CordisClock>) -> Self {
        let fiber = Fiber::root();
        let root = Arc::new(Root {
            events: EventBus::new(),
            services: Arc::new(ServiceStore::default()),
            service_changes: Arc::new(ServiceChangeBus::default()),
            accessors: Arc::new(RwLock::new(HashMap::new())),
            plugins: PluginRegistry::new(),
            logger: LoggerService::new(clock),
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

    /// Root-owned plugin registry shared by every child context.
    #[must_use]
    pub fn registry(&self) -> &PluginRegistry {
        &self.root.plugins
    }

    /// Built-in structured logging service.
    #[must_use]
    pub fn logger_service(&self) -> &Arc<LoggerService> {
        &self.root.logger
    }

    /// Creates a logger using explicit name or fiber/intercept defaults.
    #[must_use]
    pub fn logger(&self, name: Option<&str>) -> Logger {
        self.root.logger.logger(self, name)
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

    /// Declares a computed reflected property owned by this context's fiber.
    ///
    /// # Errors
    ///
    /// Rejects conflicts with existing service/accessor declarations or an
    /// inactive owner.
    pub fn accessor<T, G, S>(
        &self,
        name: impl Into<String>,
        getter: G,
        setter: Option<S>,
    ) -> Result<EffectHandle, CordisError>
    where
        T: Any + Send + Sync,
        G: Fn(&Context) -> anyhow::Result<Option<Arc<T>>> + Send + Sync + 'static,
        S: Fn(&Context, Arc<T>) -> anyhow::Result<bool> + Send + Sync + 'static,
    {
        let getter = Arc::new(getter);
        let setter = setter.map(Arc::new);
        self.accessor_erased(
            name.into(),
            Arc::new(move |context| {
                getter(context).map(|value| value.map(|value| value as DynamicValue))
            }),
            setter.map(|setter| {
                Arc::new(move |context: &Context, value: DynamicValue| {
                    let value = Arc::downcast::<T>(value)
                        .map_err(|_| anyhow::anyhow!("reflected setter received the wrong type"))?;
                    setter(context, value)
                }) as AccessorSetter
            }),
        )
    }

    /// Declares a read-only computed reflected property.
    ///
    /// # Errors
    ///
    /// Returns the same declaration and ownership failures as [`Self::accessor`].
    pub fn accessor_read_only<T, G>(
        &self,
        name: impl Into<String>,
        getter: G,
    ) -> Result<EffectHandle, CordisError>
    where
        T: Any + Send + Sync,
        G: Fn(&Context) -> anyhow::Result<Option<Arc<T>>> + Send + Sync + 'static,
    {
        let getter = Arc::new(getter);
        self.accessor_erased(
            name.into(),
            Arc::new(move |context| {
                getter(context).map(|value| value.map(|value| value as DynamicValue))
            }),
            None,
        )
    }

    fn accessor_erased(
        &self,
        name: String,
        getter: AccessorGetter,
        setter: Option<AccessorSetter>,
    ) -> Result<EffectHandle, CordisError> {
        if self.root.services.is_declared(&name) {
            return Err(CordisError::PropertyDeclared {
                name,
                kind: "service",
            });
        }
        let id = Uuid::now_v7();
        {
            let mut accessors = self.root.accessors.write();
            if accessors.contains_key(&name) {
                return Err(CordisError::PropertyDeclared {
                    name,
                    kind: "accessor",
                });
            }
            accessors.insert(name.clone(), AccessorEntry { id, getter, setter });
        }
        let accessors = self.root.accessors.clone();
        let disposal_name = name.clone();
        let effect = EffectHandle::synchronous(format!("ctx.accessor({name:?})"), move || {
            let mut accessors = accessors.write();
            if accessors
                .get(&disposal_name)
                .is_some_and(|entry| entry.id == id)
            {
                accessors.remove(&disposal_name);
            }
            Ok(())
        });
        match self.own(effect.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                self.root.accessors.write().remove(&name);
                Err(error)
            }
        }
    }

    /// Reads a reflected accessor, falling back to a dynamic service value.
    ///
    /// # Errors
    ///
    /// Returns getter failures or a type mismatch.
    pub fn property<T: Service>(&self, name: &str) -> anyhow::Result<Option<Arc<T>>> {
        let getter = self
            .root
            .accessors
            .read()
            .get(name)
            .map(|entry| entry.getter.clone());
        let Some(getter) = getter else {
            return Ok(self.get_named(name));
        };
        getter(self)?.map_or(Ok(None), |value| {
            Arc::downcast::<T>(value)
                .map(Some)
                .map_err(|_| anyhow::anyhow!("reflected property {name:?} has the wrong type"))
        })
    }

    /// Writes a reflected accessor or a provider-owned dynamic service.
    ///
    /// # Errors
    ///
    /// Returns setter, type, missing-provider, or owner failures.
    pub fn set_property<T: Service>(&self, name: &str, value: Arc<T>) -> anyhow::Result<bool> {
        let setter = self
            .root
            .accessors
            .read()
            .get(name)
            .and_then(|entry| entry.setter.clone());
        if self.root.accessors.read().contains_key(name) {
            return setter.map_or(Ok(false), |setter| setter(self, value));
        }
        self.root
            .services
            .replace(&self.slot(name), &self.fiber, value)?;
        Ok(true)
    }

    /// Whether a service or accessor property has ever been declared.
    #[must_use]
    pub fn has_property(&self, name: &str) -> bool {
        self.root.services.is_declared(name) || self.root.accessors.read().contains_key(name)
    }

    /// Exposes typed service members as fiber-owned reflected accessors.
    ///
    /// # Errors
    ///
    /// Returns the first declaration or ownership failure.
    pub fn mixin<T: Service>(
        &self,
        source: ServiceKey<T>,
        members: impl IntoIterator<Item = MixinMember<T>>,
    ) -> Result<MixinHandle, CordisError> {
        let mut effects = Vec::new();
        for member in members {
            let getter_context = self.clone();
            let getter = member.getter.clone();
            let setter_context = self.clone();
            let setter = member.setter.clone();
            let result = self.accessor_erased(
                member.target,
                Arc::new(move |_| {
                    Ok(getter_context
                        .get(source)
                        .map(|service| getter(service.as_ref())))
                }),
                setter.map(|setter| {
                    Arc::new(move |_context: &Context, value: DynamicValue| {
                        let Some(service) = setter_context.get(source) else {
                            return Ok(false);
                        };
                        setter(service.as_ref(), value)
                    }) as AccessorSetter
                }),
            );
            match result {
                Ok(effect) => effects.push(effect),
                Err(error) => {
                    for effect in &effects {
                        let _ = futures::executor::block_on(effect.dispose());
                    }
                    return Err(error);
                }
            }
        }
        Ok(MixinHandle { effects })
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

    /// Guards every successful service provision and withdrawal synchronously.
    ///
    /// A provision rejected by a guard is removed before plugin dependency
    /// reconciliation and returned to the provider as
    /// [`CordisError::ServicePublication`]. Withdrawal has already committed;
    /// a guard failure is returned by the disposing effect.
    ///
    /// # Errors
    ///
    /// Returns [`CordisError::InactiveEffect`] after the owning fiber begins disposal.
    pub fn on_service_change_checked(
        &self,
        guard: impl Fn(&str) -> anyhow::Result<()> + Send + Sync + 'static,
    ) -> Result<EffectHandle, CordisError> {
        let id = Uuid::now_v7();
        self.root
            .service_changes
            .guards
            .lock()
            .insert(id, Arc::new(guard));
        let changes = self.root.service_changes.clone();
        let effect = EffectHandle::synchronous("ctx.on_service_change_checked", move || {
            changes.guards.lock().remove(&id);
            Ok(())
        });
        match self.own(effect.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                self.root.service_changes.guards.lock().remove(&id);
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
        if self.root.accessors.read().contains_key(name) {
            return Err(CordisError::PropertyDeclared {
                name: name.to_owned(),
                kind: "accessor",
            });
        }
        let slot = self.slot(name);
        let Some(id) = self
            .root
            .services
            .insert(&slot, &self.fiber, value, expression_projection)
        else {
            return Err(CordisError::DuplicateService(name.to_owned()));
        };
        if let Err(error) = self.root.service_changes.check(name) {
            self.root.services.remove(&slot, id);
            return Err(CordisError::ServicePublication(format!("{error:#}")));
        }
        self.root.services.mark_changed(&slot);
        self.root.plugins.notify_service_change();
        self.root.service_changes.notify();
        let services = self.root.services.clone();
        let plugins = self.root.plugins.clone();
        let service_changes = self.root.service_changes.clone();
        let disposal_slot = slot.clone();
        let effect = EffectHandle::synchronous(format!("ctx.provide({name:?})"), move || {
            if services.remove(&disposal_slot, id) {
                services.mark_changed(&disposal_slot);
                plugins.notify_service_change();
                service_changes.notify();
                service_changes.check(&disposal_slot.name)?;
            }
            Ok(())
        });
        match self.own(effect.clone()) {
            Ok(effect) => Ok(effect),
            Err(error) => {
                if self.root.services.remove(&slot, id) {
                    self.root.services.mark_changed(&slot);
                }
                self.root.plugins.notify_service_change();
                self.root.service_changes.notify();
                let _ = self.root.service_changes.check(name);
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
        self.get_named_relaxed(key.name())
    }

    /// Resolves a runtime-named provider even while its owning plugin is loading.
    #[must_use]
    pub fn get_named_relaxed<T: Service>(&self, name: &str) -> Option<Arc<T>> {
        self.root.services.get(&self.slot(name), false)
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

    /// Monotonic revision of successful provision and withdrawal for one
    /// service slot in this context's isolation scope.
    ///
    /// Unlike [`Self::service_revision`], unrelated service changes do not
    /// advance this value. Consumers can therefore fence observations across
    /// an optional dependency's mount, replacement generation, and unmount
    /// without retaining the dependency itself.
    #[must_use]
    pub fn service_slot_revision<T: Service>(&self, key: ServiceKey<T>) -> u64 {
        self.root.services.slot_revision(&self.slot(key.name()))
    }

    /// Snapshots all registered service implementations across isolation scopes.
    #[must_use]
    pub fn service_providers(&self) -> Vec<ServiceProviderSnapshot> {
        self.root.services.snapshots()
    }

    /// Resolves one typed service provided by an exact fiber subtree,
    /// independently of its isolation realm.
    ///
    /// This is a privileged ownership lookup for Host code that already holds
    /// the subject whose private composition is being addressed.
    #[must_use]
    pub fn service_from_fiber<T: Service>(
        &self,
        key: ServiceKey<T>,
        fiber: &Arc<Fiber>,
    ) -> Option<Arc<T>> {
        self.root.services.value_from_fiber(key.name(), fiber)
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
