//! Command UI contracts, fuzzy ranking, and popup filtering semantics.

mod contract;
mod directory;
mod locales;
mod popup;
mod ranking;

pub use contract::*;
pub use directory::*;
pub use locales::*;
pub use popup::*;
pub use ranking::*;

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
