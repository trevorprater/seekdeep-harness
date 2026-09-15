//! Independently owned Conversation Event and View Definition registries.

use std::{cell::RefCell, collections::BTreeMap, fmt, rc::Rc};

use crate::RuntimeDisposer;

type Listener = Rc<dyn Fn()>;

struct DefinitionState<D> {
    definitions: BTreeMap<String, Rc<D>>,
    order: Vec<String>,
    cached: Rc<Vec<Rc<D>>>,
    listeners: BTreeMap<u64, Listener>,
    next_listener: u64,
}

struct DefinitionRegistry<D> {
    state: RefCell<DefinitionState<D>>,
}

impl<D> DefinitionRegistry<D>
where
    D: 'static,
{
    fn new() -> Rc<Self> {
        Rc::new(Self {
            state: RefCell::new(DefinitionState {
                definitions: BTreeMap::new(),
                order: Vec::new(),
                cached: Rc::new(Vec::new()),
                listeners: BTreeMap::new(),
                next_listener: 0,
            }),
        })
    }

    fn entries(&self) -> Rc<Vec<Rc<D>>> {
        self.state.borrow().cached.clone()
    }

    fn subscribe(self: &Rc<Self>, listener: Listener) -> RuntimeDisposer {
        let id = {
            let mut state = self.state.borrow_mut();
            state.next_listener = state.next_listener.wrapping_add(1);
            let id = state.next_listener;
            state.listeners.insert(id, listener);
            id
        };
        let weak = Rc::downgrade(self);
        RuntimeDisposer::new(move || {
            if let Some(registry) = weak.upgrade() {
                registry.state.borrow_mut().listeners.remove(&id);
            }
        })
    }

    fn register(
        self: &Rc<Self>,
        key: String,
        definition: Rc<D>,
        duplicate_message: String,
    ) -> Result<RuntimeDisposer, ConversationRegistryError> {
        if self.state.borrow().definitions.contains_key(&key) {
            return Err(ConversationRegistryError(duplicate_message));
        }
        {
            let mut state = self.state.borrow_mut();
            state.order.push(key.clone());
            state.definitions.insert(key.clone(), definition.clone());
        }
        self.refresh();
        let weak = Rc::downgrade(self);
        Ok(RuntimeDisposer::new(move || {
            let Some(registry) = weak.upgrade() else {
                return;
            };
            let removed = {
                let mut state = registry.state.borrow_mut();
                if state
                    .definitions
                    .get(&key)
                    .is_none_or(|current| !Rc::ptr_eq(current, &definition))
                {
                    false
                } else {
                    state.definitions.remove(&key);
                    state.order.retain(|candidate| candidate != &key);
                    true
                }
            };
            if removed {
                registry.refresh();
            }
        }))
    }

    fn refresh(&self) {
        let listeners = {
            let mut state = self.state.borrow_mut();
            state.cached = Rc::new(
                state
                    .order
                    .iter()
                    .filter_map(|key| state.definitions.get(key).cloned())
                    .collect(),
            );
            state.listeners.values().cloned().collect::<Vec<_>>()
        };
        for listener in listeners {
            listener();
        }
    }
}

/// Conversation Definition registration failure.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConversationRegistryError(String);

/// One independently owned Conversation business Definition.
#[derive(Clone, Debug)]
pub struct ConversationNodeDefinition<D> {
    /// Registry-local unique kind.
    pub kind: String,
    /// Snapshot target when this Definition produces a view.
    pub target: Option<String>,
    /// Whether a matching view-node builder is present.
    pub has_view_builder: bool,
    /// Host-specific Definition behavior.
    pub payload: D,
}

/// Runtime registry of Conversation business Definitions and one fallback.
pub struct ConversationEventRegistry<D> {
    definitions: Rc<DefinitionRegistry<ConversationNodeDefinition<D>>>,
    fallback: RefCell<Option<Rc<ConversationNodeDefinition<D>>>>,
}

impl<D> fmt::Debug for ConversationEventRegistry<D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConversationEventRegistry")
            .field(
                "definitions",
                &self.definitions.state.borrow().definitions.len(),
            )
            .field("fallback", &self.fallback.borrow().is_some())
            .finish()
    }
}

