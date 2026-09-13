//! Target-portable roster and default-settings store semantics.

use std::rc::Rc;

use futures::future::LocalBoxFuture;
use seekdeep_client_runtime::{SnapshotStore, StoreFlushMode, StoreFlushScheduler, StoreLogger};
use serde::{Deserialize, Serialize};

use crate::{AgentPresetOption, RosterPreset, preset_options};

/// Complete Host roster and capability response.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RosterValue {
    /// Every preset in Host order.
    pub presets: Vec<RosterPreset>,
    /// Whether a writable preset root exists.
    #[serde(default)]
    pub authorable: bool,
    /// Whether the Host can open preset directories.
    #[serde(default)]
    pub has_document: bool,
}

/// Transport used by the default-settings row.
pub trait AgentPresetSettingsTransport {
    /// Reads the current roster.
    fn list(&self) -> LocalBoxFuture<'static, Result<RosterValue, String>>;
    /// Reads whether this browser may write Settings.
    fn describe_settings(&self) -> LocalBoxFuture<'static, Result<bool, String>>;
    /// Writes only `agent-presets.default`.
    fn update_default(&self, id: String) -> LocalBoxFuture<'static, Result<(), String>>;
}

/// Shared roster-backed controller status vocabulary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentPresetStoreStatus {
    /// No read has started.
    #[default]
    Idle,
    /// A roster read is in flight.
    Loading,
    /// A usable roster is available.
    Ready,
    /// A default write is in flight.
    Saving,
    /// The valid deployment has no presets.
    Unavailable,
    /// The latest required read failed.
    Error,
}

/// Default-settings row snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentPresetSettingsState {
    /// Current lifecycle status.
    pub status: AgentPresetStoreStatus,
    /// Latest failure copy.
    pub error: Option<String>,
    /// Whether Settings accepts writes.
    pub writable: bool,
    /// Selected default id.
    pub current_value: String,
    /// Healthy selectable presets.
    pub options: Vec<AgentPresetOption>,
}

impl Default for AgentPresetSettingsState {
    fn default() -> Self {
        Self {
            status: AgentPresetStoreStatus::Idle,
            error: None,
            writable: true,
            current_value: String::new(),
            options: Vec::new(),
        }
    }
}

struct ImmediateScheduler;

impl StoreFlushScheduler for ImmediateScheduler {
    fn queue(&self, callback: Box<dyn FnOnce()>) {
        callback();
    }
}

pub(crate) fn preset_snapshot_store<T: Clone + 'static>(initial: T) -> Rc<SnapshotStore<T>> {
    SnapshotStore::new(
        initial,
        StoreFlushMode::Sync,
        Rc::new(ImmediateScheduler),
        None,
        Rc::new(|_| {}) as StoreLogger,
    )
}

/// Reads the roster and persists its default through Host Settings.
pub struct AgentPresetSettingsController {
    transport: Rc<dyn AgentPresetSettingsTransport>,
    store: Rc<SnapshotStore<AgentPresetSettingsState>>,
}

impl AgentPresetSettingsController {
    /// Creates an idle controller.
    #[must_use]
    pub fn new(transport: Rc<dyn AgentPresetSettingsTransport>) -> Rc<Self> {
        Rc::new(Self {
            transport,
            store: preset_snapshot_store(AgentPresetSettingsState::default()),
        })
    }

    /// Reference-stable observable row state.
    #[must_use]
    pub fn store(&self) -> Rc<SnapshotStore<AgentPresetSettingsState>> {
        self.store.clone()
    }

    fn set(&self, update: impl FnOnce(&mut AgentPresetSettingsState)) {
        let mut next = self.store.snapshot().as_ref().clone();
        update(&mut next);
        self.store.set(next);
    }

    /// Loads roster options, default identity, and Settings writability.
    pub async fn load(&self) {
        if self.store.snapshot().status == AgentPresetStoreStatus::Loading {
            return;
        }
        self.set(|state| {
            state.status = AgentPresetStoreStatus::Loading;
            state.error = None;
        });
        let roster = match self.transport.list().await {
            Ok(roster) => roster,
            Err(error) => {
                self.set(|state| {
                    state.status = AgentPresetStoreStatus::Error;
                    state.error = Some(error);
                });
                return;
            }
        };
        let Some(first) = roster.presets.first() else {
            self.set(|state| {
                state.status = AgentPresetStoreStatus::Unavailable;
                state.options.clear();
                state.current_value.clear();
            });
            return;
        };
        let writable = match self.transport.describe_settings().await {
            Ok(writable) => writable,
            Err(error) => {
                self.set(|state| {
                    state.status = AgentPresetStoreStatus::Error;
                    state.error = Some(error);
                });
                return;
            }
        };
        let current = roster
            .presets
            .iter()
            .find(|preset| preset.is_default)
            .unwrap_or(first)
            .id
            .clone();
        self.set(|state| {
            state.status = AgentPresetStoreStatus::Ready;
            state.error = None;
            state.writable = writable;
            state.options = preset_options(&roster.presets);
            state.current_value = current;
        });
    }

    /// Optimistically selects a default, restoring Host truth on failure.
    pub async fn select(&self, id: &str) {
        let before = self.store.snapshot();
        if before.status == AgentPresetStoreStatus::Saving || before.current_value == id {
            return;
        }
        let previous = before.current_value.clone();
        self.set(|state| {
            state.status = AgentPresetStoreStatus::Saving;
            state.error = None;
            id.clone_into(&mut state.current_value);
        });
        if let Err(error) = self.transport.update_default(id.to_owned()).await {
            self.set(|state| {
                state.status = AgentPresetStoreStatus::Ready;
                state.current_value = previous;
                state.error = Some(error);
            });
            return;
        }
        self.load().await;
    }
}
