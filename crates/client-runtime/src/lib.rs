//! React-free Client object layer and browser runtime services.

mod assistant_timing;
mod context_provenance;
mod conversation_registry;
mod lineage;
mod ordered_baseline;
mod slots;
mod store;
mod subagent_lineage;
mod time_zone;
#[cfg(target_arch = "wasm32")]
mod wasm_conversation_registry;
#[cfg(target_arch = "wasm32")]
mod wasm_misc;
#[cfg(target_arch = "wasm32")]
mod wasm_slots;
#[cfg(target_arch = "wasm32")]
mod wasm_store;

pub use assistant_timing::*;
pub use context_provenance::*;
pub use conversation_registry::*;
pub use lineage::*;
pub use ordered_baseline::*;
pub use slots::*;
pub use store::*;
pub use subagent_lineage::*;
pub use time_zone::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_conversation_registry::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_misc::*;
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