impl<D> ConversationEventRegistry<D>
where
    D: 'static,
{
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            definitions: DefinitionRegistry::new(),
            fallback: RefCell::new(None),
        })
    }

    /// Reference-stable Definitions in registration order.
    #[must_use]
    pub fn entries(&self) -> Rc<Vec<Rc<ConversationNodeDefinition<D>>>> {
        self.definitions.entries()
    }

    /// Synchronously observes registry changes.
    #[must_use]
    pub fn subscribe(self: &Rc<Self>, listener: Listener) -> RuntimeDisposer {
        self.definitions.subscribe(listener)
    }

    /// Registers one uniquely kinded Definition.
    ///
    /// # Errors
    ///
    /// Rejects target/builder mismatch and duplicate kinds.
    pub fn register(
        self: &Rc<Self>,
        definition: ConversationNodeDefinition<D>,
    ) -> Result<RuntimeDisposer, ConversationRegistryError> {
        assert_target(&definition)?;
        let definition = Rc::new(definition);
        self.definitions.register(
            definition.kind.clone(),
            definition.clone(),
            format!(
                "conversation Definition \"{}\" is already registered",
                definition.kind
            ),
        )
    }

    /// Registers the sole unmatched-event fallback.
    ///
    /// # Errors
    ///
    /// Rejects target/builder mismatch, missing target, and duplicate fallback.
    pub fn register_fallback(
        self: &Rc<Self>,
        definition: ConversationNodeDefinition<D>,
    ) -> Result<RuntimeDisposer, ConversationRegistryError> {
        assert_target(&definition)?;
        if definition.target.is_none() {
            return Err(ConversationRegistryError(
                "conversation fallback Definition must declare a target".to_owned(),
            ));
        }
        if self.fallback.borrow().is_some() {
            return Err(ConversationRegistryError(
                "conversation fallback Definition is already registered".to_owned(),
            ));
        }
        let definition = Rc::new(definition);
        *self.fallback.borrow_mut() = Some(definition.clone());
        self.definitions.refresh();
        let weak = Rc::downgrade(self);
        Ok(RuntimeDisposer::new(move || {
            let Some(registry) = weak.upgrade() else {
                return;
            };
            let remove = registry
                .fallback
                .borrow()
                .as_ref()
                .is_some_and(|current| Rc::ptr_eq(current, &definition));
            if remove {
                *registry.fallback.borrow_mut() = None;
                registry.definitions.refresh();
            }
        }))
    }

    /// Current unmatched-event fallback.
    #[must_use]
    pub fn fallback(&self) -> Option<Rc<ConversationNodeDefinition<D>>> {
        self.fallback.borrow().clone()
    }
}

/// One per-target Conversation snapshot builder.
#[derive(Clone, Debug)]
pub struct ConversationViewDefinition<D> {
    /// Unique target name.
    pub target: String,
    /// Host-specific builder behavior.
    pub payload: D,
}

/// Runtime registry of per-target snapshot builders.
pub struct ConversationViewRegistry<D> {
    definitions: Rc<DefinitionRegistry<ConversationViewDefinition<D>>>,
}

impl<D> fmt::Debug for ConversationViewRegistry<D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConversationViewRegistry")
            .field(
                "definitions",
                &self.definitions.state.borrow().definitions.len(),
            )
            .finish()
    }
}

impl<D> ConversationViewRegistry<D>
where
    D: 'static,
{
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Rc<Self> {
        Rc::new(Self {
            definitions: DefinitionRegistry::new(),
        })
    }

    /// Reference-stable view Definitions in registration order.
    #[must_use]
    pub fn entries(&self) -> Rc<Vec<Rc<ConversationViewDefinition<D>>>> {
        self.definitions.entries()
    }

    /// Synchronously observes view registry changes.
    #[must_use]
    pub fn subscribe(self: &Rc<Self>, listener: Listener) -> RuntimeDisposer {
        self.definitions.subscribe(listener)
    }

    /// Registers one uniquely targeted view builder.
    ///
    /// # Errors
    ///
    /// Rejects duplicate targets.
    pub fn register(
        self: &Rc<Self>,
        definition: ConversationViewDefinition<D>,
    ) -> Result<RuntimeDisposer, ConversationRegistryError> {
        let definition = Rc::new(definition);
        self.definitions.register(
            definition.target.clone(),
            definition.clone(),
            format!(
                "conversation view target \"{}\" is already registered",
                definition.target
            ),
        )
    }
}

fn assert_target<D>(
    definition: &ConversationNodeDefinition<D>,
) -> Result<(), ConversationRegistryError> {
    if definition.target.is_some() != definition.has_view_builder {
        return Err(ConversationRegistryError(format!(
            "conversation Definition \"{}\" must declare target and buildViewNode together",
            definition.kind
        )));
    }
    Ok(())
}

/// Injected microtask scheduler for coalesced resident-Session rebuilds.
pub trait ConversationRegistryScheduler {
    /// Queues one rebuild flush.
    fn queue(&self, callback: Box<dyn FnOnce()>);
}

/// Coalesces Event and View registry changes into one resident rebuild per microtask.
pub struct ConversationRegistryCoordinator {
    scheduled: Rc<RefCell<bool>>,
    subscriptions: Vec<RuntimeDisposer>,
}

impl fmt::Debug for ConversationRegistryCoordinator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConversationRegistryCoordinator")
            .field("scheduled", &self.scheduled.borrow())
            .finish_non_exhaustive()
    }
}

impl ConversationRegistryCoordinator {
    /// Subscribes to both registries and batches the supplied rebuild callback.
    #[must_use]
    pub fn new<E: 'static, V: 'static>(
        events: &Rc<ConversationEventRegistry<E>>,
        views: &Rc<ConversationViewRegistry<V>>,
        scheduler: &Rc<dyn ConversationRegistryScheduler>,
        rebuild: &Rc<dyn Fn()>,
    ) -> Self {
        let pending = Rc::new(RefCell::new(false));
        let notify = {
            let pending = pending.clone();
            let scheduler = scheduler.clone();
            let rebuild = rebuild.clone();
            Rc::new(move || {
                if *pending.borrow() {
                    return;
                }
                *pending.borrow_mut() = true;
                let pending = pending.clone();
                let rebuild = rebuild.clone();
                scheduler.queue(Box::new(move || {
                    *pending.borrow_mut() = false;
                    rebuild();
                }));
            }) as Listener
        };
        Self {
            scheduled: pending,
            subscriptions: vec![events.subscribe(notify.clone()), views.subscribe(notify)],
        }
    }
}

impl Drop for ConversationRegistryCoordinator {
    fn drop(&mut self) {
        for subscription in &self.subscriptions {
            subscription.dispose();
        }
    }
}
