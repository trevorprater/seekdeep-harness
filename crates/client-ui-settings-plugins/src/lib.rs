//! Configurable-plugin Settings cards, staged forms, and browser assembly.

mod controllers;
mod form;

#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
mod browser_apply;
#[cfg(target_arch = "wasm32")]
mod browser_components;
#[cfg(target_arch = "wasm32")]
mod browser_controllers;

pub use controllers::*;
pub use form::*;

#[cfg(target_arch = "wasm32")]
pub use browser_apply::*;
#[cfg(target_arch = "wasm32")]
pub use browser_components::*;
#[cfg(target_arch = "wasm32")]
pub use browser_controllers::{
    create_plugins_agent_loop_controller, create_plugins_bash_controller,
    create_plugins_web_search_controller,
};

/// Stable Host plugin identity.
pub const NAME: &str = "client-ui-settings-plugins";

/// Builds the no-op Host half of this pure Client plugin.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn host_plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, std::iter::empty::<String>(), |_, _| {
        Box::pin(async { Ok(()) })
    })
}
