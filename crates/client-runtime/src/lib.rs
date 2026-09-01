//! React-free Client object layer and browser runtime services.

mod assistant_timing;
mod context_provenance;
mod conversation_assembler;
mod conversation_context;
mod conversation_location;
mod conversation_registry;
mod lineage;
mod notifier;
mod ordered_baseline;
mod partial;
mod pending;
mod projection_store;
mod provide;
mod queue_mirror;
mod request_inspection;
mod session;
mod session_manager;
mod session_service;
mod settings_scope;
mod slots;
mod steering_history;
mod store;
mod subagent_lineage;
mod time_zone;
mod tool_call_tree;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_client_runtime;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_conversation_adapter;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_conversation_public;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_conversation_registry;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_invariant;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_misc;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_native_definition;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_notifier;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_pending;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_projection_store;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_provide;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_public_api;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_scope;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_session;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_session_manager;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_session_service;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_slots;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_store;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm_workspace_service;
mod workspace;
mod workspace_manager;
mod workspace_service;

pub use assistant_timing::*;
pub use context_provenance::*;
pub use conversation_assembler::*;
pub use conversation_context::*;
pub use conversation_location::*;
pub use conversation_registry::*;
pub use lineage::*;
pub use notifier::*;
pub use ordered_baseline::*;
pub use partial::*;
pub use pending::*;
pub use projection_store::*;
pub use provide::*;
pub use queue_mirror::*;
pub use request_inspection::*;
pub use session::*;
pub use session_manager::*;
pub use session_service::*;
pub use settings_scope::*;
pub use slots::*;
pub use steering_history::*;
pub use store::*;
pub use subagent_lineage::*;
pub use time_zone::*;
pub use tool_call_tree::*;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
pub use wasm_client_runtime::*;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
pub use wasm_conversation_public::*;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
pub use wasm_conversation_registry::*;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
pub use wasm_invariant::*;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
pub use wasm_misc::*;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
pub use wasm_native_definition::*;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
pub use wasm_pending::*;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
pub use wasm_projection_store::*;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
pub use wasm_provide::*;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
pub use wasm_public_api::*;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
pub use wasm_scope::*;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
pub use wasm_session::*;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
pub use wasm_session_manager::*;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
pub use wasm_session_service::*;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
pub use wasm_slots::*;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
pub use wasm_store::*;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
pub use wasm_workspace_service::*;
pub use workspace::*;
pub use workspace_manager::*;
pub use workspace_service::*;

/// Host-side package row; the observable runtime lives in the browser entrypoint.
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new("client-runtime", std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
