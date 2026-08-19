//! Model-facing workflow tool: orchestration script runner.

pub mod index;
pub mod invariant;
pub mod types;

pub use index::{Config, INJECT, NAME, apply};
pub use types::{
    ToolWorkflowAgentEndData, ToolWorkflowAgentStartData, ToolWorkflowRunEndData,
    ToolWorkflowRunStartData,
};
