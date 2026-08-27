//! Workspace browser tree, search, ordering, and Rust/WASM UI semantics.

mod locales;
mod state;
mod tree;

pub use locales::*;
pub use state::*;
pub use tree::*;

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-workspace";

/// Builds the no-op Host half of this pure Client plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
