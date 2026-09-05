//! Settings-domain Client service, portable namespace synchronization, and Slot contracts.

mod core;
mod slots;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use core::*;
pub use slots::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;

/// Stable no-op invariant companion identity.
pub const INVARIANT_NAME: &str = "client-ui-settings-invariant";

/// Host-side package row; all observable behavior lives in the browser entrypoint.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(
        "client-ui-settings",
        std::iter::empty::<String>(),
        |_, _| Box::pin(async { Ok(()) }),
    )
}
