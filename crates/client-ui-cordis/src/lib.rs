//! Rust-owned Cordis browser inventory and lifecycle-card state.

mod card_model;
mod dynamic_port;
mod inventory;
mod locales;
mod presentation;
mod run_card_index;
mod slots;
mod status;
#[cfg(target_arch = "wasm32")]
mod wasm;
#[cfg(target_arch = "wasm32")]
mod wasm_components;
#[cfg(target_arch = "wasm32")]
mod wasm_plugin;

pub use card_model::*;
pub use dynamic_port::*;
pub use inventory::*;
pub use locales::*;
pub use presentation::*;
pub use run_card_index::*;
pub use slots::*;
pub use status::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_components::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_plugin::*;

pub use seekdeep_cordis_dynamic_types::{
    ApprovalRequestId, CordisDynamicPackageId, CordisDynamicPluginId, CordisDynamicPluginRunId,
    DynamicCordisInventoryPackage, DynamicCordisInventoryRow, DynamicCordisPackage,
    DynamicCordisRequestResolved, DynamicCordisRetracted, DynamicCordisRunMode,
    DynamicCordisRunRequest,
};

/// Host-side package row; every observable UI behavior belongs to the browser entrypoint.
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new("client-ui-cordis", std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
