//! Model-provider settings and first-run onboarding semantic core.

mod api_key;
mod editor;
mod models;
mod onboarding;
mod welcome;

#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
mod browser_apply;
#[cfg(target_arch = "wasm32")]
mod browser_components;
#[cfg(target_arch = "wasm32")]
mod browser_store;

pub use api_key::*;
pub use editor::*;
pub use models::*;
pub use onboarding::*;
pub use welcome::*;

#[cfg(target_arch = "wasm32")]
pub use browser_apply::*;
#[cfg(target_arch = "wasm32")]
pub use browser_components::*;
#[cfg(target_arch = "wasm32")]
pub use browser_store::*;

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
