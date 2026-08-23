//! React-free Client object layer and browser runtime services.

mod assistant_timing;
mod context_provenance;
mod conversation_context;
mod conversation_registry;
mod lineage;
mod notifier;
mod ordered_baseline;
mod partial;
mod pending;
mod projection_store;
mod queue_mirror;
mod request_inspection;
mod slots;
mod steering_history;
mod store;
mod subagent_lineage;
mod time_zone;
mod tool_call_tree;
#[cfg(target_arch = "wasm32")]
mod wasm_conversation_registry;
#[cfg(target_arch = "wasm32")]
mod wasm_misc;
#[cfg(target_arch = "wasm32")]
mod wasm_notifier;
#[cfg(target_arch = "wasm32")]
mod wasm_projection_store;
#[cfg(target_arch = "wasm32")]
mod wasm_slots;
#[cfg(target_arch = "wasm32")]
mod wasm_store;

pub use assistant_timing::*;
pub use context_provenance::*;
pub use conversation_context::*;
pub use conversation_registry::*;
pub use lineage::*;
pub use notifier::*;
pub use ordered_baseline::*;
pub use partial::*;
pub use pending::*;
pub use projection_store::*;
pub use queue_mirror::*;
pub use request_inspection::*;
pub use slots::*;
pub use steering_history::*;
pub use store::*;
pub use subagent_lineage::*;
pub use time_zone::*;
pub use tool_call_tree::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_conversation_registry::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_misc::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_projection_store::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_slots::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_store::*;

/// Host-side package row; the observable runtime lives in the browser entrypoint.
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new("client-runtime", std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
