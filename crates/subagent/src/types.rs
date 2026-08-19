//! The seam's consumer-facing contracts.

use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use seekdeep_agent::{Agent, AgentOptions};
use seekdeep_core::session::{SessionEvent, SessionId};
use seekdeep_llm::{AbortSignal, ContentBlock};
use seekdeep_tools::ToolRestriction;
use serde::{Deserialize, Serialize};

use crate::descriptor::SubagentDescriptorData;

seekdeep_util::string_brand!(
    /// Identifies one accepted subagent run across its lifecycle event pair.
    pub struct SubagentRunId;
);

/// Observe-only identifying detail for a published subagent run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentRunInfo {
    /// Unique identity shared with the paired terminal event.
    pub run_id: SubagentRunId,
    /// Provider name recorded when the child was first created.
    pub provider: String,
    /// The child agent's id.
    pub id: SessionId,
    /// Snapshot of whether localAgent was present when start fulfilled.
    pub local: bool,
}

/// Observe-only outcome detail for a settled subagent run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentRunEndInfo {
    /// Unique identity shared with the paired start event.
    pub run_id: SubagentRunId,
    /// The same provider name carried by the paired start event.
    pub provider: String,
    /// The child agent's id.
    pub id: SessionId,
    /// Snapshot of whether localAgent was present when start fulfilled.
    pub local: bool,
    /// The terminal stop reason.
    pub stop_reason: SubagentStopReason,
    /// The child's final assistant output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_assistant_message: Option<Vec<ContentBlock>>,
}

/// Which start-time features a provider supports.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentCapabilities {
    /// Structured-output support.
    pub output_schema: bool,
    /// Depth-limit support.
    pub depth_limit: bool,
    /// Tool-filter support.
    pub tool_filter: bool,
    /// Persona support.
    pub persona: bool,
}

/// What a caller asks for when starting a one-shot subagent.
#[derive(Clone, Debug)]
pub struct SubagentStartRequest {
    /// Optional short display label.
    pub label: Option<String>,
    /// Content delivered as the child's user message.
    pub prompt: Vec<ContentBlock>,
    /// The spawning agent.
    pub parent: Arc<Agent>,
    /// Cancellation signal from the spawning context.
    pub signal: AbortSignal,
    /// Optional per-child agent options.
    pub agent_options: Option<AgentOptions>,
    /// Optional structured-output schema.
    pub output_schema: Option<serde_json::Value>,
    /// Optional absolute delegation-depth cap.
    pub max_depth: Option<u64>,
    /// Optional child tool scoping.
    pub tool_filter: Option<ToolRestriction>,
    /// Optional per-child persona.
    pub persona: Option<String>,
}

/// Provider-facing one-shot request after the runtime resolves the descriptor.
#[derive(Clone, Debug)]
pub struct ResolvedSubagentStartRequest {
    /// The base request.
    pub request: SubagentStartRequest,
    /// Detached descriptor a session-backed provider persists.
    pub descriptor: SubagentDescriptorData,
}

/// What the continuation manager asks a provider for.
#[derive(Clone, Debug)]
pub struct ContinuableCreateRequest {
    /// The reserved durable child session id.
    pub session_id: SessionId,
    /// The delegating parent agent.
    pub parent: Arc<Agent>,
    /// Caller cancellation.
    pub signal: AbortSignal,
}

/// A provider's detached contribution to one continuable child's creation.
#[derive(Clone, Debug, Default)]
pub struct ContinuableCreateSpec {
    /// Completed-turn prefix of the parent's log to seed the child with.
    pub seed: Option<Vec<SessionEvent>>,
}

/// Why a subagent run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubagentStopReason {
    /// The child finished its turn normally.
    Completed,
    /// Cancelled through the request signal or disposal.
    Aborted,
    /// Model or transport failure.
    Error,
    /// The child hit its token ceiling.
    #[serde(rename = "max-tokens")]
    MaxTokens,
    /// The child declined the task.
    Refusal,
}

/// The terminal outcome of a subagent run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentResult {
    /// The child's final assistant output.
    pub output: Vec<ContentBlock>,
    /// The structured result, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<serde_json::Value>,
    /// Why the run ended.
    pub stop_reason: SubagentStopReason,
}

/// One-shot child handle returned after publication.
#[async_trait]
pub trait SubagentRun: Send + Sync {
    /// Parent-scoped run id.
    fn id(&self) -> &SessionId;
    /// The exact published in-process child, or none for a remote run.
    fn local_agent(&self) -> Option<&Arc<Agent>>;
    /// Resolves with the child's terminal result.
    fn result(&self) -> BoxFuture<'static, SubagentResult>;
    /// Cancels remaining work, reaches quiescence, and releases resources.
    fn dispose(&self) -> BoxFuture<'static, ()>;
}

/// One registered transport for running child agents.
#[async_trait]
pub trait SubagentProvider: Send + Sync {
    /// Unique registry name.
    fn name(&self) -> &str;
    /// The start-time features this provider supports.
    fn capabilities(&self) -> &SubagentCapabilities;
    /// Whether the child sees the parent's completed-turn prefix.
    fn inherits_parent_context(&self) -> bool;

    /// Establishes a one-shot child and returns its handle after publication.
    ///
    /// # Errors
    ///
    /// Returns the provider's setup or publication failure.
    async fn start(
        &self,
        request: ResolvedSubagentStartRequest,
    ) -> anyhow::Result<Arc<dyn SubagentRun>>;

    /// Contributes the detached creation inputs for a continuable child.
    ///
    /// # Errors
    ///
    /// Returns unsupported-capability by default; providers that support
    /// continuable children override this.
    async fn prepare_continuable(
        &self,
        _request: ContinuableCreateRequest,
    ) -> anyhow::Result<ContinuableCreateSpec> {
        Err(crate::error::SubagentError::new(
            format!(
                "subagent provider \"{}\" does not support continuable children (no prepareContinuable capability)",
                self.name()
            ),
            "UNSUPPORTED_CAPABILITY",
        )
        .into())
    }
}
