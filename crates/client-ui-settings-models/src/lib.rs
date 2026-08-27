//! Model-provider settings and first-run onboarding semantic core.

mod api_key;
mod models;
mod onboarding;
mod welcome;

pub use api_key::*;
pub use models::*;
pub use onboarding::*;
pub use welcome::*;

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-settings-models";

/// Builds the no-op Host half of this pure Client plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
