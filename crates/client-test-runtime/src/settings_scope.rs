//! Controllable in-memory Client settings scope.

use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};

use futures::{FutureExt as _, future::LocalBoxFuture};
use seekdeep_client_settings_contract::{
    ClientSettingsDisposer, ClientSettingsMode, ClientSettingsScope, ClientSettingsScopeSnapshot,
    ClientSettingsStatus,
};
use serde_json::Value;

/// Partial replacement applied by [`StubSettingsScope::publish`].
pub struct SettingsScopePatch<T> {
    /// New status, or the existing status when absent.
    pub status: Option<ClientSettingsStatus>,
    /// New optional decoded value; outer `None` preserves the existing field.
    pub value: Option<Option<Rc<T>>>,
    /// New optional composition layer; outer `None` preserves the existing field.
    pub base: Option<Option<Value>>,
    /// New optional user layer; outer `None` preserves the existing field.
    pub user: Option<Option<Value>>,
    /// New optional revision; outer `None` preserves the existing field.
    pub revision: Option<Option<f64>>,
    /// New writability when present.
    pub writable: Option<bool>,
    /// New ownership mode when present.
    pub mode: Option<ClientSettingsMode>,
}

impl<T> Default for SettingsScopePatch<T> {
    fn default() -> Self {
        Self {
            status: None,
            value: None,
            base: None,
            user: None,
            revision: None,
            writable: None,
            mode: None,
        }
    }
}

struct StubState<T> {
    snapshot: Rc<ClientSettingsScopeSnapshot<T>>,
    listeners: Vec<Rc<dyn Fn()>>,
    set_calls: Vec<(String, Value)>,
    unset_calls: Vec<String>,
}

struct StubScope<T> {
    state: Rc<RefCell<StubState<T>>>,
}

impl<T: 'static> ClientSettingsScope<T> for StubScope<T> {
    fn snapshot(&self) -> Rc<ClientSettingsScopeSnapshot<T>> {
        self.state.borrow().snapshot.clone()
    }

    fn subscribe(&self, listener: Rc<dyn Fn()>) -> ClientSettingsDisposer {
        let mut state = self.state.borrow_mut();
        if !state
            .listeners
            .iter()
            .any(|registered| Rc::ptr_eq(registered, &listener))
        {
            state.listeners.push(listener.clone());
        }
        drop(state);
        let state: Weak<RefCell<StubState<T>>> = Rc::downgrade(&self.state);
        ClientSettingsDisposer::new(move || {
            if let Some(state) = state.upgrade() {
                state
                    .borrow_mut()
                    .listeners
                    .retain(|registered| !Rc::ptr_eq(registered, &listener));
            }
        })
    }

    fn set(&self, field: String, value: Value) -> LocalBoxFuture<'static, Result<(), String>> {
        self.state.borrow_mut().set_calls.push((field, value));
        async { Ok(()) }.boxed_local()
    }

    fn unset(&self, field: String) -> LocalBoxFuture<'static, Result<(), String>> {
        self.state.borrow_mut().unset_calls.push(field);
        async { Ok(()) }.boxed_local()
    }
}

/// Handle over a settings-scope double, write records, and publication controls.
pub struct StubSettingsScope<T> {
    scope: Rc<StubScope<T>>,
}

impl<T> Clone for StubSettingsScope<T> {
    fn clone(&self) -> Self {
        Self {
            scope: self.scope.clone(),
        }
    }
}

impl<T: 'static> Default for StubSettingsScope<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: 'static> StubSettingsScope<T> {
    /// Constructs the source loading, read-only, Host-owned initial state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            scope: Rc::new(StubScope {
                state: Rc::new(RefCell::new(StubState {
                    snapshot: Rc::new(ClientSettingsScopeSnapshot {
                        status: ClientSettingsStatus::Loading,
                        value: None,
                        base: None,
                        user: None,
                        revision: None,
                        writable: false,
                        mode: ClientSettingsMode::Host,
                    }),
                    listeners: Vec::new(),
                    set_calls: Vec::new(),
                    unset_calls: Vec::new(),
                })),
            }),
        }
    }

    /// Returns the face handed to the service under test.
    #[must_use]
    pub fn scope(&self) -> Rc<dyn ClientSettingsScope<T>> {
        self.scope.clone()
    }

    /// Number of currently subscribed listeners.
    #[must_use]
    pub fn listener_count(&self) -> usize {
        self.scope.state.borrow().listeners.len()
    }

    /// Snapshot of recorded `set` writes in call order.
    #[must_use]
    pub fn set_calls(&self) -> Vec<(String, Value)> {
        self.scope.state.borrow().set_calls.clone()
    }

    /// Snapshot of recorded `unset` writes in call order.
    #[must_use]
    pub fn unset_calls(&self) -> Vec<String> {
        self.scope.state.borrow().unset_calls.clone()
    }

    /// Replaces supplied snapshot fields and synchronously notifies a listener snapshot.
    pub fn publish(&self, patch: SettingsScopePatch<T>) {
        let listeners = {
            let mut state = self.scope.state.borrow_mut();
            let current = &state.snapshot;
            state.snapshot = Rc::new(ClientSettingsScopeSnapshot {
                status: patch.status.unwrap_or(current.status),
                value: patch.value.unwrap_or_else(|| current.value.clone()),
                base: patch.base.unwrap_or_else(|| current.base.clone()),
                user: patch.user.unwrap_or_else(|| current.user.clone()),
                revision: patch.revision.unwrap_or(current.revision),
                writable: patch.writable.unwrap_or(current.writable),
                mode: patch.mode.unwrap_or(current.mode),
            });
            state.listeners.clone()
        };
        for listener in listeners {
            listener();
        }
    }
}
