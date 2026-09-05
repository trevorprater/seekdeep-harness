//! Generation-safe, single-flight Host inventory state.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Weak},
};

use parking_lot::Mutex;
use seekdeep_cordis_dynamic_types::{CordisDynamicPluginId, DynamicCordisInventoryRow};

/// What the panel last learned from the Host definition registry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CordisInventorySnapshot {
    /// Last successfully read Plugin rows.
    pub rows: Vec<DynamicCordisInventoryRow>,
    /// Plugins explicitly removed or observed disappearing, retained for historical cards.
    pub removed: BTreeSet<CordisDynamicPluginId>,
    /// Whether a first successful read has settled.
    pub read: bool,
    /// Last current-generation read failure.
    pub error: Option<String>,
}

/// Exact ownership token for one Host inventory request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InventoryReadTicket {
    generation: u64,
    request: u64,
}

type Listener = Arc<dyn Fn() + Send + Sync>;

struct InventoryState {
    snapshot: Arc<CordisInventorySnapshot>,
    generation: u64,
    next_request: u64,
    in_flight: Option<InventoryReadTicket>,
    next_listener: u64,
    listeners: BTreeMap<u64, Listener>,
}

impl Default for InventoryState {
    fn default() -> Self {
        Self {
            snapshot: Arc::new(CordisInventorySnapshot::default()),
            generation: 0,
            next_request: 0,
            in_flight: None,
            next_listener: 0,
            listeners: BTreeMap::new(),
        }
    }
}

/// Page-lifetime inventory observable.
#[derive(Default)]
pub struct CordisInventory {
    state: Mutex<InventoryState>,
}

impl std::fmt::Debug for CordisInventory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock();
        formatter
            .debug_struct("CordisInventory")
            .field("generation", &state.generation)
            .field("in_flight", &state.in_flight)
            .field("snapshot", &state.snapshot)
            .finish_non_exhaustive()
    }
}

impl CordisInventory {
    /// Creates an unread inventory.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Returns a stable snapshot owner until the next publication.
    #[must_use]
    pub fn snapshot(&self) -> Arc<CordisInventorySnapshot> {
        self.state.lock().snapshot.clone()
    }

    /// Subscribes to every committed snapshot replacement.
    #[must_use]
    pub fn subscribe(self: &Arc<Self>, listener: Listener) -> CordisInventorySubscription {
        let id = {
            let mut state = self.state.lock();
            state.next_listener = state.next_listener.wrapping_add(1);
            let id = state.next_listener;
            state.listeners.insert(id, listener);
            id
        };
        CordisInventorySubscription {
            inventory: Arc::downgrade(self),
            id,
        }
    }

    /// Claims the current generation's read slot.
    ///
    /// Returns `None` while another read from the same connection is active.
    #[must_use]
    pub fn begin_refresh(&self) -> Option<InventoryReadTicket> {
        let mut state = self.state.lock();
        if state.in_flight.is_some() {
            return None;
        }
        state.next_request = state.next_request.wrapping_add(1);
        let ticket = InventoryReadTicket {
            generation: state.generation,
            request: state.next_request,
        };
        state.in_flight = Some(ticket);
        Some(ticket)
    }

    /// Publishes a successful read only when `ticket` still owns the current slot.
    ///
    /// Returns false for stale answers from a prior connection or superseded request.
    pub fn resolve(
        &self,
        ticket: InventoryReadTicket,
        rows: Vec<DynamicCordisInventoryRow>,
    ) -> bool {
        let listeners = {
            let mut state = self.state.lock();
            if state.in_flight != Some(ticket) || state.generation != ticket.generation {
                return false;
            }
            let prior = state.snapshot.clone();
            let live = rows
                .iter()
                .map(|row| row.plugin_id.clone())
                .collect::<BTreeSet<_>>();
            let mut removed = prior.removed.clone();
            for row in &prior.rows {
                if !live.contains(&row.plugin_id) {
                    removed.insert(row.plugin_id.clone());
                }
            }
            state.in_flight = None;
            state.snapshot = Arc::new(CordisInventorySnapshot {
                rows,
                removed,
                read: true,
                error: None,
            });
            state.listeners.values().cloned().collect::<Vec<_>>()
        };
        notify(listeners);
        true
    }

    /// Publishes a current-generation read failure without discarding known rows.
    ///
    /// `message` is `None` when JavaScript rejected with a non-Error value.
    pub fn reject(&self, ticket: InventoryReadTicket, message: Option<String>) -> bool {
        let listeners = {
            let mut state = self.state.lock();
            if state.in_flight != Some(ticket) || state.generation != ticket.generation {
                return false;
            }
            let prior = state.snapshot.clone();
            state.in_flight = None;
            state.snapshot = Arc::new(CordisInventorySnapshot {
                rows: prior.rows.clone(),
                removed: prior.removed.clone(),
                read: prior.read,
                error: Some(
                    message.unwrap_or_else(|| "reading the cordis inventory failed".to_owned()),
                ),
            });
            state.listeners.values().cloned().collect::<Vec<_>>()
        };
        notify(listeners);
        true
    }

    /// Records an explicit removal and drops its live row immediately.
    pub fn retire(&self, plugin_id: &CordisDynamicPluginId) {
        let listeners = {
            let mut state = self.state.lock();
            let prior = state.snapshot.clone();
            let mut removed = prior.removed.clone();
            removed.insert(plugin_id.clone());
            state.snapshot = Arc::new(CordisInventorySnapshot {
                rows: prior
                    .rows
                    .iter()
                    .filter(|row| row.plugin_id != *plugin_id)
                    .cloned()
                    .collect(),
                removed,
                read: prior.read,
                error: prior.error.clone(),
            });
            state.listeners.values().cloned().collect::<Vec<_>>()
        };
        notify(listeners);
    }

    /// Invalidates the prior connection and frees its read slot immediately.
    ///
    /// Explicit removal history remains so replayed cards stay terminal.
    pub fn reset(&self) {
        let listeners = {
            let mut state = self.state.lock();
            state.generation = state.generation.wrapping_add(1);
            state.in_flight = None;
            let removed = state.snapshot.removed.clone();
            state.snapshot = Arc::new(CordisInventorySnapshot {
                rows: Vec::new(),
                removed,
                read: false,
                error: None,
            });
            state.listeners.values().cloned().collect::<Vec<_>>()
        };
        notify(listeners);
    }
}

/// Idempotent inventory-listener registration.
pub struct CordisInventorySubscription {
    inventory: Weak<CordisInventory>,
    id: u64,
}

impl CordisInventorySubscription {
    /// Stops future notifications.
    pub fn dispose(&self) {
        if let Some(inventory) = self.inventory.upgrade() {
            inventory.state.lock().listeners.remove(&self.id);
        }
    }
}

fn notify(listeners: Vec<Listener>) {
    for listener in listeners {
        listener();
    }
}
