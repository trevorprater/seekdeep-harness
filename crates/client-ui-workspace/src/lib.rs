//! Workspace browser tree, search, ordering, and Rust/WASM UI semantics.

mod browser_logic;
mod locales;
mod state;
mod tree;

#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
mod browser_apply;
#[cfg(target_arch = "wasm32")]
mod browser_lists;
#[cfg(target_arch = "wasm32")]
mod browser_model;
#[cfg(target_arch = "wasm32")]
mod browser_picker;
#[cfg(target_arch = "wasm32")]
mod browser_rows;
#[cfg(target_arch = "wasm32")]
mod browser_workspace;

pub use browser_logic::*;
pub use locales::*;
pub use state::*;
pub use tree::*;

#[cfg(target_arch = "wasm32")]
pub use browser_apply::*;
#[cfg(target_arch = "wasm32")]
pub use browser_picker::*;
#[cfg(target_arch = "wasm32")]
pub use browser_rows::*;
#[cfg(target_arch = "wasm32")]
pub use browser_workspace::*;

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
