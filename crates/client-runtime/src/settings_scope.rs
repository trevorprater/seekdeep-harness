//! Target-portable Client settings-namespace scope contract.

use std::rc::Rc;

use futures::future::LocalBoxFuture;
use serde_json::Value;

use crate::RuntimeDisposer;

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
pub struct ClientSettingsScopeSnapshot<T> {
    /// Current sync state.
    pub status: ClientSettingsStatus,
    /// Last accepted decoded value.
    pub value: Option<Rc<T>>,
    /// Resolved composition layer.
    pub base: Value,
    /// Raw user layer whose field presence marks overrides.
    pub user: Value,
    /// Revision fencing the next write.
    pub revision: Option<u64>,
    /// Whether the Host document accepts writes.
    pub writable: bool,
    /// Durable Host or process-local memory ownership.
    pub mode: ClientSettingsMode,
}

/// Optional domain-specific Settings section decoder.
pub type ClientSettingsDecoder<T> = Rc<dyn Fn(&Value) -> Option<T>>;

/// Domain-owned description of one Settings namespace.
pub struct ClientSettingsScopeSpec<T> {
    /// Registered namespace.
    pub namespace: String,
    /// Optional narrowing beyond the namespace wire schema.
    pub decode: Option<ClientSettingsDecoder<T>>,
}

/// Reactive owner handle over one namespace's durable section.
pub trait ClientSettingsScope<T> {
    /// Current reference-stable snapshot.
    fn snapshot(&self) -> Rc<ClientSettingsScopeSnapshot<T>>;
    /// Observes snapshot replacements.
    fn subscribe(&self, listener: Rc<dyn Fn()>) -> RuntimeDisposer;
    /// Queues one ordered revision-fenced field write.
    fn set(&self, field: String, value: Value) -> LocalBoxFuture<'static, ()>;
    /// Queues one ordered revision-fenced field clear.
    fn unset(&self, field: String) -> LocalBoxFuture<'static, ()>;
}
