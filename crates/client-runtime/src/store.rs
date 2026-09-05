//! Framework-neutral immutable snapshot Store engine and declarative handles.

use std::{
    any::Any,
    cell::RefCell,
    collections::BTreeMap,
    fmt,
    rc::{Rc, Weak},
};

use seekdeep_client_ui_slots::{SlotStoreFactory, SlotStoreInstance};
use serde::Serialize;
use serde_json::Value;

/// Subscriber flush policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StoreFlushMode {
    /// Notify within the update call; required for controlled-input echoes.
    #[default]
    Sync,
    /// Coalesce updates into one injected animation-frame or microtask flush.
    Frame,
}

/// Injected frame or microtask scheduler for deterministic Store tests.
pub trait StoreFlushScheduler {
    /// Queues one future notification flush.
    fn queue(&self, callback: Box<dyn FnOnce()>);
}

/// Whole-value persistence adapter.
pub trait StorePersistence<T> {
    /// Reads the prior complete value.
    ///
    /// # Errors
    ///
    /// Returns the storage adapter's non-fatal read diagnostic.
    fn read(&self) -> Result<Option<T>, String>;
    /// Writes the complete committed value.
    ///
    /// # Errors
    ///
    /// Returns the storage adapter's non-fatal write diagnostic.
    fn write(&self, value: &T) -> Result<(), String>;
    /// Removes the complete persisted value.
    ///
    /// # Errors
    ///
    /// Returns the storage adapter's non-fatal cleanup diagnostic.
    fn remove(&self) -> Result<(), String>;
}

/// Non-fatal persistence diagnostic sink.
pub type StoreLogger = Rc<dyn Fn(String)>;
type StoreListener = Rc<dyn Fn()>;

struct SnapshotState<T> {
    value: Rc<T>,
    listeners: BTreeMap<u64, StoreListener>,
    next_listener: u64,
    flush_pending: bool,
}

/// Bare immutable observable Store with draft-style update and wholesale set operations.
pub struct SnapshotStore<T> {
    state: RefCell<SnapshotState<T>>,
    mode: StoreFlushMode,
    scheduler: Rc<dyn StoreFlushScheduler>,
    persistence: Option<Rc<dyn StorePersistence<T>>>,
    persistence_name: Option<String>,
    logger: StoreLogger,
}

impl<T> fmt::Debug for SnapshotStore<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.borrow();
        formatter
            .debug_struct("SnapshotStore")
            .field("mode", &self.mode)
            .field("listeners", &state.listeners.len())
            .field("flush_pending", &state.flush_pending)
            .field("persistence_name", &self.persistence_name)
            .finish_non_exhaustive()
    }
}

impl<T> SnapshotStore<T>
where
    T: Clone + 'static,
{
    /// Creates a Store and rehydrates one complete persisted value when available.
    #[must_use]
    pub fn new(
        initial: T,
        mode: StoreFlushMode,
        scheduler: Rc<dyn StoreFlushScheduler>,
        persistence: Option<(String, Rc<dyn StorePersistence<T>>)>,
        logger: StoreLogger,
    ) -> Rc<Self> {
        let (persistence_name, persistence) = persistence
            .map_or((None, None), |(name, persistence)| {
                (Some(name), Some(persistence))
            });
        let value =
            persistence
                .as_ref()
                .map_or(initial.clone(), |persistence| match persistence.read() {
                    Ok(Some(value)) => value,
                    Ok(None) => initial.clone(),
                    Err(error) => {
                        logger(format!(
                            "snapshot store '{}' rehydration failed: {error}",
                            persistence_name.as_deref().unwrap_or_default()
                        ));
                        initial.clone()
                    }
                });
        Rc::new(Self {
            state: RefCell::new(SnapshotState {
                value: Rc::new(value),
                listeners: BTreeMap::new(),
                next_listener: 0,
                flush_pending: false,
            }),
            mode,
            scheduler,
            persistence,
            persistence_name,
            logger,
        })
    }

    /// Returns a reference-stable snapshot until the next state replacement.
    #[must_use]
    pub fn snapshot(&self) -> Rc<T> {
        self.state.borrow().value.clone()
    }

    /// Subscribes to committed state changes.
    #[must_use]
    pub fn subscribe(self: &Rc<Self>, listener: StoreListener) -> SnapshotStoreSubscription<T> {
        let id = {
            let mut state = self.state.borrow_mut();
            state.next_listener = state.next_listener.wrapping_add(1);
            let id = state.next_listener;
            state.listeners.insert(id, listener);
            id
        };
        SnapshotStoreSubscription {
            store: Rc::downgrade(self),
            id,
        }
    }

    /// Clones the current value, applies one draft mutator, and commits the replacement.
    pub fn update(self: &Rc<Self>, mutator: impl FnOnce(&mut T)) {
        let mut next = self.snapshot().as_ref().clone();
        mutator(&mut next);
        self.set(next);
    }

    /// Replaces the complete value.
    pub fn set(self: &Rc<Self>, next: T) {
        self.state.borrow_mut().value = Rc::new(next);
        self.persist();
        match self.mode {
            StoreFlushMode::Sync => self.notify(),
            StoreFlushMode::Frame => self.schedule_flush(),
        }
    }

    /// Removes persisted state, swallowing storage failures.
    pub fn clear_persisted(&self) {
        let Some(persistence) = &self.persistence else {
            return;
        };
        if let Err(error) = persistence.remove() {
            (self.logger)(format!(
                "snapshot store '{}' persistence cleanup failed: {error}",
                self.persistence_name.as_deref().unwrap_or_default()
            ));
        }
    }

    fn persist(&self) {
        let Some(persistence) = &self.persistence else {
            return;
        };
        if let Err(error) = persistence.write(self.snapshot().as_ref()) {
            (self.logger)(format!(
                "snapshot store '{}' persistence failed: {error}",
                self.persistence_name.as_deref().unwrap_or_default()
            ));
        }
    }

    fn schedule_flush(self: &Rc<Self>) {
        {
            let mut state = self.state.borrow_mut();
            if state.flush_pending {
                return;
            }
            state.flush_pending = true;
        }
        let weak = Rc::downgrade(self);
        self.scheduler.queue(Box::new(move || {
            if let Some(store) = weak.upgrade() {
                store.state.borrow_mut().flush_pending = false;
                store.notify();
            }
        }));
    }

    fn notify(&self) {
        let listeners = self
            .state
            .borrow()
            .listeners
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for listener in listeners {
            listener();
        }
    }
}

