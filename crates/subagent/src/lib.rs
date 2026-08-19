//! Subagent capability seam: provider registry, one-shot runs, and
//! continuable-child operations.

pub mod assistant_output;
pub mod client;
pub mod depth;
pub mod descriptor;
pub mod error;
pub mod projection_types;
pub mod types;

pub use assistant_output::{AssistantOutputFold, final_assistant_output};
pub use depth::{assert_subagent_max_depth, delegation_depth_of};
pub use descriptor::{
    SUBAGENT_DESCRIPTOR_VERSION, SubagentDescriptorData, SubagentDescriptorInput,
    fold_subagent_descriptor, snapshot_subagent_descriptor,
};
pub use error::SubagentError;
pub use projection_types::{SubagentIdentityProjection, SubagentTimingProjection};
pub use types::{
    ContinuableCreateRequest, ContinuableCreateSpec, ResolvedSubagentStartRequest,
    SubagentCapabilities, SubagentProvider, SubagentResult, SubagentRun, SubagentRunEndInfo,
    SubagentRunId, SubagentRunInfo, SubagentStartRequest, SubagentStopReason,
};
