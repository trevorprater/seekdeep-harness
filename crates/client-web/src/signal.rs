//! Reference-stable kernel signals and loader-status projection.

use std::{cell::RefCell, rc::Rc};

use indexmap::IndexMap;

/// Source Cordis fiber-state numeric vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum WebFiberState {
    /// Waiting for injected services.
    Pending = 0,
    /// Applying the plugin.
    Loading = 1,
    /// Fully active.
    Active = 2,
    /// Apply or import failure.
    Failed = 3,
    /// Fully disposed.
    Disposed = 4,
    /// Running teardown.
    Unloading = 5,
}

impl WebFiberState {
    /// Lower-case boot-page projection label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Loading => "loading",
            Self::Active => "active",
            Self::Failed => "failed",
            Self::Disposed => "disposed",
            Self::Unloading => "unloading",
        }
    }
}

type Listener = Rc<dyn Fn()>;

struct SignalState<T> {
    value: Rc<T>,
    listeners: IndexMap<u64, Listener>,
    next_listener: u64,
}

/// Writable kernel-owned observable with stable snapshots between writes.
pub struct KernelSignal<T> {
    state: Rc<RefCell<SignalState<T>>>,
}

impl<T> Clone for KernelSignal<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
        }
    }
}

impl<T> std::fmt::Debug for KernelSignal<T>
where
    T: std::fmt::Debug,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KernelSignal")
            .field("value", &self.state.borrow().value)
            .field("listeners", &self.state.borrow().listeners.len())
            .finish()
    }
}

impl<T> KernelSignal<T> {
    /// Creates a signal at one initial value.
    #[must_use]
    pub fn new(value: T) -> Self {
        Self {
            state: Rc::new(RefCell::new(SignalState {
                value: Rc::new(value),
                listeners: IndexMap::new(),
                next_listener: 0,
            })),
        }
    }

    /// Stable snapshot reference until the next `set`.
    #[must_use]
    pub fn snapshot(&self) -> Rc<T> {
        self.state.borrow().value.clone()
    }

    /// Publishes one value and synchronously wakes a snapshot of listeners.
    pub fn set(&self, value: T) {
        let listeners = {
            let mut state = self.state.borrow_mut();
            state.value = Rc::new(value);
            state.listeners.values().cloned().collect::<Vec<_>>()
        };
        for listener in listeners {
            listener();
        }
    }

    /// Subscribes and returns an idempotent exact-generation disposer.
    #[must_use]
    pub fn subscribe(&self, listener: Listener) -> SignalSubscription<T> {
        let id = {
            let mut state = self.state.borrow_mut();
            state.next_listener = state.next_listener.wrapping_add(1);
            let id = state.next_listener;
            state.listeners.insert(id, listener);
            id
        };
        SignalSubscription {
            signal: self.clone(),
            id,
        }
    }
}

/// Exact-generation signal subscription.
pub struct SignalSubscription<T> {
    signal: KernelSignal<T>,
    id: u64,
}

impl<T> SignalSubscription<T> {
    /// Removes this listener; repeated calls are harmless.
    pub fn dispose(&self) {
        self.signal
            .state
            .borrow_mut()
            .listeners
            .shift_remove(&self.id);
    }
}

/// Per-entry loader status in insertion order.
pub type LoaderStatus = IndexMap<String, WebFiberState>;

/// Copy-on-write loader-status signal.
#[derive(Clone, Debug)]
pub struct LoaderStatusStore(KernelSignal<LoaderStatus>);

impl LoaderStatusStore {
    /// Creates an empty status feed.
    #[must_use]
    pub fn new() -> Self {
        Self(KernelSignal::new(IndexMap::new()))
    }

    /// Observable face.
    #[must_use]
    pub fn signal(&self) -> KernelSignal<LoaderStatus> {
        self.0.clone()
    }

    /// Projects one entry state with a new snapshot identity.
    pub fn set(&self, id: String, state: WebFiberState) {
        let mut next = (*self.0.snapshot()).clone();
        next.insert(id, state);
        self.0.set(next);
    }
}

impl Default for LoaderStatusStore {
    fn default() -> Self {
        Self::new()
    }
}
