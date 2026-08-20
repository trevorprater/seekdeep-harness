//! Subagent capability seam: provider registry, one-shot runs, and
//! continuable-child operations.

pub mod activation_setup_registry;
pub mod assistant_output;
pub mod child_agent;
pub mod client;
pub mod depth;
pub mod descriptor;
pub mod descriptor_seed;
pub mod error;
pub mod index;
pub mod invariant;
pub mod lifecycle;
pub mod out_of_process;
pub mod projection;
pub mod projection_types;
pub mod run_settlement;
pub mod types;

pub use activation_setup_registry::{
    ContinuableSetupContribution, SubagentActivationSetupRegistry,
};
pub use assistant_output::{AssistantOutputFold, final_assistant_output};
pub use child_agent::{
    ChildComposition, ChildCreateInputs, DelegatedPolicyOverrides, SUBAGENT_DELEGATION_CONTEXT,
    SubagentDepthError, append_delegated_policy_overrides, apply_child_composition,
    capture_delegated_policy_overrides, child_session_meta, resolve_child_agent_options,
    resolve_child_depth,
};
pub use depth::{assert_subagent_max_depth, delegation_depth_of};
pub use descriptor::{
    SUBAGENT_DESCRIPTOR_VERSION, SubagentDescriptorData, SubagentDescriptorInput,
    fold_subagent_descriptor, snapshot_subagent_descriptor,
};
pub use descriptor_seed::seed_descriptor_turn;
pub use error::SubagentError;
pub use index::{INJECT, NAME, SUBAGENTS, SubagentRuntime, plugin};
pub use lifecycle::{emit_subagent_lifecycle, epoch_output, epoch_stop_reason, observe_run};
pub use out_of_process::{
    RunResultSettlement, SubprocessRunHandle, assert_positive_finite, assert_usable_cwd,
    no_start_capabilities, resolve_child_cwd, settle_run_result, validate_configured_cwd,
};
pub use projection::{
    subagent_identity_projection_definition, subagent_timing_projection_definition,
};
pub use projection_types::{SubagentIdentityProjection, SubagentTimingProjection};
pub use run_settlement::{JobOutcome, run_outcome, settle_run};
pub use types::{
    ContinuableCreateRequest, ContinuableCreateSpec, ResolvedSubagentStartRequest,
    SubagentCapabilities, SubagentProvider, SubagentResult, SubagentRun, SubagentRunEndInfo,
    SubagentRunId, SubagentRunInfo, SubagentStartRequest, SubagentStopReason,
};