/// Idempotent Store listener registration.
pub struct SnapshotStoreSubscription<T> {
    store: Weak<SnapshotStore<T>>,
    id: u64,
}

impl<T> SnapshotStoreSubscription<T> {
    /// Stops future notifications.
    pub fn dispose(&self) {
        if let Some(store) = self.store.upgrade() {
            store.state.borrow_mut().listeners.remove(&self.id);
        }
    }
}

/// One named draft mutation in a declarative Store handle.
pub type StoreAction<T> = Rc<dyn Fn(&mut T, &[Value]) -> Result<(), String>>;

/// Declarative Store specification.
pub struct StoreDeclaration<T> {
    /// Fresh initial state per instance.
    pub init: Rc<dyn Fn() -> T>,
    /// Optional base persistence key.
    pub persist: Option<String>,
    /// Complete named write set.
    pub actions: BTreeMap<String, StoreAction<T>>,
}

/// Persistence adapter factory addressed by the resolved root or Session key.
pub type StorePersistenceFactory<T> = Rc<dyn Fn(&str) -> Rc<dyn StorePersistence<T>>>;

/// Environment needed to create Store instances.
pub struct StoreEnvironment<T> {
    /// Frame/microtask scheduler.
    pub scheduler: Rc<dyn StoreFlushScheduler>,
    /// Optional persistence adapter factory addressed by resolved key.
    pub persistence: Option<StorePersistenceFactory<T>>,
    /// Non-fatal diagnostic sink.
    pub logger: StoreLogger,
}

/// Engine-backed declarative Store handle.
pub struct EngineStoreHandle<T> {
    declaration: StoreDeclaration<T>,
    environment: StoreEnvironment<T>,
}

impl<T> EngineStoreHandle<T>
where
    T: Clone + Serialize + 'static,
{
    /// Declares a reusable Store handle; each `create` call remains independent.
    #[must_use]
    pub fn new(declaration: StoreDeclaration<T>, environment: StoreEnvironment<T>) -> Rc<Self> {
        Rc::new(Self {
            declaration,
            environment,
        })
    }

    /// Creates one typed engine instance.
    #[must_use]
    pub fn create_typed(&self, scope_key: Option<&str>) -> Rc<EngineStoreInstance<T>> {
        let persistence_name =
            self.declaration.persist.as_ref().map(|base| {
                scope_key.map_or_else(|| base.clone(), |scope| format!("{base}.{scope}"))
            });
        let persistence = persistence_name.as_ref().and_then(|name| {
            self.environment
                .persistence
                .as_ref()
                .map(|factory| (name.clone(), factory(name)))
        });
        let store = SnapshotStore::new(
            (self.declaration.init)(),
            StoreFlushMode::Sync,
            self.environment.scheduler.clone(),
            persistence,
            self.environment.logger.clone(),
        );
        Rc::new(EngineStoreInstance {
            store,
            actions: self.declaration.actions.clone(),
        })
    }
}

impl<T> SlotStoreFactory for EngineStoreHandle<T>
where
    T: Clone + Serialize + 'static,
{
    fn create(&self, scope_key: Option<&str>) -> Rc<dyn SlotStoreInstance> {
        self.create_typed(scope_key)
    }
}

/// Live declarative Store instance.
pub struct EngineStoreInstance<T> {
    /// Bare snapshot Store.
    pub store: Rc<SnapshotStore<T>>,
    actions: BTreeMap<String, StoreAction<T>>,
}

impl<T> EngineStoreInstance<T>
where
    T: Clone + Serialize + 'static,
{
    /// Invokes one declared draft-stripped action.
    ///
    /// # Errors
    ///
    /// Rejects unknown actions and action-specific argument failures.
    pub fn invoke(self: &Rc<Self>, action: &str, arguments: &[Value]) -> Result<(), String> {
        let action = self
            .actions
            .get(action)
            .cloned()
            .ok_or_else(|| format!("unknown Store action {action:?}"))?;
        let mut next = self.store.snapshot().as_ref().clone();
        action(&mut next, arguments)?;
        self.store.set(next);
        Ok(())
    }
}

impl<T> SlotStoreInstance for EngineStoreInstance<T>
where
    T: Clone + Serialize + 'static,
{
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn snapshot(&self) -> Value {
        serde_json::to_value(self.store.snapshot().as_ref()).unwrap_or(Value::Null)
    }

    fn subscribe(&self, listener: Rc<dyn Fn()>) -> Box<dyn Fn()> {
        let subscription = self.store.subscribe(listener);
        Box::new(move || subscription.dispose())
    }

    fn clear_persisted(&self) {
        self.store.clear_persisted();
    }
}
