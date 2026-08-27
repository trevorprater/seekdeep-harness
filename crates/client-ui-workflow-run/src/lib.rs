//! Durable workflow-run projection and browser UI semantics.

mod locales;
mod panel;
mod projection;

pub use locales::*;
pub use panel::*;
pub use projection::*;

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-workflow-run";

/// Builds the dependency-free Host half of this Client plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
