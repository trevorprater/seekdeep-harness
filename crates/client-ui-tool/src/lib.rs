//! Tool-call semantic models shared by native tests and the browser WASM boundary.

mod cards;
mod model;
mod plan;

#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
mod browser_apply;
#[cfg(target_arch = "wasm32")]
mod browser_details;
#[cfg(target_arch = "wasm32")]
mod browser_model;
#[cfg(target_arch = "wasm32")]
mod browser_row;
#[cfg(target_arch = "wasm32")]
mod browser_tree;
#[cfg(target_arch = "wasm32")]
mod browser_views;

pub use cards::*;
pub use model::*;
pub use plan::*;

#[cfg(target_arch = "wasm32")]
pub use browser_apply::*;
#[cfg(target_arch = "wasm32")]
pub use browser_details::*;
#[cfg(target_arch = "wasm32")]
pub use browser_row::*;
#[cfg(target_arch = "wasm32")]
pub use browser_tree::*;
#[cfg(target_arch = "wasm32")]
pub use browser_views::*;

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-tool";

/// Builds the no-op Host half of this pure Client plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
