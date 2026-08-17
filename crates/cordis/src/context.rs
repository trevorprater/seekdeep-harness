//! Scoped dependency container.

use std::{
    collections::HashMap,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
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
}

/// Cloneable context view carrying service isolation and plugin ownership.
#[derive(Clone)]
pub struct Context {
    root: Arc<Root>,
    fiber: Arc<Fiber>,
    isolation: Arc<HashMap<String, Uuid>>,
    intercepts: Arc<HashMap<String, Value>>,
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
        let root = Arc::new(Root {
            events: EventBus::new(),
            services: Arc::new(ServiceStore::default()),
            service_changes: Arc::new(ServiceChangeBus::default()),
            plugins: PluginRegistry::new(),
        });
        Self {
            root,
            fiber: Fiber::root(),
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

    /// Creates a child context owned by `fiber`.
    #[must_use]
    pub fn with_fiber(&self, fiber: Arc<Fiber>) -> Self {
        Self {
            root: self.root.clone(),
            fiber,
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
        let mut isolation = (*self.isolation).clone();
        isolation.insert(name.to_owned(), Uuid::now_v7());
        Self {
            root: self.root.clone(),
            fiber: self.fiber.clone(),
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
        let merged = intercepts
            .remove(name)
            .map_or(config.clone(), |parent| merge_json(parent, config));
        intercepts.insert(name.to_owned(), merged);
        Self {
            root: self.root.clone(),
            fiber: self.fiber.clone(),
            isolation: self.isolation.clone(),
            intercepts: Arc::new(intercepts),
            metadata: self.metadata.clone(),
            filter: self.filter.clone(),
        }
    }

    /// Returns accumulated intercept configuration for a service.
    #[must_use]
    pub fn intercepted(&self, name: &str) -> Option<&Value> {
        self.intercepts.get(name)
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
        self.provide_named(key.name(), value)
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
        let slot = self.slot(name);
        let Some(id) = self.root.services.insert(slot.clone(), &self.fiber, value) else {
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
            isolation: self.isolation.get(name).copied(),
        }
    }
}

fn merge_json(parent: Value, child: Value) -> Value {
    match (parent, child) {
        (Value::Object(mut parent), Value::Object(child)) => {
            for (key, value) in child {
                let value = parent
                    .remove(&key)
                    .map_or(value.clone(), |previous| merge_json(previous, value));
                parent.insert(key, value);
            }
            Value::Object(parent)
        }
        (_, child) => child,
    }
}
