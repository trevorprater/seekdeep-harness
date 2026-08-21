//! Plugin lifecycle, service registry, scoped contexts, and reversible effects.

/// Scoped dependency container.
pub mod context;
/// Dynamically named event bus.
pub mod events;
/// Plugin lifecycle ownership.
pub mod fiber;
/// Plugin registration and dependency-driven activation.
pub mod plugin;
/// Typed and type-erased services.
pub mod service;

pub use context::Context;
pub use events::{
    DispatchMode, EventArgs, EventBus, EventOptions, EventReply, EventSubjectToken, EventValue,
    PreparedEmission,
};
pub use fiber::{CordisError, Fiber, FiberState};
pub use plugin::{Plugin, PluginFiber, PluginRegistry};
pub use service::{Service, ServiceKey};
