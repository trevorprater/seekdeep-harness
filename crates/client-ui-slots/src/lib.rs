//! Portable Slot registry, observable ledger, and renderer/store contracts.

mod core;
mod renderer;
mod store;
mod typed;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
mod wasm;

pub use core::*;
pub use renderer::*;
pub use store::*;
pub use typed::*;
#[cfg(all(target_arch = "wasm32", feature = "wasm-bindings"))]
pub use wasm::*;

/// Host-side package row; browser behavior ships through the Rust/WASM entrypoint.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new("client-ui-slots", std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
