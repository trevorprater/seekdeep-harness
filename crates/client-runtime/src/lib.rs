//! React-free Client object layer and browser runtime services.

mod slots;
mod store;
#[cfg(target_arch = "wasm32")]
mod wasm_slots;
#[cfg(target_arch = "wasm32")]
mod wasm_store;

pub use slots::*;
pub use store::*;
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
