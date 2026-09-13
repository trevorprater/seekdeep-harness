//! Non-protocol wire vocabulary for the worker-thread engine: the worker-data
//! init payload and the child-port interfaces the worker-side runtime consumes.

use std::sync::Arc;

use futures::future::BoxFuture;
use seekdeep_llm::ContentBlock;
use seekdeep_workflow::WorkflowMeta;
use serde::{Deserialize, Serialize};

/// The per-run limits the worker-side runtime enforces. The host keeps the
/// knobs only it can act on (provider, dispose grace).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerLimits {
    /// Concurrent agent-call ceiling (already auto-resolved; >= 1).
    pub max_concurrent_agents: u64,
    /// Total agent calls per run (the runaway-loop backstop).
    pub max_total_agents: u64,
    /// Items accepted by one parallel/pipeline call.
    pub max_items_per_call: u64,
    /// vm timeout for the script's initial synchronous slice (inside the worker).
    pub sync_timeout_ms: u64,
}

/// The worker-data payload one run is initialized with (host to worker, once, at spawn).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerInit {
    /// The validated meta block.
    pub meta: WorkflowMeta,
    /// The plain-JS script body, exactly as the start request carried it.
    pub body: String,
    /// The run's args value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
    /// The worker-enforced limits.
    pub limits: WorkerLimits,
}

/// What the worker asks the host to start for one agent call.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildStartRequest {
    /// The child's prompt text.
    pub prompt: String,
    /// The structured-output schema, if the call passed one (already subset-checked).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<serde_json::Value>,
    /// The per-child provider override, if the call passed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The per-child model override, if the call passed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// The JSON projection of a child's result crossing the port.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildResult {
    /// The child's final assistant output blocks.
    pub output: Vec<ContentBlock>,
    /// The structured value, present iff the request carried a schema AND the provider honored it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<serde_json::Value>,
    /// Why the child run ended (completed is the only value the runtime branches on).
    pub stop_reason: String,
}

/// The worker-side handle for one started child - the RPC mirror of the
/// subagent seam's run handle, reduced to what the runtime consumes.
pub trait ChildHandle: Send + Sync {
    /// The child agent's id.
    fn id(&self) -> &str;
    /// Resolves with the child's terminal result; rejects only when the host
    /// reports an infrastructure fault.
    fn result(&self) -> BoxFuture<'static, anyhow::Result<ChildResult>>;
    /// Ask the host to dispose the child; resolves on the host's ack.
    fn dispose(&self) -> BoxFuture<'static, anyhow::Result<()>>;
}

/// The worker-side port the runtime starts child agents through.
pub trait ChildPort: Send + Sync {
    /// Start one child agent on the host.
    fn start_agent(
        &self,
        request: ChildStartRequest,
    ) -> BoxFuture<'static, anyhow::Result<Arc<dyn ChildHandle>>>;
}
