//! In-app directory-browser state, navigation, and Rust/WASM surfaces.

mod controller;
mod model;

pub use controller::*;
pub use model::*;

/// Stable Host plugin identity for the pure Client package.
pub const NAME: &str = "client-ui-directory-picker-browse";

/// Builds the inert Host half of the Client-only surface package.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
