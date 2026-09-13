//! Host-only workflow request and live-run handles.

use std::sync::Arc;

use futures::future::BoxFuture;
use seekdeep_agent::Agent;
use seekdeep_llm::AbortSignal;

use crate::types::{WorkflowMeta, WorkflowResult, WorkflowRunId};

/// What a caller asks for when starting a workflow run.
#[derive(Clone, Debug)]
pub struct WorkflowStartRequest {
    /// The plain script body (top-level await allowed).
    pub script: String,
    /// The workflow's identity block.
    pub meta: WorkflowMeta,
    /// Optional input exposed verbatim as the args global.
    pub args: Option<serde_json::Value>,
    /// Optional engine-wide child-provider override.
    pub subagent_provider: Option<String>,
    /// Optional per-run total-child ceiling.
    pub max_total_agents: Option<u64>,
    /// The agent on whose behalf the run executes.
    pub parent: Arc<Agent>,
    /// Cancels the run when aborted.
    pub signal: Option<AbortSignal>,
}

/// Holder-owned live workflow run.
pub trait WorkflowRun: Send + Sync {
    /// The run's id.
    fn id(&self) -> &WorkflowRunId;
    /// The validated meta block.
    fn meta(&self) -> &WorkflowMeta;
    /// Resolves when the script settles; never rejects.
    fn result(&self) -> BoxFuture<'static, WorkflowResult>;
    /// Cancels the run and its children.
    fn cancel(&self, reason: Option<&str>);
    /// Cancels if needed and awaits bounded settlement and cleanup.
    fn dispose(&self) -> BoxFuture<'static, ()>;
}
