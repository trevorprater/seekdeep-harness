//! Rust-owned browser-half evaluation contracts and dynamic package state.

mod api_catalog;
mod evaluator;
mod face;
mod guard;
mod inspect_registry;
mod orchestrator;
mod providers;
mod runtime;
mod slot_catalog;
#[cfg(target_arch = "wasm32")]
mod wasm_evaluator;
#[cfg(target_arch = "wasm32")]
mod wasm_guard;
#[cfg(target_arch = "wasm32")]
mod wasm_mount_engine;
#[cfg(target_arch = "wasm32")]
mod wasm_plugin;
#[cfg(target_arch = "wasm32")]
mod wasm_providers;
#[cfg(target_arch = "wasm32")]
mod wasm_remote;
#[cfg(target_arch = "wasm32")]
mod wasm_runtime;
#[cfg(target_arch = "wasm32")]
mod wasm_timer;
#[cfg(target_arch = "wasm32")]
mod wasm_timer_service;

pub use api_catalog::*;
pub use evaluator::*;
pub use face::*;
pub use guard::*;
pub use inspect_registry::*;
pub use orchestrator::*;
pub use providers::*;
pub use runtime::*;
pub use slot_catalog::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_evaluator::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_guard::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_mount_engine::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_plugin::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_providers::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_remote::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_runtime::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_timer::*;
#[cfg(target_arch = "wasm32")]
pub use wasm_timer_service::*;

/// Host-side package row; all capability ships through the Client entrypoint.
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(
        "cordis-client-runner",
        std::iter::empty::<String>(),
        |_, _| Box::pin(async { Ok(()) }),
    )
}
