//! Portable permission Settings controller with generation-owned publication.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::{Rc, Weak},
};

use futures::{FutureExt as _, future::LocalBoxFuture};
use serde_json::Value;

use crate::{PERMISSION_SETTINGS_NS, PermissionDefaultOption, permission_default_of};

/// Permission Settings load/save lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionSettingsStatus {
    /// No descriptor request has started.
    Idle,
    /// Descriptor request is active.
    Loading,
    /// Descriptor and options are usable.
    Ready,
    /// Mutation request is active.
    Saving,
    /// Host does not expose the namespace.
    Unavailable,
    /// Latest operation failed.
    Error,
}

/// Permission settings-row snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionSettingsState {
    /// Current lifecycle.
    pub status: PermissionSettingsStatus,
    /// Latest contained failure message.
    pub error: Option<String>,
    /// Whether the Host permits mutation.
    pub writable: bool,
    /// Current advertised default.
    pub current_value: String,
    /// Dynamic choices.
    pub options: Vec<PermissionDefaultOption>,
    /// Optimistic-concurrency revision.
    pub revision: u64,
}

impl Default for PermissionSettingsState {
    fn default() -> Self {
        Self {
            status: PermissionSettingsStatus::Idle,
            error: None,
            writable: false,
            current_value: String::new(),
            options: Vec::new(),
            revision: 0,
        }
    }
}

/// One Host Settings namespace descriptor.
#[derive(Clone, Debug, PartialEq)]
pub struct PermissionNamespaceView {
    /// Namespace identity.
    pub namespace: String,
    /// Schemastery wire graph.
    pub schema: Value,
    /// Current namespace value.
    pub value: Value,
    /// Optimistic-concurrency revision.
    pub revision: u64,
}

/// Settings describe projection used by this controller.
#[derive(Clone, Debug, PartialEq)]
pub struct PermissionSettingsDescription {
    /// Whether namespace writes are accepted.
    pub writable: bool,
    /// Available namespace descriptors.
    pub namespaces: Vec<PermissionNamespaceView>,
}

/// Exact permission default mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionSettingsMutation {
    /// Advertised preset to write.
    pub preset: String,
    /// Descriptor revision the write is based on.
    pub expected_revision: u64,
}

/// Injected Settings transport.
pub trait PermissionSettingsTransport {
    /// Describes every exposed namespace.
    fn describe(&self) -> LocalBoxFuture<'static, Result<PermissionSettingsDescription, String>>;

    /// Writes `defaultPreset` and returns the replacement descriptor.
    fn mutate(
        &self,
        request: PermissionSettingsMutation,
    ) -> LocalBoxFuture<'static, Result<PermissionNamespaceView, String>>;
}

type Listener = Rc<dyn Fn()>;

struct ControllerState {
    generation: u64,
    view: Option<PermissionNamespaceView>,
    snapshot: Rc<PermissionSettingsState>,
    listeners: BTreeMap<u64, Listener>,
    next_listener: u64,
}

/// Latest-request-wins permission Settings controller.
pub struct PermissionPresetSettingsController {
    transport: Rc<dyn PermissionSettingsTransport>,
    state: RefCell<ControllerState>,
}

impl PermissionPresetSettingsController {
    /// Creates an idle controller.
    #[must_use]
    pub fn new(transport: Rc<dyn PermissionSettingsTransport>) -> Rc<Self> {
        Rc::new(Self {
            transport,
            state: RefCell::new(ControllerState {
                generation: 0,
                view: None,
                snapshot: Rc::new(PermissionSettingsState::default()),
                listeners: BTreeMap::new(),
                next_listener: 0,
            }),
        })
    }

    /// Returns a stable snapshot identity until the next update.
    #[must_use]
    pub fn snapshot(&self) -> Rc<PermissionSettingsState> {
        self.state.borrow().snapshot.clone()
    }

    /// Subscribes to every synchronous state replacement.
    ///
    /// # Panics
    ///
    /// Panics after exhausting every `u64` listener id rather than aliasing a live subscription.
    #[must_use]
    pub fn subscribe(self: &Rc<Self>, listener: Listener) -> PermissionSettingsSubscription {
        let id = {
            let mut state = self.state.borrow_mut();
            state.next_listener = state
                .next_listener
                .checked_add(1)
                .expect("permission Settings listener id exhausted");
            let id = state.next_listener;
            state.listeners.insert(id, listener);
            id
        };
        PermissionSettingsSubscription {
            controller: Rc::downgrade(self),
            id: Some(id),
        }
    }

