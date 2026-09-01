//! Model-facing foreground Ralph loop over the workflow and subagent seams.

pub mod index;
pub mod invariant;

pub use index::{Config, INJECT, NAME, apply};
pub use invariant::register_invariant;

/// Builds the Loader-compatible Ralph tool plugin.
#[must_use]
pub fn plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(NAME, INJECT.iter().copied(), |context, value| {
        Box::pin(async move {
            let config: Config = serde_json::from_value(value)?;
            apply(&context, &config)?;
            Ok(())
        })
    })
}
