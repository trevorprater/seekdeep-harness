//! Deterministic agent driver, runtime-context projection, and tool scheduling.

/// Default maximum number of parallel-safe calls dispatched per agent step.
pub const DEFAULT_MAX_PARALLEL_TOOL_CALLS: usize = 10;

/// Durable projection of dynamic runtime context.
pub mod controller;
/// Durable turn/step machine and request reconstruction.
pub mod driver;
/// Rollback-covered agent/session factory publication.
pub mod factory;
/// Dispatch-time reconstruction guard for loop-built model calls.
pub mod invariant;
/// Durable projection of dynamic runtime context.
pub mod runtime_context;
/// Ordered, bounded scheduling for one assistant step's tool calls.
pub mod tool_calls;

pub use controller::{AgentInboxClaimed, AgentInboxMessage, DriverTask, LoopAgent, LoopController};
pub use driver::{
    AgentErrorEvent, AgentLoopServices, AgentPreStepEvent, AgentRequestErrorEvent,
    AgentRequestEvent, AgentTurnStoppingEvent, DefaultAgentDriver,
};
pub use factory::{AgentLoop, SessionStartEvent};
pub use invariant::{install_request_invariant, validate_agent_loop_request};
pub use runtime_context::RuntimeContextProjection;
pub use seekdeep_agent::AgentStatusChanged;
pub use seekdeep_agent::factory::{
    AgentFactory, AgentHandle, AgentSetup, AgentSetupCommit, CreateAgentMeta, CreateAgentOptions,
    ResumeAgentOptions,
};
pub use tool_calls::{ToolCall, ToolCallBatch, ToolCallBatchOutcome, execute_tool_calls};
