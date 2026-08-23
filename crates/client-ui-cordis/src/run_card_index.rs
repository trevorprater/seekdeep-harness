//! Session-local ownership index for Package business views on `cordis_run` cards.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Weak},
};

use parking_lot::Mutex;
use seekdeep_cordis_dynamic_types::{
    CordisDynamicPackageId, CordisDynamicPluginId, CordisDynamicPluginRunId,
};
use seekdeep_identity::SessionId;

/// Stable keyed-slot identity of one Package-owned business view.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CordisToolViewKey(String);

impl CordisToolViewKey {
    /// Wraps an exact keyed-Slot identity received from the browser Store.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the exact shared Slot key.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Builds the Package business-view key shared by registrations and Run cards.
#[must_use]
pub fn cordis_tool_view_key(
    plugin_id: &CordisDynamicPluginId,
    package_id: &CordisDynamicPackageId,
) -> CordisToolViewKey {
    CordisToolViewKey(format!("{plugin_id}.{package_id}"))
}

/// One successful tool result competing to host a Package business view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CordisRunCardPointer {
    /// Stable Plugin-and-Package Slot key.
    pub key: CordisToolViewKey,
    /// Tool call that owns the card.
    pub call_id: String,
    /// Append-only session-log sequence.
    pub seq: u64,
    /// Exact successful activation.
    pub plugin_run_id: CordisDynamicPluginRunId,
}

type Listener = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct StoreState {
    pointers: BTreeMap<CordisToolViewKey, CordisRunCardPointer>,
    snapshot: Option<Arc<BTreeMap<CordisToolViewKey, CordisRunCardPointer>>>,
    listeners: BTreeMap<u64, Listener>,
    next_listener: u64,
}

/// Per-session observable latest-card index.
#[derive(Default)]
pub struct CordisRunCardStore {
    state: Mutex<StoreState>,
}

impl std::fmt::Debug for CordisRunCardStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CordisRunCardStore")
            .field("pointers", &self.state.lock().pointers)
            .finish_non_exhaustive()
    }
}

impl CordisRunCardStore {
    /// Returns a stable snapshot owner until a greater sequence is accepted.
    #[must_use]
    pub fn snapshot(&self) -> Arc<BTreeMap<CordisToolViewKey, CordisRunCardPointer>> {
        let mut state = self.state.lock();
        if let Some(snapshot) = &state.snapshot {
            return snapshot.clone();
        }
        let snapshot = Arc::new(state.pointers.clone());
        state.snapshot = Some(snapshot.clone());
        snapshot
    }

    /// Subscribes to accepted pointer changes.
    #[must_use]
    pub fn subscribe(self: &Arc<Self>, listener: Listener) -> CordisRunCardSubscription {
        let id = {
            let mut state = self.state.lock();
            state.next_listener = state.next_listener.wrapping_add(1);
            let id = state.next_listener;
            state.listeners.insert(id, listener);
            id
        };
        CordisRunCardSubscription {
            store: Arc::downgrade(self),
            id,
        }
    }

    /// Publishes a successful result when its sequence is strictly newer.
    pub fn observe(&self, pointer: CordisRunCardPointer) -> bool {
        let listeners = {
            let mut state = self.state.lock();
            if state
                .pointers
                .get(&pointer.key)
                .is_some_and(|current| current.seq >= pointer.seq)
            {
                return false;
            }
            state.pointers.insert(pointer.key.clone(), pointer);
            state.snapshot = None;
            state.listeners.values().cloned().collect::<Vec<_>>()
        };
        for listener in listeners {
            listener();
        }
        true
    }
}

/// Idempotent run-card listener registration.
pub struct CordisRunCardSubscription {
    store: Weak<CordisRunCardStore>,
    id: u64,
}

impl CordisRunCardSubscription {
    /// Stops future notifications.
    pub fn dispose(&self) {
        if let Some(store) = self.store.upgrade() {
            store.state.lock().listeners.remove(&self.id);
        }
    }
}

/// Page-lifetime registry sharing one Store across all cards in a Session.
#[derive(Debug, Default)]
pub struct CordisRunCardRegistry {
    sessions: Mutex<HashMap<SessionId, Arc<CordisRunCardStore>>>,
}

impl CordisRunCardRegistry {
    /// Returns the persistent page-local Store for `session_id`.
    #[must_use]
    pub fn for_session(&self, session_id: SessionId) -> Arc<CordisRunCardStore> {
        let mut sessions = self.sessions.lock();
        sessions.entry(session_id).or_default().clone()
    }
}
