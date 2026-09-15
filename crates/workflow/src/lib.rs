//! Workflow orchestration seam: engine contract, vocabulary, and invariants.

pub mod index;
pub mod invariant;
pub mod runtime_types;
pub mod types;

pub use index::{
    WORKFLOW_ENGINE, WorkflowEngine, WorkflowEngineService, WorkflowError, WorkflowErrorCode,
    WorkflowEventName, emit_workflow_event, is_fatal_workflow_error,
};
pub use runtime_types::{WorkflowRun, WorkflowStartRequest};
pub use types::{
    WorkflowAgentEndInfo, WorkflowAgentInfo, WorkflowAgentOutcome, WorkflowMeta, WorkflowPhase,
    WorkflowResult, WorkflowResultInfo, WorkflowRunId, WorkflowRunInfo, WorkflowStopReason,
};

/// Cordis plugin name.
pub const NAME: &str = "workflow";
