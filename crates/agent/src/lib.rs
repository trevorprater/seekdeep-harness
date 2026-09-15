//! Live agents, durable inbox projection, lifecycle registry, and dispatch.

/// Durable accounting of work consumed by an agent log.
pub mod consumed_work;
/// Subject-fused scoped event dispatch.
pub mod dispatch;
/// Programmatic agent-creation factory contract.
pub mod factory;
/// Durable pending-message projection.
pub mod inbox;
/// Package-owned live Agent invariants.
pub mod invariant;
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
pub use invariant::{INVARIANT_INJECT, INVARIANT_NAME, register_invariant};
pub use model_selection::{
    ModelSelection, ModelSelectionInstallation, ModelSelectionRef, install_model_selection,
};
pub use registry::{
    AGENTS, AgentDetach, AgentFactoryRegistration, AgentLifecycleEvent, AgentRegistry,
    AgentRegistryError,
};
pub use runtime_types::{
    AGENT, Agent, AgentControlError, AgentController, AgentOptions, AgentStatus,
    AgentStatusChanged, CancelOptions, MaintenanceReservation, PreStepDecision, RequestErrorAction,
    SessionStartSource,
};
pub use seekdeep_core::session::AgentCancelCause;

/// Loader plugin identity.
pub const PLUGIN_NAME: &str = "agent";
/// Agent registry has no service prerequisites.
pub const PLUGIN_INJECT: &[&str] = &[];

/// Builds the Loader-compatible live Agent registry plugin.
#[must_use]
pub fn plugin() -> seekdeep_cordis::Plugin {
    seekdeep_cordis::Plugin::new(PLUGIN_NAME, PLUGIN_INJECT.iter().copied(), |context, _| {
        Box::pin(async move {
            let registry = std::sync::Arc::new(AgentRegistry::new(context.clone()));
            registry.provide(&context)?;
            let cleanup = registry.clone();
            context.own(seekdeep_cordis::fiber::EffectHandle::new(
                "agent registry initiators",
                move || {
                    let cleanup = cleanup.clone();
                    Box::pin(async move {
                        cleanup.dispose_initiators().await;
                        Ok(())
                    })
                },
            ))?;
            Ok(())
        })
    })
}
