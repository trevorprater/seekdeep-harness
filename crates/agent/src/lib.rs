//! Live agents, durable inbox projection, lifecycle registry, and dispatch.

/// Durable accounting of work consumed by an agent log.
pub mod consumed_work;
/// Subject-fused scoped event dispatch.
pub mod dispatch;
/// Programmatic agent-creation factory contract.
pub mod factory;
/// Durable pending-message projection.
pub mod inbox;
/// Coupled prompt-variable and request-route model selection.
pub mod model_selection;
/// Two-phase live-agent registry and paired lifecycle events.
pub mod registry;
/// Public live-agent state and construction surface.
pub mod runtime_types;

pub use consumed_work::{ConsumedWork, fold_consumed_work};
pub use dispatch::{AgentEvent, AgentEvents, assemble_context_for};
pub use factory::{
    AgentFactory, AgentHandle, AgentSetup, AgentSetupCommit, CreateAgentMeta, CreateAgentOptions,
    ResumeAgentOptions,
};
pub use inbox::{Inbox, InboxError, InboxNotifications, InboxTarget, NoopInboxNotifications};
pub use model_selection::{
    ModelSelection, ModelSelectionInstallation, ModelSelectionRef, install_model_selection,
};
pub use registry::{AGENTS, AgentDetach, AgentLifecycleEvent, AgentRegistry, AgentRegistryError};
pub use runtime_types::{
    AGENT, Agent, AgentControlError, AgentController, AgentOptions, AgentStatus, CancelOptions,
    MaintenanceReservation, PreStepDecision, RequestErrorAction, SessionStartSource,
};
pub use seekdeep_core::session::AgentCancelCause;