    /// Loads the descriptor; only the exact current generation may publish.
    #[must_use]
    pub fn load(self: &Rc<Self>) -> LocalBoxFuture<'static, ()> {
        let generation = self.begin(PermissionSettingsStatus::Loading);
        let controller = self.clone();
        async move {
            let result = controller.transport.describe().await;
            if !controller.is_current(generation) {
                return;
            }
            match result {
                Ok(description) => {
                    if let Err(error) = controller.accept_description(description) {
                        controller.fail(error);
                    }
                }
                Err(error) => controller.fail(error),
            }
        }
        .boxed_local()
    }

    /// Persists one preset; absent and read-only descriptors are no-ops.
    #[must_use]
    pub fn select(self: &Rc<Self>, preset: String) -> LocalBoxFuture<'static, ()> {
        let request = {
            let state = self.state.borrow();
            let Some(view) = &state.view else {
                return futures::future::ready(()).boxed_local();
            };
            if !state.snapshot.writable {
                return futures::future::ready(()).boxed_local();
            }
            PermissionSettingsMutation {
                preset,
                expected_revision: view.revision,
            }
        };
        let generation = self.begin(PermissionSettingsStatus::Saving);
        let controller = self.clone();
        async move {
            let result = controller.transport.mutate(request).await;
            if !controller.is_current(generation) {
                return;
            }
            match result {
                Ok(view) => {
                    if let Err(error) = controller.accept(view, true) {
                        controller.fail(error);
                    }
                }
                Err(error) => controller.fail(error),
            }
        }
        .boxed_local()
    }

    /// Invalidates in-flight publication and releases the writable descriptor.
    ///
    /// # Panics
    ///
    /// Panics after exhausting every `u64` generation rather than permitting stale publication.
    pub fn dispose(&self) {
        let mut state = self.state.borrow_mut();
        state.generation = state
            .generation
            .checked_add(1)
            .expect("permission Settings generation exhausted");
        state.view = None;
    }

    /// Refetches only after the row has attempted its initial load.
    #[must_use]
    pub fn refresh_if_loaded(self: &Rc<Self>) -> Option<LocalBoxFuture<'static, ()>> {
        (self.snapshot().status != PermissionSettingsStatus::Idle).then(|| self.load())
    }

    fn begin(&self, status: PermissionSettingsStatus) -> u64 {
        let generation = {
            let mut state = self.state.borrow_mut();
            state.generation = state
                .generation
                .checked_add(1)
                .expect("permission Settings generation exhausted");
            state.generation
        };
        self.update(|state| {
            state.status = status;
            state.error = None;
        });
        generation
    }

    fn is_current(&self, generation: u64) -> bool {
        self.state.borrow().generation == generation
    }

    fn accept_description(
        &self,
        mut description: PermissionSettingsDescription,
    ) -> Result<(), String> {
        let Some(view) = description
            .namespaces
            .drain(..)
            .find(|view| view.namespace == PERMISSION_SETTINGS_NS)
        else {
            self.state.borrow_mut().view = None;
            self.update(|state| {
                state.status = PermissionSettingsStatus::Unavailable;
                state.writable = false;
                state.current_value.clear();
                state.options.clear();
            });
            return Ok(());
        };
        self.accept(view, description.writable)
    }

    fn accept(&self, view: PermissionNamespaceView, writable: bool) -> Result<(), String> {
        let resolved = permission_default_of(&view.schema, &view.value)?;
        let revision = view.revision;
        self.state.borrow_mut().view = Some(view);
        self.update(move |state| {
            state.status = PermissionSettingsStatus::Ready;
            state.error = None;
            state.writable = writable;
            state.current_value = resolved.current_value;
            state.options = resolved.options;
            state.revision = revision;
        });
        Ok(())
    }

    fn fail(&self, error: String) {
        self.update(move |state| {
            state.status = PermissionSettingsStatus::Error;
            state.error = Some(error);
        });
    }

    fn update(&self, mutate: impl FnOnce(&mut PermissionSettingsState)) {
        let listeners = {
            let mut state = self.state.borrow_mut();
            let mut snapshot = (*state.snapshot).clone();
            mutate(&mut snapshot);
            state.snapshot = Rc::new(snapshot);
            state.listeners.values().cloned().collect::<Vec<_>>()
        };
        for listener in listeners {
            listener();
        }
    }

    fn unsubscribe(&self, id: u64) {
        self.state.borrow_mut().listeners.remove(&id);
    }
}

/// Idempotent controller subscription.
pub struct PermissionSettingsSubscription {
    controller: Weak<PermissionPresetSettingsController>,
    id: Option<u64>,
}

impl PermissionSettingsSubscription {
    /// Removes the listener once.
    pub fn dispose(&mut self) {
        let Some(id) = self.id.take() else {
            return;
        };
        if let Some(controller) = self.controller.upgrade() {
            controller.unsubscribe(id);
        }
    }
}

impl Drop for PermissionSettingsSubscription {
    fn drop(&mut self) {
        self.dispose();
    }
}
