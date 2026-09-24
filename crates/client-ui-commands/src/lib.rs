//! Command UI contracts, fuzzy ranking, and popup filtering semantics.

mod contract;
mod directory;
mod locales;
mod popup;
mod ranking;
#[cfg(all(target_arch = "wasm32", feature = "browser"))]
mod wasm;

pub use contract::*;
pub use directory::*;
pub use locales::*;
pub use popup::*;
pub use ranking::*;
#[cfg(all(target_arch = "wasm32", feature = "browser"))]
pub use wasm::*;

/// Compiled popup-select stylesheet.
pub const POPUP_VIEW_STYLES: &str = include_str!("../data/popup-select.css");

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-commands";

/// Builds the no-op Host half of this pure Client UI plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
