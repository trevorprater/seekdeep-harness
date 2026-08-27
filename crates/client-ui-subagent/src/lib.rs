//! Subagent catalog and read-only composer semantics.

mod locales;
mod model;

pub use locales::*;
pub use model::*;

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
