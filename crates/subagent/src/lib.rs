//! Subagent capability seam: provider registry, one-shot runs, and
//! continuable-child operations.

pub mod assistant_output;
pub mod client;
pub mod depth;
pub mod error;
pub mod projection_types;

pub use assistant_output::{AssistantOutputFold, final_assistant_output};
pub use depth::{assert_subagent_max_depth, delegation_depth_of};
pub use error::SubagentError;
pub use projection_types::{SubagentIdentityProjection, SubagentTimingProjection};
