//! Framework-neutral Store and observable contracts used at Slot boundaries.

use std::{any::Any, rc::Rc};

/// Minimal observable API for framework-provided snapshot sources.
pub trait HostObservable<T> {
    /// Returns a reference-stable snapshot until the next notification.
    fn snapshot(&self) -> Rc<T>;
    /// Subscribes to committed snapshot changes.
    fn subscribe(&self, listener: Rc<dyn Fn()>) -> Box<dyn Fn()>;
}

/// Type-erased Store instance face at the render boundary.
pub trait SlotStoreInstance: Any {
    /// Type-erased downcast support for host adapters retaining native instance values.
    fn as_any(&self) -> &dyn Any;
    /// Returns the current JSON-compatible snapshot.
    fn snapshot(&self) -> serde_json::Value;
    /// Subscribes to committed state changes.
    fn subscribe(&self, listener: Rc<dyn Fn()>) -> Box<dyn Fn()>;
    /// Removes persisted state when the owning scope dies permanently.
    fn clear_persisted(&self);
}

/// Framework-owned factory for one entry-and-scope Store instance.
pub trait SlotStoreFactory {
    /// Creates a fresh root or session-scoped instance.
    fn create(&self, scope_key: Option<&str>) -> Rc<dyn SlotStoreInstance>;
}
