//! Subagent catalog and read-only composer semantics.

mod locales;
mod model;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use locales::*;
pub use model::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;

/// Compiled subagent catalog stylesheet.
pub const SUBAGENT_CATALOG_STYLES: &str = include_str!("../data/catalog.css");
/// Compiled read-only composer stylesheet.
pub const SUBAGENT_READ_ONLY_STYLES: &str = include_str!("../data/read-only.css");

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-subagent";

/// Builds the no-op Host half of this pure Client plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
