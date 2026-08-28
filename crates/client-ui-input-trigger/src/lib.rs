//! Input-trigger contracts, detection, and candidate-menu state machine.

mod detect;
mod locales;
mod menu;
mod types;
#[cfg(target_arch = "wasm32")]
mod wasm;

pub use detect::*;
pub use locales::*;
pub use menu::*;
pub use types::*;
#[cfg(target_arch = "wasm32")]
pub use wasm::*;

/// Compiled trigger menu stylesheet.
pub const MENU_VIEW_STYLES: &str = include_str!("../data/menu-view.css");

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-input-trigger";

/// Builds the no-op Host half of this pure Client plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
