//! Plugin lifecycle, service registry, scoped contexts, and reversible effects.

/// Scoped dependency container.
pub mod context;
/// Dynamically named event bus.
pub mod events;
/// Plugin lifecycle ownership.
pub mod fiber;
/// Structured, exporter-driven logging with injectable time.
pub mod logger;
/// Plugin registration and dependency-driven activation.
pub mod plugin;
/// Typed and type-erased services.
pub mod service;
/// Identity-disposable and error-composition utility substrate.
pub mod utils;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use context::{Context, DynamicValue, MixinHandle, MixinMember};
pub use events::{
    BailReply, DispatchMode, EventArgs, EventBus, EventOptions, EventReply, EventSubjectToken,
    EventValue, PreparedEmission,
};
pub use fiber::{CordisError, Fiber, FiberState};
pub use logger::{
    CordisClock, LogExporter, LogFormatter, LogMessage, Logger, LoggerLevel, LoggerOptions,
    LoggerService, LoggerType, SystemCordisClock,
};
pub use plugin::{Plugin, PluginFiber, PluginRegistry, PluginRuntimeSnapshot};
pub use service::{Service, ServiceKey, ServiceProviderSnapshot};
pub use utils::{DisposableList, DisposableListHandle, compose_error, is_json_object_like};
#[cfg(target_arch = "wasm32")]
pub use wasm::*;
