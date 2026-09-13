//! Portable per-session composer-block registry.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::{Rc, Weak},
};

use seekdeep_identity::SessionId;

/// Why one session's composer is inert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposerBlock {
    /// Localized placeholder owned by the blocking plugin.
    pub reason: String,
}

type Listener = Rc<dyn Fn()>;

struct BlockStoreState {
    value: Option<ComposerBlock>,
    listeners: BTreeMap<u64, Listener>,
    next_listener: u64,
}

/// Synchronously observable whole-value store for one session's block.
pub struct ComposerBlockStore {
    state: RefCell<BlockStoreState>,
}

impl ComposerBlockStore {
    fn new() -> Rc<Self> {
        Rc::new(Self {
            state: RefCell::new(BlockStoreState {
                value: None,
                listeners: BTreeMap::new(),
                next_listener: 0,
            }),
        })
    }

    /// Returns the current whole block.
    #[must_use]
    pub fn snapshot(&self) -> Option<ComposerBlock> {
        self.state.borrow().value.clone()
    }

    /// Replaces the complete value and notifies every subscriber synchronously.
    pub fn set(&self, value: Option<ComposerBlock>) {
        let listeners = {
            let mut state = self.state.borrow_mut();
            state.value = value;
            state.listeners.values().cloned().collect::<Vec<_>>()
        };
        for listener in listeners {
            listener();
        }
    }

    /// Mutates a cloned whole value and commits it as one synchronous replacement.
    pub fn update(&self, mutator: impl FnOnce(&mut Option<ComposerBlock>)) {
        let mut next = self.snapshot();
        mutator(&mut next);
        self.set(next);
    }

    /// Subscribes to whole-value replacements.
    #[must_use]
    pub fn subscribe(self: &Rc<Self>, listener: Listener) -> ComposerBlockSubscription {
        let id = {
            let mut state = self.state.borrow_mut();
            state.next_listener = state.next_listener.wrapping_add(1);
            let id = state.next_listener;
            state.listeners.insert(id, listener);
            id
        };
        ComposerBlockSubscription {
            store: Rc::downgrade(self),
            id,
        }
    }
}

/// Subscription removed on drop.
pub struct ComposerBlockSubscription {
    store: Weak<ComposerBlockStore>,
    id: u64,
}

impl Drop for ComposerBlockSubscription {
    fn drop(&mut self) {
        if let Some(store) = self.store.upgrade() {
            store.state.borrow_mut().listeners.remove(&self.id);
        }
    }
}

/// One plugin-fiber composer-block registry.
#[derive(Default)]
pub struct ComposerBlockRegistry {
    stores: RefCell<BTreeMap<SessionId, Rc<ComposerBlockStore>>>,
}

impl ComposerBlockRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Raises or clears one session's block, notifying only when the reason changes.
    pub fn set(&self, session_id: SessionId, block: Option<ComposerBlock>) {
        let store = self.store_for(session_id);
        let current = store.snapshot();
        if current.as_ref().map(|value| &value.reason) == block.as_ref().map(|value| &value.reason)
        {
            return;
        }
        store.set(block);
    }

    /// Returns the identity-stable store for one session, creating it on first access.
    #[must_use]
    pub fn store_for(&self, session_id: SessionId) -> Rc<ComposerBlockStore> {
        if let Some(existing) = self.stores.borrow().get(&session_id) {
            return existing.clone();
        }
        let created = ComposerBlockStore::new();
        self.stores.borrow_mut().insert(session_id, created.clone());
        created
    }

    /// Drops one session's registry-owned store handle.
    pub fn forget(&self, session_id: &SessionId) {
        self.stores.borrow_mut().remove(session_id);
    }

    /// Returns the number of live registry entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.stores.borrow().len()
    }

    /// Returns whether no per-session stores exist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stores.borrow().is_empty()
    }
}
