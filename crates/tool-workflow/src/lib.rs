//! Model-facing workflow tool: orchestration script runner.

pub mod index;
pub mod invariant;
pub mod types;

pub use index::{Config, INJECT, NAME, apply};
pub use types::{
    ToolWorkflowAgentEndData, ToolWorkflowAgentStartData, ToolWorkflowRunEndData,
    ToolWorkflowRunStartData,
};

/// Builds the Loader-compatible workflow tool plugin.
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
