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

pub use controller::{
    AgentInboxClaimed, AgentInboxMessage, AgentStatusChanged, DriverTask, LoopAgent, LoopController,
};
pub use driver::{
    AgentErrorEvent, AgentLoopServices, AgentPreStepEvent, AgentRequestErrorEvent,
    AgentRequestEvent, AgentTurnStoppingEvent, DefaultAgentDriver,
};
pub use factory::{
    AgentHandle, AgentLoop, AgentSetup, AgentSetupCommit, CreateAgentMeta, CreateAgentOptions,
    ResumeAgentOptions, SessionStartEvent,
};
pub use invariant::{install_request_invariant, validate_agent_loop_request};
pub use runtime_context::RuntimeContextProjection;
pub use tool_calls::{ToolCall, ToolCallBatch, ToolCallBatchOutcome, execute_tool_calls};
