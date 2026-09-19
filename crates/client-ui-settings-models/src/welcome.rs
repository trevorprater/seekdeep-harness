//! Welcome notice acknowledgement state machine.

use std::{cell::Cell, rc::Rc};

use futures::future::LocalBoxFuture;
use seekdeep_client_runtime::{SnapshotStore, StoreFlushMode, StoreFlushScheduler, StoreLogger};

use crate::{WELCOME_NOTICE_ACK_FIELD, WELCOME_NOTICE_SETTINGS_NAMESPACE, WELCOME_NOTICE_VERSION};

/// Welcome notice operation status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WelcomeStatus {
    /// No operation has started.
    #[default]
    Idle,
    /// Durable acknowledgement is loading.
    Loading,
    /// Current state is known.
    Ready,
    /// Acknowledgement is saving.
    Saving,
    /// Latest operation failed.
    Error,
}

/// Welcome notice state rendered by onboarding.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WelcomeNoticeState {
    /// Current operation status.
    pub status: WelcomeStatus,
    /// Whether this exact notice version is acknowledged.
    pub acknowledged: bool,
    /// Latest operation failure.
    pub error: Option<String>,
}

/// Durable settings calls needed by the welcome store.
pub trait WelcomeTransport {
    /// Loads the stored acknowledgement value, or `None` for absent/malformed state.
    fn describe(&self) -> LocalBoxFuture<'static, Result<Option<String>, String>>;
    /// Persists the exact namespace/field/version mutation.
    fn acknowledge(
        &self,
        namespace: &'static str,
        field: &'static str,
        version: &'static str,
    ) -> LocalBoxFuture<'static, Result<(), String>>;
}

/// Persistence boundary selected for this browser.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WelcomePersistence {
    /// Loopback browser may use Host settings.
    #[default]
    Host,
    /// Remote browser retains process-local acknowledgement only.
    Memory,
}

struct NoopScheduler;

impl StoreFlushScheduler for NoopScheduler {
    fn queue(&self, callback: Box<dyn FnOnce()>) {
        callback();
    }
}

/// Welcome acknowledgement controller with latest-operation-wins guards.
pub struct WelcomeNoticeStore {
    /// Immutable observable state.
    pub store: Rc<SnapshotStore<WelcomeNoticeState>>,
    transport: Rc<dyn WelcomeTransport>,
    persistence: WelcomePersistence,
    generation: Cell<u64>,
}

impl std::fmt::Debug for WelcomeNoticeStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WelcomeNoticeStore")
            .field("persistence", &self.persistence)
            .field("generation", &self.generation.get())
            .finish_non_exhaustive()
    }
}

impl WelcomeNoticeStore {
    /// Creates an idle welcome controller.
    #[must_use]
    pub fn new(transport: Rc<dyn WelcomeTransport>, persistence: WelcomePersistence) -> Rc<Self> {
        Rc::new(Self {
            store: SnapshotStore::new(
                WelcomeNoticeState::default(),
                StoreFlushMode::Sync,
                Rc::new(NoopScheduler),
                None,
                Rc::new(|_| {}) as StoreLogger,
            ),
            transport,
            persistence,
            generation: Cell::new(0),
        })
    }

    fn next_generation(&self) -> u64 {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        generation
    }

    /// Loads durable acknowledgement or initializes memory mode.
    pub async fn load(&self) {
        let generation = self.next_generation();
        if self.persistence == WelcomePersistence::Memory {
            self.store.update(|state| {
                state.status = WelcomeStatus::Ready;
                state.error = None;
            });
            return;
        }
        self.store.update(|state| {
            state.status = WelcomeStatus::Loading;
            state.error = None;
        });
        let result = self.transport.describe().await;
        if self.generation.get() != generation {
            return;
        }
        match result {
            Ok(version) => self.store.update(|state| {
                state.status = WelcomeStatus::Ready;
                state.acknowledged = version.as_deref() == Some(WELCOME_NOTICE_VERSION);
                state.error = None;
            }),
            Err(error) => self.store.update(|state| {
                state.status = WelcomeStatus::Error;
                state.acknowledged = false;
                state.error = Some(error);
            }),
        }
    }

    /// Persists this copy version or advances process-local memory state.
    pub async fn acknowledge(&self) -> bool {
        let generation = self.next_generation();
        if self.persistence == WelcomePersistence::Memory {
            self.store.update(|state| {
                state.status = WelcomeStatus::Ready;
                state.acknowledged = true;
                state.error = None;
            });
            return true;
        }
        self.store.update(|state| {
            state.status = WelcomeStatus::Saving;
            state.error = None;
        });
        let result = self
            .transport
            .acknowledge(
                WELCOME_NOTICE_SETTINGS_NAMESPACE,
                WELCOME_NOTICE_ACK_FIELD,
                WELCOME_NOTICE_VERSION,
            )
            .await;
        if self.generation.get() == generation {
            match &result {
                Ok(()) => self.store.update(|state| {
                    state.status = WelcomeStatus::Ready;
                    state.acknowledged = true;
                    state.error = None;
                }),
                Err(error) => self.store.update(|state| {
                    state.status = WelcomeStatus::Error;
                    state.acknowledged = false;
                    state.error = Some(error.clone());
                }),
            }
        }
        result.is_ok()
    }

    /// Reloads only after this store has left idle.
    pub fn should_refresh(&self) -> bool {
        self.store.snapshot().status != WelcomeStatus::Idle
    }
}
