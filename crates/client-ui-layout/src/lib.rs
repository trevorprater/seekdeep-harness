//! Three-column browser shell geometry, transient panel state, and plugin facade.

mod columns;
mod service;
mod state;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use columns::*;
pub use service::*;
pub use state::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;

/// Browser plugin dependencies in source order.
pub const INJECT: &[&str] = &["slots", "theme"];
/// Body attribute selecting the dark token palette.
pub const DARK_ATTRIBUTE: &str = "data-ds-dark-theme";
/// Stable no-op invariant companion identity.
pub const INVARIANT_NAME: &str = "client-ui-layout-invariant";
/// Compiled shell stylesheet embedded by the browser module.
pub const LAYOUT_STYLES: &str = include_str!("../data/styles.css");

/// Host-side no-op package row.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new("client-ui-layout", std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
