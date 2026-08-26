//! Target-portable Client settings namespace scope contract.

use std::{cell::RefCell, rc::Rc};

use futures::future::LocalBoxFuture;
use serde_json::Value;

/// Settings namespace identity crossing the browser/Host protocol boundary.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientSettingsNamespace(String);

impl ClientSettingsNamespace {
    /// Preserves the exact branded namespace supplied by a Client feature.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Exact wire spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Client-side sync state of one Settings namespace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientSettingsStatus {
    /// No accepted section has arrived.
    Loading,
    /// One accepted section is current.
    Ready,
    /// The namespace or durable Host document is unavailable.
    Unavailable,
}

/// Settings persistence owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientSettingsMode {
    /// Synchronizes through the Host document.
    Host,
    /// Remains process-local and read-only.
    Memory,
}

/// Immutable browser mirror of one Settings namespace.
#[derive(Clone)]
pub struct ClientSettingsScopeSnapshot<T> {
    /// Current sync state.
    pub status: ClientSettingsStatus,
    /// Last accepted decoded value.
    pub value: Option<Rc<T>>,
    /// Resolved composition layer when the Host supplied one.
    pub base: Option<Value>,
    /// Raw user layer whose field presence marks overrides.
    pub user: Option<Value>,
    /// Revision fencing the next write.
    pub revision: Option<f64>,
    /// Whether the Host document accepts writes.
    pub writable: bool,
    /// Durable Host or process-local memory ownership.
    pub mode: ClientSettingsMode,
}

/// Optional domain-specific Settings section decoder.
pub type ClientSettingsDecoder<T> = Rc<dyn Fn(&Value) -> Result<Option<T>, String>>;

/// Domain-owned description of one Settings namespace.
pub struct ClientSettingsScopeSpec<T> {
    /// Registered namespace.
    pub namespace: ClientSettingsNamespace,
    /// Optional narrowing beyond the namespace wire schema.
    pub decode: Option<ClientSettingsDecoder<T>>,
}

type DisposalCallback = Rc<RefCell<Option<Box<dyn FnOnce()>>>>;

/// Idempotent settings-scope listener cleanup.
#[derive(Clone)]
pub struct ClientSettingsDisposer {
    callback: DisposalCallback,
}

impl std::fmt::Debug for ClientSettingsDisposer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClientSettingsDisposer")
            .field("active", &self.callback.borrow().is_some())
            .finish()
    }
}

impl ClientSettingsDisposer {
    /// Wraps one exact-once listener cleanup.
    #[must_use]
    pub fn new(callback: impl FnOnce() + 'static) -> Self {
        Self {
            callback: Rc::new(RefCell::new(Some(Box::new(callback)))),
        }
    }

    /// Runs cleanup at most once.
    pub fn dispose(&self) {
        if let Some(callback) = self.callback.borrow_mut().take() {
            callback();
        }
    }

    /// Whether cleanup remains active.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.callback.borrow().is_some()
    }
}

/// Reactive owner handle over one namespace's durable section.
pub trait ClientSettingsScope<T> {
    /// Current reference-stable snapshot.
    fn snapshot(&self) -> Rc<ClientSettingsScopeSnapshot<T>>;
    /// Observes snapshot replacements.
    fn subscribe(&self, listener: Rc<dyn Fn()>) -> ClientSettingsDisposer;
    /// Queues one ordered revision-fenced field write.
    fn set(&self, field: String, value: Value) -> LocalBoxFuture<'static, Result<(), String>>;
    /// Queues one ordered revision-fenced field clear.
    fn unset(&self, field: String) -> LocalBoxFuture<'static, Result<(), String>>;
}
